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

	"k8s.io/apimachinery/pkg/api/resource"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

// Ceilings are the operator's: what the runtime was started with.
type Ceilings struct {
	// Engine bounds a run — Timeout (--module-timeout) and MemoryLimit
	// (--module-memory-limit), with defaults applied (engine.Config() or
	// Config.WithDefaults()).
	Engine engine.Config
	// Sandbox is the private /tmp and environment ceiling
	// (--enable-sandbox-private-tmp, --enable-sandbox-env); nil allows
	// nothing but the default sandbox.
	Sandbox *sandbox.Ceiling
	// Egress is the HTTP egress ceiling (--enable-sandbox-egress,
	// --sandbox-egress-policy); nil refuses every sandbox.egress grant.
	Egress *egress.Egress
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
}

// Admit judges in against c in the order RunFunction does — sandbox shape,
// filesystem and environment grant, egress grant, limits, module and policy
// shape — and returns the first refusal in the runtime's words: it is the
// fatal result as is. The module source is only checked for shape here; a
// module.from source is materialised against the composite resource by
// module.FromComposite afterwards.
func Admit(in *v1beta1.Input, c Ceilings) (Admitted, error) {
	var out Admitted
	if err := sandbox.Validate(in.Sandbox); err != nil {
		return out, err
	}
	// Filesystem (the private /tmp) and environment: what the Composition
	// asks for, within the operator's ceiling, or a refusal naming the grant
	// and the flag.
	grant, err := c.Sandbox.Grant(in.Sandbox)
	if err != nil {
		return out, err
	}
	// The Composition's HTTP rules must fit the operator's ceiling; the
	// intersection is this run's grant. Without the flag the capability does
	// not exist.
	if sandbox.RequestsEgress(in.Sandbox) {
		if c.Egress == nil {
			return out, errors.New("sandbox.egress is refused: the runtime was started without --enable-sandbox-egress")
		}
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
	if err := module.Validate(in.Module); err != nil {
		return out, fmt.Errorf("cannot resolve module: %w", err)
	}
	if err := module.ValidatePolicy(in.Policy); err != nil {
		return out, fmt.Errorf("cannot resolve module: %w", err)
	}
	return out, nil
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
