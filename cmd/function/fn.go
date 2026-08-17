package main

import (
	"context"
	"fmt"
	"runtime/debug"
	"sync"
	"time"

	"github.com/google/go-containerregistry/pkg/authn"
	"google.golang.org/protobuf/types/known/durationpb"

	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/request"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/admission"
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
	// sandbox is the operator's ceiling for the Input's sandbox grants
	// (--enable-sandbox-*); nil allows nothing but the default sandbox.
	sandbox *sandbox.Ceiling
	// egress is the operator's HTTP egress ceiling (--enable-sandbox-egress,
	// --sandbox-egress-policy); nil refuses every sandbox.egress grant.
	egress *egress.Egress

	// manifests is the on-disk store of module manifests by digest, kept
	// beside the compiled artifacts so an artifact hit — a warm volume, a
	// restart — needs no registry read to learn what a module requires; nil
	// (tests) means every process reads it from the source once. An empty
	// entry records that a module has none.
	manifests *cache.Store
	// parsed caches each digest's parsed manifest (nil for a module without
	// one) so its schema is compiled once per process, like the module.
	parsed sync.Map // digest → *manifest.Manifest
}

// ceilings are what every request's Input is admitted against.
func (f *Function) ceilings() admission.Ceilings {
	return admission.Ceilings{Engine: f.engine.Config(), Sandbox: f.sandbox, Egress: f.egress}
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

	// What the Composition asks of the runtime — sandbox grants, egress and
	// limits — is settled before any module is resolved, fetched or compiled:
	// nothing will run if it is refused. The same admission runs offline as
	// function validate.
	admitted, err := admission.Admit(in, f.ceilings())
	if err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}
	limits := admitted.Options

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

	// The module's own requirements — its manifest — are checked against
	// what the Composition granted and the operator admitted: a manifest can
	// refuse a run earlier, never make one possible.
	if err := f.checkManifest(ctx, ref, in, admitted); err != nil {
		return f.fatal(rsp, log, metrics.OutcomeRefused, err), nil
	}

	// The per-run client logs every request with the
	// module's reference and digest.
	if admitted.HTTP != nil {
		limits.HTTP = admitted.HTTP.Client(log)
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

// checkManifest applies the module's manifest, if it has one, to what the
// Composition was granted: an unmet requirement or a config outside the
// module's schema is a refusal naming the module. The parsed manifest and
// its compiled schema are cached per digest.
func (f *Function) checkManifest(ctx context.Context, ref *module.Ref, in *v1beta1.Input, admitted admission.Admitted) error {
	m, err := f.manifestFor(ctx, ref)
	if err != nil {
		return err
	}
	if m == nil {
		return nil
	}
	grants := manifest.Grants{PrivateTmp: admitted.Grant.PrivateTmp}
	if admitted.HTTP != nil && in.Sandbox != nil && in.Sandbox.Egress != nil {
		grants.HTTP = in.Sandbox.Egress.HTTP
	}
	if err := m.Check(grants, in.Config, manifest.RuntimeVersion()); err != nil {
		return errors.Errorf("module %s %v", ref.Description, err)
	}
	return nil
}

// manifestFor returns a module's parsed manifest, nil when it carries none:
// from memory, then the on-disk store, then the source (the artifact's
// manifest layer), read once per digest per volume and parsed once per
// process. A manifest that does not parse or validate refuses the module.
func (f *Function) manifestFor(ctx context.Context, ref *module.Ref) (*manifest.Manifest, error) {
	if v, ok := f.parsed.Load(ref.Digest); ok {
		m, _ := v.(*manifest.Manifest)
		return m, nil
	}
	raw, ok := []byte(nil), false
	if f.manifests != nil {
		raw, ok = f.manifests.Get(ref.Digest)
	}
	if !ok {
		var err error
		if raw, _, err = ref.Manifest(ctx); err != nil {
			return nil, errors.Wrapf(err, "cannot read the manifest of module %s", ref.Description)
		}
		if f.manifests != nil {
			// An empty entry records "no manifest": the next process asks
			// the registry nothing. A full or read-only store only costs
			// the next process the read.
			_ = f.manifests.Put(ref.Digest, raw)
		}
	}
	var m *manifest.Manifest
	if len(raw) > 0 {
		parsed, err := manifest.Parse(raw)
		if err != nil {
			return nil, errors.Wrapf(err, "module %s has an invalid manifest", ref.Description)
		}
		m = parsed
	}
	f.parsed.Store(ref.Digest, m)
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
