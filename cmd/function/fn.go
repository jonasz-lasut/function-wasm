package main

import (
	"context"
	"fmt"
	"runtime/debug"
	"sync"
	"time"

	"github.com/google/go-containerregistry/pkg/authn"
	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/structpb"

	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/request"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/admission"
	"github.com/jonasz-lasut/function-wasm/internal/authz"
	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
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
	// sandbox marks the sandbox startup checks as passed (the $TMPDIR probe); it
	// carries no capability state, since enablement is the policy's decision.
	sandbox *sandbox.Ceiling
	// egress is the HTTP egress mechanism (the SSRF block list, fixed budgets,
	// the operator's Cedar CIDR rules and rate limit); always built, its use
	// gated by the policy's grantEgress. Nil refuses every required egress rule.
	egress *egress.Egress
	// policy is the operator's grant policy (--sandbox-policy-file), the sole
	// authority that enables a sandbox capability; nil refuses every sandbox
	// grant, so a runtime offers only the default sandbox.
	policy *authz.OperatorPolicy

	// manifests is the on-disk store of module manifests by manifest key
	// (module.Ref.ManifestKey - the manifest's own identity, not the module
	// digest), kept beside the compiled artifacts so an artifact hit — a warm
	// volume, a restart — needs no registry read to learn what a module
	// requires; nil (tests) means every process reads it from the source once.
	// An empty entry records that a module has none.
	manifests *cache.Store
	// parsed caches each manifest key's parsed manifest (nil for a module
	// without one) so its schema is compiled once per process, like the module.
	// A source with no cacheable manifest key (a path source, read fresh) is
	// absent here.
	parsed sync.Map // manifest key → *manifest.Manifest
	// stepSlots bounds per-step concurrency (limits.concurrency), keyed by
	// the module's digest. main always sets it; nil (tests) disables the
	// per-step bound rather than panicking.
	stepSlots *engine.StepSlots
}

// ceilings are what every request's Input is admitted against.
func (f *Function) ceilings() admission.Ceilings {
	return admission.Ceilings{Engine: f.engine.Config(), Sandbox: f.sandbox, Egress: f.egress, Policy: f.policy}
}

// RunFunction resolves, loads and runs the module and returns its response
// verbatim. Everything that stops the module from running to completion is
// reported as a fatal result — a host-side panic included: one request must
// never take the process, and every other Composition it serves, down.
func (f *Function) RunFunction(ctx context.Context, req *fnv1.RunFunctionRequest) (out *fnv1.RunFunctionResponse, err error) {
	log := f.log.WithValues("tag", req.GetMeta().GetTag())
	log.Info("Running function")
	rsp := response.To(req, f.ttl)
	in := &v1beta1.Input{}
	defer func() {
		if r := recover(); r != nil {
			log.Info("Panic while running the function", "panic", fmt.Sprint(r), "stack", string(debug.Stack()))
			out, err = f.fatal(rsp, log, metrics.OutcomeError, errors.Errorf("internal error while running the module: %v", r)), nil
		}
	}()
	if err := request.GetInput(req, in); err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, errors.Wrapf(err, "cannot get function input from %T", req)), nil
	}

	// The observed composite resource identifies the caller for the policy
	// layers: its kind and namespace, read cheaply before admission. It is
	// also the source of a module.from value, read as a map further down.
	xr := req.GetObserved().GetComposite().GetResource()
	principal := principalFrom(xr)

	// What the Composition asks of the runtime — its compositionPolicy
	// compiled, limits, the module source's shape — is settled before any
	// module is resolved, fetched or compiled: nothing will run if it is
	// refused. The same admission runs offline as function validate.
	admitted, err := admission.Admit(in, f.ceilings())
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}
	limits := admitted.Options

	// A module.from source names a field of the composite resource; it is
	// read from the observed XR on every request (converting the XR is not
	// free, so only then). The composition policy is the Input's: nothing
	// but the source is read from the XR.
	var composite map[string]any
	if in.Module.From != "" && xr != nil {
		composite = xr.AsMap()
	}
	src, err := module.FromComposite(in.Module, admitted.Composition, composite)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, errors.Wrap(err, "cannot resolve module")), nil
	}
	auth, err := registryAuth(req, src)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}

	// The credential that pulls the module is the host's business: the guest
	// sees every other step credential, as a native function would, but not
	// the one that fetched it. The full set is kept aside for the manifest's
	// env bindings, which still may not name the withheld one.
	var pullCred string
	if src.OCI != nil && src.OCI.Credentials != "" {
		pullCred = src.OCI.Credentials
	}
	creds := req.GetCredentials()
	if pullCred != "" {
		req.Credentials = withoutCredential(creds, pullCred)
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

	// The module's ask — its manifest's requires — is decided by the three
	// layers: the manifest requests, the compositionPolicy and the operator
	// policy permit. A manifest can refuse a run earlier, never make one
	// possible; a module without one gets the default sandbox.
	m, err := f.manifestFor(ctx, ref)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}
	caps, err := admission.AdmitRequires(requiresOf(m), f.ceilings(), admitted.Composition, principal)
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, errors.Errorf("module %s %v", ref.Description, err)), nil
	}
	if err := checkManifestGrants(m, ref.Description, in, caps.Grants()); err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}
	limits.PrivateTmp = caps.PrivateTmp

	// The manifest's env bindings resolve against the request's own
	// credentials, the withheld pull credential excluded.
	if len(caps.Env) > 0 {
		env, err := sandbox.Materialize(caps.Env, sandbox.Sources{Credentials: creds, Withheld: pullCred})
		if err != nil {
			return f.fatal(rsp, log, metrics.OutcomeRefused, errors.Wrapf(err, "module %s", ref.Description)), nil
		}
		limits.Env = env
	}

	// The per-run client logs every request with the
	// module's reference and digest.
	if caps.HTTP != nil {
		limits.HTTP = caps.HTTP.Client(log, ref.Digest)
	}
	limits.Key = ref.Digest
	// A per-step slot, when limits.concurrency is set, is taken before the
	// engine's global slot: one step does not take every global slot from
	// every other. The slot is released when the run ends.
	if admitted.Concurrency > 0 && f.stepSlots != nil {
		release, err := f.stepSlots.Acquire(ctx, ref.Digest, admitted.Concurrency)
		if err != nil {
			return f.fatal(rsp, log, metrics.OutcomeError, errors.Wrapf(err, "module %s failed", ref.Description)), nil
		}
		defer release()
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
// visible on the runtime side too - one log line with the reason and a
// count by outcome - so an operator of a shared runtime can see what is
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

// requiresOf is the module's ask: its manifest's requires, nil when it
// carries no manifest (or one without requirements) - the default sandbox.
func requiresOf(m *manifest.Manifest) *manifest.Requires {
	if m == nil {
		return nil
	}
	return m.Requires
}

// manifestFor returns a module's parsed manifest, nil when it carries none:
// from memory, then the on-disk store, then the source (the artifact's manifest
// layer, or the wasmfn.yaml an http/path source names by reference), read once
// per manifest key per volume and parsed once per process. The key is the
// manifest's own identity (ref.ManifestKey), not the module digest, since a
// manifest-less source names its manifest separately; when it is empty the
// manifest is read fresh every request (a path file may change) and not cached.
// A manifest that does not parse or validate refuses the module.
func (f *Function) manifestFor(ctx context.Context, ref *module.Ref) (*manifest.Manifest, error) {
	key := ref.ManifestKey()
	if key == "" {
		raw, _, err := ref.Manifest(ctx)
		if err != nil {
			return nil, manifestReadError(err, ref.Description)
		}
		return parseModuleManifest(raw, ref.Description)
	}
	if v, ok := f.parsed.Load(key); ok {
		m, _ := v.(*manifest.Manifest)
		return m, nil
	}
	raw, ok := []byte(nil), false
	if f.manifests != nil {
		raw, ok = f.manifests.Get(key)
	}
	if !ok {
		var err error
		if raw, _, err = ref.Manifest(ctx); err != nil {
			return nil, manifestReadError(err, ref.Description)
		}
		if f.manifests != nil {
			// An empty entry records "no manifest": the next process asks
			// the registry nothing. A full or read-only store only costs
			// the next process the read.
			_ = f.manifests.Put(key, raw)
		}
	}
	m, err := parseModuleManifest(raw, ref.Description)
	if err != nil {
		return nil, err
	}
	f.parsed.Store(key, m)
	return m, nil
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

// principalFrom builds the operator-policy principal from the observed
// composite resource: its kind and namespace, read cheaply without converting
// the whole object. A RunFunctionRequest does not carry the Composition's name,
// so principal.composition stays empty; an operator policy that keys on it
// simply never matches. A nil XR (a request with no observed composite) yields
// the zero principal, which matches no principal condition - safe, since the
// operator policy only narrows.
func principalFrom(xr *structpb.Struct) authz.Principal {
	if xr == nil {
		return authz.Principal{}
	}
	fields := xr.GetFields()
	p := authz.Principal{XRKind: fields["kind"].GetStringValue()}
	if md := fields["metadata"].GetStructValue(); md != nil {
		p.Namespace = md.GetFields()["namespace"].GetStringValue()
	}
	return p
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
