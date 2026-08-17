// Package sandbox validates the Input's sandbox grants — a private /tmp, HTTP
// egress rules, environment variables — as designed in
// docs/one-pager-sandbox.md, and checks them against the ceiling the operator
// set with the --enable-sandbox-* flags. Validate checks the shape of a
// sandbox; a Ceiling turns it into what one run gets (Grant) or refuses it.
// The mechanics live in internal/engine; HTTP egress has its own ceiling in
// internal/egress. Host directories are deliberately not mountable into a
// module — the request is a module's only view of the world beyond what it
// may write for itself — so the filesystem grant is the private /tmp alone.
package sandbox

import (
	"errors"
	"fmt"
	"maps"
	"os"
	"regexp"
	"strings"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
)

var (
	envKeyPattern = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)
	// methods an egress rule may list — the CRD's enum, enforced here as
	// well: Crossplane never installs the Input CRD, so nothing but the
	// runtime checks a Composition's Input.
	methods = map[string]bool{"GET": true, "HEAD": true, "POST": true, "PUT": true, "PATCH": true, "DELETE": true, "OPTIONS": true}
)

// Validate checks the shape of a sandbox: each HTTP rule names exactly one
// of host and hostPattern (a leading wildcard label), lists at least one
// known method and an absolute, normalized path prefix, and env keys are
// identifiers with NUL-free values. A nil sandbox is valid. Every rule the
// CRD markers express is enforced here too — Crossplane does not install the
// Input CRD, so its schema only serves tooling.
func Validate(s *v1beta1.Sandbox) error {
	if s == nil {
		return nil
	}
	if s.Egress != nil {
		if err := ValidateRules("sandbox.egress.http", s.Egress.HTTP); err != nil {
			return err
		}
	}
	for k, v := range s.Env {
		if !ValidEnvKey(k) {
			return fmt.Errorf("sandbox.env key %q is not an identifier ([A-Za-z_][A-Za-z0-9_]*)", k)
		}
		// WASI hands the environment over as NUL-terminated strings; a NUL
		// inside a value would truncate it silently.
		if strings.IndexByte(v, 0) >= 0 {
			return fmt.Errorf("sandbox.env %s: the value must not contain a NUL byte", k)
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

// RequestsEgress reports whether s asks for any HTTP egress rule — the one
// grant whose ceiling lives outside this package (internal/egress).
func RequestsEgress(s *v1beta1.Sandbox) bool {
	return s != nil && s.Egress != nil && len(s.Egress.HTTP) > 0
}

// PrivateTmpPath is where a guest sees its private /tmp.
const PrivateTmpPath = engine.PrivateTmpGuestPath

// Options are the operator's sandbox flags — the ceiling.
type Options struct {
	// EnablePrivateTmp lets Compositions ask for a private /tmp
	// (--enable-sandbox-private-tmp).
	EnablePrivateTmp bool
	// EnableEnv lets Compositions set environment variables
	// (--enable-sandbox-env).
	EnableEnv bool
}

// Ceiling is what the operator allows Compositions to grant their modules.
// The zero value allows nothing — the default sandbox.
type Ceiling struct {
	enablePrivateTmp bool
	enableEnv        bool
}

// NewCeiling checks the operator's flags once, at startup, so a misconfigured
// runtime stops instead of failing every request: a private /tmp is probed
// under os.TempDir() to make sure one can be created there.
func NewCeiling(o Options) (*Ceiling, error) {
	c := &Ceiling{enablePrivateTmp: o.EnablePrivateTmp, enableEnv: o.EnableEnv}
	if o.EnablePrivateTmp {
		dir, err := os.MkdirTemp("", engine.PrivateTmpPrefix)
		if err != nil {
			return nil, fmt.Errorf("--enable-sandbox-private-tmp: cannot create a private /tmp under %s (set TMPDIR to a writable directory): %w", os.TempDir(), err)
		}
		if err := os.Remove(dir); err != nil {
			return nil, fmt.Errorf("--enable-sandbox-private-tmp: cannot remove a private /tmp under %s: %w", os.TempDir(), err)
		}
	}
	return c, nil
}

// Grant is what one run gets: the sandbox its Composition asked for, within
// the ceiling. The zero value is the default sandbox.
type Grant struct {
	// PrivateTmp asks for a fresh, writable /tmp for the run.
	PrivateTmp bool
	// Env are the guest's environment variables.
	Env map[string]string
}

// Grant checks s (already validated for shape) against the ceiling and
// returns what the run gets. A grant outside the ceiling — a private /tmp or
// environment the operator did not enable — is an error naming the grant and
// the flag, so either author can act on it; egress is not this method's
// business. A nil Ceiling allows nothing, like the zero value.
func (c *Ceiling) Grant(s *v1beta1.Sandbox) (Grant, error) {
	var g Grant
	if s == nil {
		return g, nil
	}
	if c == nil {
		c = &Ceiling{}
	}
	if s.Filesystem != nil && s.Filesystem.PrivateTmp {
		if !c.enablePrivateTmp {
			return Grant{}, errors.New("sandbox.filesystem.privateTmp is refused: the runtime was started without --enable-sandbox-private-tmp")
		}
		g.PrivateTmp = true
	}
	if len(s.Env) > 0 {
		if !c.enableEnv {
			return Grant{}, errors.New("sandbox.env is refused: the runtime was started without --enable-sandbox-env")
		}
		g.Env = maps.Clone(s.Env)
	}
	return g, nil
}
