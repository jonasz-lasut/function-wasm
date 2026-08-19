// Package admission is the runtime's gate over a Composition's Input — the
// checks RunFunction runs before it resolves, fetches or compiles anything:
// the sandbox's shape and its grant within the operator's ceiling, HTTP
// egress rules within the egress policy, limits within the engine's ceilings,
// and the module source's and policy's shape. Crossplane never installs a
// function's Input CRD, so this is the only place a Composition's Input is
// ever judged; function validate runs the same function over Compositions
// offline, so a refusal reads the same on a laptop, in CI and in an XR
// condition (docs/one-pager-admission-tooling.md).
package admission

import (
	"errors"
	"fmt"
	"strings"

	"k8s.io/apimachinery/pkg/api/resource"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/authz"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

// Ceilings are the operator's: what the runtime was started with.
type Ceilings struct {
	// Engine bounds a run — Timeout (--module-timeout) and MemoryLimit
	// (--module-memory-limit), with defaults applied (engine.Config() or
	// Config.WithDefaults()).
	Engine engine.Config
	// Sandbox marks the sandbox startup checks as passed (the $TMPDIR probe);
	// it carries no capability state, since enablement is Policy's decision. A
	// nil *sandbox.Ceiling is safe.
	Sandbox *sandbox.Ceiling
	// Egress is the HTTP egress mechanism: the default SSRF block list, the
	// fixed per-run budgets, the operator's Cedar CIDR rules and the rate
	// limit. It is always built; whether a run may use it is Policy's grantEgress
	// decision. A nil *egress.Egress refuses every sandbox.egress grant.
	Egress *egress.Egress
	// Policy is the operator's grant policy (--sandbox-policy-file), compiled once
	// at startup, and the sole authority that enables a sandbox capability. Nil
	// when no --sandbox-policy-file is set, in which case every sandbox grant is
	// refused and a runtime offers only the default sandbox. It evaluates
	// default-deny and is AND-combined with the built-in fences.
	Policy *authz.OperatorPolicy
}

// Admitted is what one run gets when its Input is admitted.
type Admitted struct {
	// Options are the run's budget within the ceilings and its filesystem
	// and environment grant, ready for engine.Run. HTTP is left nil: the
	// per-run client is the caller's to make from HTTP.
	Options engine.RunOptions
	// Grant is the sandbox grant Options carries, on its own.
	Grant sandbox.Grant
	// HTTP is the egress grant — the Composition's rules within the ceiling —
	// or nil when the Input asks for none.
	HTTP *egress.Grant
	// Concurrency is the per-step concurrency limit (limits.concurrency),
	// zero when unset. Keyed by the module's digest, taken before the
	// global run slot.
	Concurrency int
}

// ManifestGrants is what a module's manifest is held against between load and
// run: the sandbox grants admitted for the step, in the shape the manifest
// check reads. The HTTP rules are the Composition's own (an egress grant is
// their intersection with the ceiling, so the rules never widen the check),
// carried only when the step was admitted egress. Both RunFunction and
// function validate build it from the same admission result so the manifest
// check reads the same on either path.
func (a Admitted) ManifestGrants(in *v1beta1.Input) manifest.Grants {
	grants := manifest.Grants{PrivateTmp: a.Grant.PrivateTmp}
	if a.HTTP != nil && in.Sandbox != nil && in.Sandbox.Egress != nil {
		grants.HTTP = in.Sandbox.Egress.HTTP
	}
	return grants
}

// Admit judges in against c in the order RunFunction does — sandbox shape,
// filesystem and environment grant, egress grant, limits, module and policy
// shape — and returns the first refusal in the runtime's words: it is the
// fatal result as is. The module source is only checked for shape here; a
// module.from source is materialised against the composite resource by
// module.FromComposite afterwards.
//
// principal describes the caller (the observed composite resource's namespace
// and kind, the Composition's name) for the operator's grant policy (c.Policy),
// the sole authority that enables a sandbox capability. Every sandbox grant is
// refused unless a policy permit matches: with no --sandbox-policy-file c.Policy
// is nil and a runtime offers only the default sandbox.
func Admit(in *v1beta1.Input, c Ceilings, principal authz.Principal) (Admitted, error) {
	var out Admitted
	if err := sandbox.Validate(in.Sandbox); err != nil {
		return out, err
	}
	// Filesystem (the private /tmp) and environment: what the Composition asks
	// for, enabled only where the operator policy permits it. A nil policy (no
	// --sandbox-policy-file) permits nothing, so the grant is refused.
	grant := c.Sandbox.Grant(in.Sandbox)
	if grant.PrivateTmp && !c.Policy.PermitsPrivateTmp(principal) {
		return out, policyRefusal("sandbox.filesystem.privateTmp", c.Policy)
	}
	if sandbox.RequestsEnv(in.Sandbox) && !c.Policy.PermitsEnv(principal, envKeys(in.Sandbox)) {
		return out, policyRefusal(envField(in.Sandbox), c.Policy)
	}
	// The Composition's HTTP rules are enabled only where the operator policy's
	// grantEgress permits them; this run's grant is the rules compiled against
	// the egress mechanism. A nil policy permits none.
	if sandbox.RequestsEgress(in.Sandbox) {
		if err := admitEgressPolicy(c.Policy, principal, in.Sandbox.Egress.HTTP); err != nil {
			return out, err
		}
		if c.Egress == nil {
			return out, errors.New("sandbox.egress is refused: the runtime has no egress mechanism")
		}
		var err error
		if out.HTTP, err = c.Egress.Grant(in.Sandbox.Egress.HTTP); err != nil {
			return out, err
		}
	}
	opts, err := runOptions(in.Limits, c.Engine)
	if err != nil {
		return out, err
	}
	opts.PrivateTmp = grant.PrivateTmp
	// Env is populated by sandbox.Materialize after the pull credential is
	// known, not here: valueFrom references need the request's credentials
	// and the withheld pull credential name, neither of which Admit has.
	out.Options, out.Grant = opts, grant
	if in.Limits != nil && in.Limits.Concurrency != nil {
		n := int(*in.Limits.Concurrency)
		if n <= 0 {
			return out, fmt.Errorf("limits.concurrency %d must be positive", n)
		}
		// Silently cap to the global bound when set: a per-step limit
		// above the global one would never help, but refusing it would
		// force every Composition to know the operator's flag.
		if c.Engine.MaxConcurrentRuns > 0 && n > c.Engine.MaxConcurrentRuns {
			n = c.Engine.MaxConcurrentRuns
		}
		out.Concurrency = n
	}
	if err := module.Validate(in.Module); err != nil {
		return out, fmt.Errorf("cannot resolve module: %w", err)
	}
	if err := module.ValidatePolicy(in.Policy); err != nil {
		return out, fmt.Errorf("cannot resolve module: %w", err)
	}
	return out, nil
}

// policyRefusal names why a sandbox grant was refused: the runtime has no
// --sandbox-policy-file to enable any capability, or the policy is present but
// does not permit this one for this request.
func policyRefusal(field string, p *authz.OperatorPolicy) error {
	if p == nil {
		return fmt.Errorf("%s is refused: the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities", field)
	}
	return fmt.Errorf("%s is refused: the operator policy (--sandbox-policy-file) does not permit it for this request", field)
}

// envKeys are the explicit sandbox.env variable names, offered to the operator
// policy as context.keys. A bulk envFrom import contributes no keys (they are
// not known until the run), so a key-level policy condition governs env only.
func envKeys(s *v1beta1.Sandbox) []string {
	keys := make([]string, 0, len(s.Env))
	for _, e := range s.Env {
		keys = append(keys, e.Name)
	}
	return keys
}

// envField names the environment grant for a refusal: sandbox.env when the
// Composition sets any explicit variable, else sandbox.envFrom.
func envField(s *v1beta1.Sandbox) string {
	if len(s.Env) > 0 {
		return "sandbox.env"
	}
	return "sandbox.envFrom"
}

// admitEgressPolicy runs the operator's grant policy over the Composition's
// egress rules, once per rule and method. It is the sole gate that enables
// egress: a nil policy (no --sandbox-policy-file) permits none. The refusal
// names the rule, method and host so the Composition author can act on it.
func admitEgressPolicy(policy *authz.OperatorPolicy, principal authz.Principal, rules []v1beta1.SandboxHTTPRule) error {
	if policy == nil {
		return errors.New("sandbox.egress is refused: the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)")
	}
	for i, r := range rules {
		host := r.Host
		if host == "" {
			host = r.HostPattern
		}
		for _, m := range r.Methods {
			if !policy.PermitsEgress(principal, authz.EgressGrant{Host: r.Host, HostPattern: r.HostPattern, Method: m, Path: r.PathPrefix}) {
				return fmt.Errorf("sandbox.egress.http[%d] %s to host %q is refused: the operator policy (--sandbox-policy-file) does not permit it", i, strings.ToUpper(m), host)
			}
		}
	}
	return nil
}

// runOptions turns the Input's limits into the run's budget, checked against
// the runtime's ceilings: a Composition may ask for less than the operator
// allows, never more, and the refusal names both values so either author can
// act on it.
func runOptions(l *v1beta1.Limits, ceiling engine.Config) (engine.RunOptions, error) {
	var opts engine.RunOptions
	if l == nil {
		return opts, nil
	}
	if l.Timeout != nil {
		timeout := l.Timeout.Duration
		if timeout <= 0 {
			return opts, fmt.Errorf("limits.timeout %s must be positive", timeout)
		}
		if timeout > ceiling.Timeout {
			return opts, fmt.Errorf("limits.timeout %s exceeds the runtime's --module-timeout of %s", timeout, ceiling.Timeout)
		}
		opts.Timeout = timeout
	}
	if l.Memory != nil {
		memory := l.Memory.Value()
		if memory <= 0 {
			return opts, fmt.Errorf("limits.memory %s must be positive", l.Memory)
		}
		if memory > ceiling.MemoryLimit {
			return opts, fmt.Errorf("limits.memory %s exceeds the runtime's --module-memory-limit of %s", l.Memory, resource.NewQuantity(ceiling.MemoryLimit, resource.BinarySI))
		}
		opts.MemoryLimit = memory
	}
	return opts, nil
}
