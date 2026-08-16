package main

import (
	"context"
	"fmt"
	"runtime/debug"
	"time"

	"github.com/google/go-containerregistry/pkg/authn"
	"google.golang.org/protobuf/types/known/durationpb"
	"k8s.io/apimachinery/pkg/api/resource"

	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/request"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

// Function runs the WebAssembly module named by each request's Input.
type Function struct {
	fnv1.UnimplementedFunctionRunnerServiceServer

	log logging.Logger
	ttl time.Duration

	engine   *engine.Engine
	modules  *engine.Cache
	resolver *module.Resolver
	// sandbox is the operator's ceiling for the Input's sandbox grants
	// (--enable-sandbox-*); nil allows nothing but the default sandbox.
	sandbox *sandbox.Ceiling
	// egress is the operator's HTTP egress ceiling (--enable-sandbox-egress,
	// --sandbox-egress-policy); nil refuses every sandbox.egress grant.
	egress *egress.Egress
}

// RunFunction resolves, loads and runs the module and returns its response
// verbatim. Everything that stops the module from running to completion is
// reported as a fatal result — a host-side panic included: one request must
// never take the process, and every other Composition it serves, down.
func (f *Function) RunFunction(ctx context.Context, req *fnv1.RunFunctionRequest) (out *fnv1.RunFunctionResponse, err error) {
	log := f.log.WithValues("tag", req.GetMeta().GetTag())
	log.Info("Running function")
	rsp := response.To(req, f.ttl)
	defer func() {
		if r := recover(); r != nil {
			log.Info("Panic while running the function", "panic", fmt.Sprint(r), "stack", string(debug.Stack()))
			out, err = f.fatal(rsp, log, metrics.OutcomeError, errors.Errorf("internal error while running the module: %v", r)), nil
		}
	}()

	in := &v1beta1.Input{}
	if err := request.GetInput(req, in); err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, errors.Wrapf(err, "cannot get function input from %T", req)), nil
	}

	// What the Composition asks of the runtime — sandbox grants and limits
	// — is settled before any module is resolved, fetched or compiled:
	// nothing will run if it is refused.
	if err := sandbox.Validate(in.Sandbox); err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}
	// Filesystem (the private /tmp) and environment: what the
	// Composition asks for, within the operator's ceiling, or a fatal result
	// naming the grant and the flag.
	grant, err := f.sandbox.Grant(in.Sandbox)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}
	// The Composition's HTTP rules must fit the operator's ceiling; the
	// intersection is this run's grant, settled before anything is resolved.
	// Without the flag the capability does not exist.
	var httpGrant *egress.Grant
	if sandbox.RequestsEgress(in.Sandbox) {
		if f.egress == nil {
			return f.fatal(rsp, log, metrics.OutcomeRefused, errors.New("sandbox.egress is refused: the runtime was started without --enable-sandbox-egress")), nil
		}
		httpGrant, err = f.egress.Grant(in.Sandbox.Egress.HTTP)
		if err != nil {
			return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
		}
	}
	limits, err := runOptions(in.Limits, f.engine.Config())
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}
	limits = withSandbox(limits, grant)

	// A module.from source names a field of the composite resource; it is
	// read from the observed XR on every request (converting the XR is not
	// free, so only then). The policy is the Input's: nothing but the source
	// is read from the XR.
	var composite map[string]any
	if in.Module.From != "" {
		if xr := req.GetObserved().GetComposite().GetResource(); xr != nil {
			composite = xr.AsMap()
		}
	}
	src, err := module.FromComposite(in.Module, in.Policy, composite)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, errors.Wrap(err, "cannot resolve module")), nil
	}
	auth, err := registryAuth(req, src)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}
	// The credential that pulls the module is the host's business: the guest
	// sees every other step credential, as a native function would, but not
	// the one that fetched it.
	if src.OCI != nil && src.OCI.Credentials != "" {
		req.Credentials = withoutCredential(req.GetCredentials(), src.OCI.Credentials)
	}
	ref, err := f.resolver.Resolve(ctx, src, auth)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, errors.Wrap(err, "cannot resolve module")), nil
	}
	log = log.WithValues("module", ref.Description, "digest", ref.Digest)

	// Signature verification gates serving, not fetching: a compiled artifact
	// on disk may predate the key, so it runs before any cache is consulted
	// (once per digest per process — the verifier remembers).
	if err := ref.Verify(ctx); err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, errors.Wrapf(err, "cannot verify module %s", ref.Description)), nil
	}
	mod, err := f.load(ctx, ref, log)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeError, err), nil
	}
	defer mod.Release()

	// The per-run client logs every request with the
	// module's reference and digest.
	if httpGrant != nil {
		limits.HTTP = httpGrant.Client(log)
	}
	// A run slot, when --max-concurrent-runs bounds them, is waited for
	// inside Run under the request context; a wait the deadline cuts short
	// is reported like any other reason the module did not run.
	got, err := f.engine.Run(ctx, mod, req, log, limits)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeError, errors.Wrapf(err, "module %s failed", ref.Description)), nil
	}
	metrics.Requests.WithLabelValues(metrics.OutcomeOK).Inc()
	// A guest that skipped the response meta (a non-Go guest, typically)
	// still gets a well-formed reply.
	if got.GetMeta() == nil {
		got.Meta = &fnv1.ResponseMeta{Tag: req.GetMeta().GetTag(), Ttl: durationpb.New(f.ttl)}
	}
	return got, nil
}

// fatal turns err into the request's fatal result and makes the refusal
// visible on the runtime side too — one log line with the reason and a
// count by outcome — so an operator of a shared runtime can see what is
// being refused (or failing) without reading every XR's conditions.
// outcome is refused (the runtime declined before running the module:
// input, policy, grants, limits, resolution, verification) or error (the
// load or the run failed).
func (f *Function) fatal(rsp *fnv1.RunFunctionResponse, log logging.Logger, outcome string, err error) *fnv1.RunFunctionResponse {
	metrics.Requests.WithLabelValues(outcome).Inc()
	log.Info("Request ended with a fatal result", "outcome", outcome, "reason", err.Error())
	response.Fatal(rsp, err)
	return rsp
}

// load returns the compiled module ref pins, leased from the cache — from
// memory, the artifact on disk, or fetched and compiled — with the fetch
// timed in the debug log. The error names the module: it is the fatal result
// as is. Requests and warm-up share it, so a warmed module is exactly what a
// request would have loaded.
func (f *Function) load(ctx context.Context, ref *module.Ref, log logging.Logger) (*engine.Module, error) {
	mod, err := f.modules.Get(ctx, ref.Digest, func(ctx context.Context) ([]byte, error) {
		start := time.Now()
		wasm, err := ref.Fetch(ctx)
		if err != nil {
			return nil, errors.Wrap(err, "cannot fetch module")
		}
		log.Debug("Fetched module", "bytes", len(wasm), "duration", time.Since(start).String())
		return wasm, nil
	})
	if err != nil {
		return nil, errors.Wrapf(err, "cannot load module %s", ref.Description)
	}
	return mod, nil
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
			return opts, errors.Errorf("limits.timeout %s must be positive", timeout)
		}
		if timeout > ceiling.Timeout {
			return opts, errors.Errorf("limits.timeout %s exceeds the runtime's --module-timeout of %s", timeout, ceiling.Timeout)
		}
		opts.Timeout = timeout
	}
	if l.Memory != nil {
		memory := l.Memory.Value()
		if memory <= 0 {
			return opts, errors.Errorf("limits.memory %s must be positive", l.Memory)
		}
		if memory > ceiling.MemoryLimit {
			return opts, errors.Errorf("limits.memory %s exceeds the runtime's --module-memory-limit of %s", l.Memory, resource.NewQuantity(ceiling.MemoryLimit, resource.BinarySI))
		}
		opts.MemoryLimit = memory
	}
	return opts, nil
}

// withSandbox adds the run's sandbox grant — its pre-opens, private /tmp and
// environment — to the options the engine applies to the store.
func withSandbox(opts engine.RunOptions, g sandbox.Grant) engine.RunOptions {
	opts.PrivateTmp = g.PrivateTmp
	opts.Env = g.Env
	return opts
}

// registryAuth returns the authenticator for an OCI source that names a
// pipeline-step credential, nil otherwise.
func registryAuth(req *fnv1.RunFunctionRequest, src v1beta1.ModuleSource) (authn.Authenticator, error) {
	if src.OCI == nil || src.OCI.Credentials == "" {
		return nil, nil
	}
	creds, err := request.GetCredentials(req, src.OCI.Credentials)
	if err != nil {
		return nil, errors.Wrapf(err, "cannot get credentials %q for module.oci", src.OCI.Credentials)
	}
	auth, err := module.AuthFor(src.OCI.Ref, creds.Data)
	if err != nil {
		return nil, errors.Wrapf(err, "cannot use credentials %q for module.oci", src.OCI.Credentials)
	}
	return auth, nil
}

// withoutCredential returns creds minus name.
func withoutCredential(creds map[string]*fnv1.Credentials, name string) map[string]*fnv1.Credentials {
	out := make(map[string]*fnv1.Credentials, len(creds))
	for k, v := range creds {
		if k != name {
			out[k] = v
		}
	}
	return out
}
