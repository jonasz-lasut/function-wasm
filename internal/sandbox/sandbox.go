// Package sandbox validates the Input's sandbox grants - a private /tmp, HTTP
// egress rules, environment variables - as designed in
// docs/one-pager-sandbox.md and docs/one-pager-request-secrets.md. Whether a
// capability is enabled is the operator's Cedar --sandbox-policy-file's decision,
// made in internal/admission; this package checks a sandbox's shape (Validate),
// turns it into what one run gets (Grant), probes $TMPDIR once at startup so a
// private /tmp a policy can grant is known to be creatable (Ceiling), and
// resolves valueFrom references against the request's credentials (Materialize).
// The mechanics live in internal/engine; HTTP egress has its own ceiling in
// internal/egress. Host directories are deliberately not mountable into a module
// - the request is a module's only view of the world beyond what it may write
// for itself - so the filesystem grant is the private /tmp alone.
package sandbox

import (
	"fmt"
	"os"
	"regexp"
	"strings"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
)

// privateTmpProbePrefix names the throwaway directory NewCeiling creates and
// removes to prove $TMPDIR is writable. It only has to be a valid MkdirTemp
// prefix; the engine names the real per-run directories itself, so this
// validation package need not depend on the CGo engine to reproduce that name.
const privateTmpProbePrefix = "function-wasm-sandbox-probe-"

var (
	envKeyPattern = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)
	// methods an egress rule may list - the CRD's enum, enforced here as
	// well: Crossplane never installs the Input CRD, so nothing but the
	// runtime checks a Composition's Input.
	methods = map[string]bool{"GET": true, "HEAD": true, "POST": true, "PUT": true, "PATCH": true, "DELETE": true, "OPTIONS": true}
)

// Validate checks the shape of a sandbox: each HTTP rule names exactly one
// of host and hostPattern (a leading wildcard label), lists at least one
// known method and an absolute, normalized path prefix; each env entry names
// exactly one of value and valueFrom with an identifier name; envFrom entries
// name a credential; and no two env entries share a name. A nil sandbox is
// valid. Every rule the CRD markers express is enforced here too - Crossplane
// does not install the Input CRD, so its schema only serves tooling.
func Validate(s *v1beta1.Sandbox) error {
	if s == nil {
		return nil
	}
	if s.Egress != nil {
		if err := ValidateRules("sandbox.egress.http", s.Egress.HTTP); err != nil {
			return err
		}
	}
	// Env: each entry is {name, value | valueFrom}.
	seen := make(map[string]string, len(s.Env)) // name -> "sandbox.env[i]"
	for i, e := range s.Env {
		field := fmt.Sprintf("sandbox.env[%d]", i)
		if !ValidEnvKey(e.Name) {
			return fmt.Errorf("%s.name %q is not an identifier ([A-Za-z_][A-Za-z0-9_]*)", field, e.Name)
		}
		if (e.Value == nil) == (e.ValueFrom == nil) {
			return fmt.Errorf("%s: exactly one of value and valueFrom must be set", field)
		}
		if e.ValueFrom != nil {
			if e.ValueFrom.Credential == nil {
				return fmt.Errorf("%s.valueFrom: exactly one source must be set (credential)", field)
			}
			if e.ValueFrom.Credential.Name == "" {
				return fmt.Errorf("%s.valueFrom.credential.name must not be empty", field)
			}
			if e.ValueFrom.Credential.Key == "" {
				return fmt.Errorf("%s.valueFrom.credential.key must not be empty", field)
			}
		}
		if e.Value != nil && strings.IndexByte(*e.Value, 0) >= 0 {
			return fmt.Errorf("%s: the value of %s must not contain a NUL byte", field, e.Name)
		}
		if prev, ok := seen[e.Name]; ok {
			return fmt.Errorf("%s: %s is already set by %s", field, e.Name, prev)
		}
		seen[e.Name] = field
	}
	// EnvFrom: each entry names a credential.
	for i, ef := range s.EnvFrom {
		field := fmt.Sprintf("sandbox.envFrom[%d]", i)
		if ef.Credential == nil {
			return fmt.Errorf("%s: exactly one source must be set (credential)", field)
		}
		if ef.Credential.Name == "" {
			return fmt.Errorf("%s.credential.name must not be empty", field)
		}
		if ef.Prefix != "" && !ValidEnvKey(ef.Prefix) {
			return fmt.Errorf("%s.prefix %q is not a valid identifier prefix ([A-Za-z_][A-Za-z0-9_]*)", field, ef.Prefix)
		}
	}
	return nil
}

// ValidateRules checks the shape of HTTP egress rules - a Composition's
// sandbox.egress.http, or a module manifest's requires.egress.http, which
// share the type - naming a wrong rule as field[i]: exactly one of host and
// hostPattern, at least one known method, an absolute normalized pathPrefix.
func ValidateRules(field string, rules []v1beta1.SandboxHTTPRule) error {
	for i, r := range rules {
		if (r.Host == "") == (r.HostPattern == "") {
			return fmt.Errorf("%s[%d] must set exactly one of host and hostPattern", field, i)
		}
		if r.Host != "" && !egress.ValidHost(r.Host) {
			return fmt.Errorf("%s[%d].host %q must be a bare host name (no scheme, port, path or zone)", field, i, r.Host)
		}
		if r.HostPattern != "" && !egress.ValidHostPattern(r.HostPattern) {
			return fmt.Errorf("%s[%d].hostPattern %q must be a host name with one leading wildcard label, e.g. *.example.com", field, i, r.HostPattern)
		}
		if len(r.Methods) == 0 {
			return fmt.Errorf("%s[%d].methods must list at least one method", field, i)
		}
		for _, m := range r.Methods {
			if !methods[m] {
				return fmt.Errorf("%s[%d].methods: %q is not one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS", field, i, m)
			}
		}
		if r.PathPrefix != "" && r.PathPrefix[0] != '/' {
			return fmt.Errorf("%s[%d].pathPrefix %q must start with /", field, i, r.PathPrefix)
		}
		if !egress.NormalizedPath(r.PathPrefix) {
			return fmt.Errorf("%s[%d].pathPrefix %q must be normalized (no . or .. segments, no empty segments)", field, i, r.PathPrefix)
		}
	}
	return nil
}

// ValidEnvKey reports whether key is an environment variable name a guest
// can be given: an identifier, [A-Za-z_][A-Za-z0-9_]*.
func ValidEnvKey(key string) bool {
	return envKeyPattern.MatchString(key)
}

// RequestsEgress reports whether s asks for any HTTP egress rule - the one
// grant whose ceiling lives outside this package (internal/egress).
func RequestsEgress(s *v1beta1.Sandbox) bool {
	return s != nil && s.Egress != nil && len(s.Egress.HTTP) > 0
}

// RequestsEnv reports whether s asks for any environment variable, either
// through env[] or envFrom[].
func RequestsEnv(s *v1beta1.Sandbox) bool {
	return s != nil && (len(s.Env) > 0 || len(s.EnvFrom) > 0)
}

// Options configure the sandbox startup checks for the capabilities the
// operator's Cedar policy can grant.
type Options struct {
	// ProbePrivateTmp asks NewCeiling to prove a private /tmp can be created
	// under $TMPDIR. Set it when the operator policy has a usePrivateTmp rule
	// (OperatorPolicy.HasPrivateTmpRules), so a misconfigured $TMPDIR stops the
	// runtime at startup rather than failing the first request that asks for one.
	ProbePrivateTmp bool
}

// Ceiling marks the sandbox startup checks as passed. Enablement is the
// operator's Cedar --sandbox-policy-file's decision (internal/admission), not a
// flag, so the ceiling carries no capability state; it exists so the $TMPDIR
// probe runs once at startup and Grant has a home. A nil *Ceiling is safe: it
// grants exactly what the sandbox shape asks and the policy permits.
type Ceiling struct{}

// NewCeiling runs the operator's sandbox startup checks once so a misconfigured
// runtime stops instead of failing every request: when a private /tmp can be
// granted (o.ProbePrivateTmp), one is probed under os.TempDir() to make sure it
// can be created there.
func NewCeiling(o Options) (*Ceiling, error) {
	if o.ProbePrivateTmp {
		dir, err := os.MkdirTemp("", privateTmpProbePrefix)
		if err != nil {
			return nil, fmt.Errorf("the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot create one under %s (set TMPDIR to a writable directory): %w", os.TempDir(), err)
		}
		if err := os.Remove(dir); err != nil {
			return nil, fmt.Errorf("the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot remove one under %s: %w", os.TempDir(), err)
		}
	}
	return &Ceiling{}, nil
}

// Grant is what one run gets: the sandbox its Composition asked for, within
// the ceiling. The zero value is the default sandbox.
type Grant struct {
	// PrivateTmp asks for a fresh, writable /tmp for the run.
	PrivateTmp bool
	// Env are the guest's resolved environment variables: literals from the
	// Input and values read from step credentials by Materialize.
	Env map[string]string
}

// Grant turns s (already validated for shape) into the run's sandbox grant.
// Whether the run may actually use it is the operator policy's decision, made in
// internal/admission; this only reads the shape. The returned Grant carries
// PrivateTmp; Env is populated by Materialize after the pull credential is known.
func (*Ceiling) Grant(s *v1beta1.Sandbox) Grant {
	var g Grant
	if s == nil {
		return g
	}
	if s.Filesystem != nil && s.Filesystem.PrivateTmp {
		g.PrivateTmp = true
	}
	return g
}
