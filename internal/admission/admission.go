// Package admission is the runtime's gate over a Composition's Input and its
// module's requests. Admit runs before anything is resolved, fetched or
// compiled: limits within the engine's ceilings, the module source's shape,
// and the Input's compositionPolicy compiled. AdmitRequires runs once the
// module's manifest is read, deciding each requested capability by the
// three-layer rule (docs/one-pager-three-layer-authz.md): the manifest
// requests it, the composition layer permits it, the operator layer permits
// it - AND-combined, so a layer can only narrow. Crossplane never installs a
// function's Input CRD, so this is the only place a Composition's Input is
// ever judged; function validate runs the same functions over Compositions
// offline, so a refusal reads the same on a laptop, in CI and in an XR
// condition (docs/one-pager-admission-tooling.md).
package admission

import (
	"fmt"

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
	// decision. A nil *egress.Egress refuses every required egress rule.
	Egress *egress.Egress
	// Policy is the operator's grant policy (--sandbox-policy-file), compiled once
	// at startup - the operator layer of the three-layer decision, and the sole
	// authority that enables a sandbox capability. Nil when no
	// --sandbox-policy-file is set, in which case every requested capability is
	// refused and a runtime offers only the default sandbox. It evaluates
	// default-deny and is AND-combined with the composition layer.
	Policy *authz.OperatorPolicy
}

// Admitted is what one run gets when its Input is admitted.
type Admitted struct {
	// Options are the run's budget within the ceilings, ready for engine.Run.
	// The sandbox fields (PrivateTmp, Env, HTTP) are the caller's to fill
	// from AdmitRequires once the module's manifest is read.
	Options engine.RunOptions
	// Concurrency is the per-step concurrency limit (limits.concurrency),
	// zero when unset. Keyed by the module's digest, taken before the
	// global run slot.
	Concurrency int
	// Composition is the Input's compositionPolicy compiled (content-hash
	// cached), nil when the Input carries none. It fences a module.from
	// source (FromComposite) and is the composition layer of AdmitRequires.
	Composition *authz.CompositionPolicy
}

// Admit judges in against c in the order RunFunction does — the composition
// policy compiled, limits within the ceilings, the module source's shape —
// and returns the first refusal in the runtime's words: it is the fatal
// result as is. The module source is only checked for shape here; a
// module.from source is materialised against the composite resource by
// module.FromComposite afterwards, and the module's requested capabilities
// are decided by AdmitRequires (which takes the caller's principal) once its
// manifest is read.
func Admit(in *v1beta1.Input, c Ceilings) (Admitted, error) {
	var out Admitted
	comp, err := authz.CompileCompositionPolicy(in.CompositionPolicy)
	if err != nil {
		return out, fmt.Errorf("compositionPolicy is invalid: %w", err)
	}
	out.Composition = comp
	opts, err := runOptions(in.Limits, c.Engine)
	if err != nil {
		return out, err
	}
	out.Options = opts
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
	return out, nil
}

// Capabilities are what one run gets of the sandbox: the module's requests,
// each permitted by the composition and operator layers. The zero value is
// the default sandbox.
type Capabilities struct {
	// PrivateTmp gives the run a fresh, writable /tmp.
	PrivateTmp bool
	// HTTP is the run's egress grant, compiled from the module's rules; nil
	// when the module requires no egress.
	HTTP *egress.Grant
	// Rules are the module's egress rules the layers admitted, as required.
	Rules []egress.HTTPRule
	// Env are the module's env bindings the layers admitted, for
	// sandbox.Materialize.
	Env []sandbox.EnvBinding
}

// Grants is the admitted set in the shape manifest.Check holds Requires
// against.
func (c Capabilities) Grants() manifest.Grants {
	return manifest.Grants{PrivateTmp: c.PrivateTmp, HTTP: c.Rules, Env: c.Env}
}

// AdmitRequires decides a module's requested capabilities - its manifest's
// requires (nil: nothing, the default sandbox) - by the three-layer rule:
// each request must be permitted by the composition layer (scoped
// default-permit for the sandbox actions: absent, or scoping no rule for the
// action, it does not narrow) and by the operator layer (default-deny: no
// --sandbox-policy-file, no capability). The first refusal is returned in the
// runtime's words, for the caller to prefix with the module's name; a request
// every layer permits is granted exactly.
func AdmitRequires(r *manifest.Requires, c Ceilings, comp *authz.CompositionPolicy, principal authz.Principal) (Capabilities, error) {
	var out Capabilities
	if r == nil {
		return out, nil
	}
	if r.Filesystem != nil && r.Filesystem.PrivateTmp {
		if comp.ScopesAction(authz.ActionUsePrivateTmp) && !comp.PermitsPrivateTmp(principal) {
			return out, fmt.Errorf("requires a private /tmp (requires.filesystem.privateTmp), which the compositionPolicy does not permit for this request")
		}
		if c.Policy == nil {
			return out, fmt.Errorf("requires a private /tmp (requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities")
		}
		if !c.Policy.PermitsPrivateTmp(principal) {
			return out, fmt.Errorf("requires a private /tmp (requires.filesystem.privateTmp), which the operator policy (--sandbox-policy-file) does not permit for this request")
		}
		out.PrivateTmp = true
	}
	if r.Egress != nil && len(r.Egress.HTTP) > 0 {
		if err := admitEgress(r.Egress.HTTP, c.Policy, comp, principal); err != nil {
			return out, err
		}
		if c.Egress == nil {
			return out, fmt.Errorf("requires egress (requires.egress.http), but the runtime has no egress mechanism")
		}
		grant, err := c.Egress.Grant(r.Egress.HTTP)
		if err != nil {
			return out, err
		}
		out.HTTP, out.Rules = grant, r.Egress.HTTP
	}
	if len(r.Env) > 0 {
		if err := admitEnv(r.Env, c.Policy, comp, principal); err != nil {
			return out, err
		}
		out.Env = r.Env
	}
	return out, nil
}

// admitEgress runs both policy layers over the module's egress rules, once
// per rule and method, so a policy can key on context.method. The composition
// layer narrows only when it scopes grantEgress; the operator layer is the
// enabler, default-deny.
func admitEgress(rules []egress.HTTPRule, policy *authz.OperatorPolicy, comp *authz.CompositionPolicy, principal authz.Principal) error {
	// The composition layer first, whole: the author closest to the fix reads
	// their own layer's refusal even where the operator would also deny.
	if comp.ScopesAction(authz.ActionGrantEgress) {
		if err := eachEgress(rules, func(i int, host string, g authz.EgressGrant) error {
			if !comp.PermitsEgress(principal, g) {
				return fmt.Errorf("requires egress %s to host %q (requires.egress.http[%d]), which the compositionPolicy does not permit", g.Method, host, i)
			}
			return nil
		}); err != nil {
			return err
		}
	}
	if policy == nil {
		return fmt.Errorf("requires egress (requires.egress.http), but the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)")
	}
	return eachEgress(rules, func(i int, host string, g authz.EgressGrant) error {
		if !policy.PermitsEgress(principal, g) {
			return fmt.Errorf("requires egress %s to host %q (requires.egress.http[%d]), which the operator policy (--sandbox-policy-file) does not permit", g.Method, host, i)
		}
		return nil
	})
}

// eachEgress visits every (rule, method) pair of the module's egress rules.
func eachEgress(rules []egress.HTTPRule, visit func(i int, host string, g authz.EgressGrant) error) error {
	for i, r := range rules {
		host := r.Host
		if host == "" {
			host = r.HostPattern
		}
		for _, m := range r.Methods {
			if err := visit(i, host, authz.EgressGrant{Host: r.Host, HostPattern: r.HostPattern, Method: m, Path: r.PathPrefix}); err != nil {
				return err
			}
		}
	}
	return nil
}

// admitEnv runs both policy layers over the module's env bindings: setEnv
// once with every bound name as context.keys, then spendCredential per
// binding - the two gates of the env model
// (docs/one-pager-three-layer-authz.md). The composition layer narrows only
// where it scopes the action; the operator layer is default-deny for both.
func admitEnv(bindings []sandbox.EnvBinding, policy *authz.OperatorPolicy, comp *authz.CompositionPolicy, principal authz.Principal) error {
	keys := make([]string, 0, len(bindings))
	for _, b := range bindings {
		keys = append(keys, b.Name)
	}
	names := fmt.Sprintf("%v", keys)
	if comp.ScopesAction(authz.ActionSetEnv) && !comp.PermitsEnv(principal, keys) {
		return fmt.Errorf("requires env %s (requires.env), which the compositionPolicy does not permit (setEnv)", names)
	}
	if policy == nil {
		return fmt.Errorf("requires env %s (requires.env), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities", names)
	}
	if !policy.PermitsEnv(principal, keys) {
		return fmt.Errorf("requires env %s (requires.env), which the operator policy (--sandbox-policy-file) does not permit (setEnv)", names)
	}
	narrows := comp.ScopesAction(authz.ActionSpendCredential)
	for _, b := range bindings {
		// An env binding spends a credential with no repository in play, so the
		// composition layer sees no context.repository (authz).
		if narrows && !comp.PermitsSpendCredential(principal, b.FromCredential.Name, "") {
			return fmt.Errorf("requires env %s from credential %q, which the compositionPolicy does not permit (spendCredential)", b.Name, b.FromCredential.Name)
		}
		if !policy.PermitsSpendCredential(principal, b.FromCredential.Name) {
			return fmt.Errorf("requires env %s from credential %q, which the operator policy (--sandbox-policy-file) does not permit (spendCredential)", b.Name, b.FromCredential.Name)
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
