package main

import (
	"context"
	"time"

	"github.com/google/go-containerregistry/pkg/authn"
	"google.golang.org/protobuf/types/known/durationpb"

	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/request"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
)

// Function runs the WebAssembly module named by each request's Input.
type Function struct {
	fnv1.UnimplementedFunctionRunnerServiceServer

	log logging.Logger
	ttl time.Duration

	engine   *engine.Engine
	modules  *engine.Cache
	resolver *module.Resolver
}

// RunFunction resolves, loads and runs the module and returns its response
// verbatim. Everything that stops the module from running to completion is
// reported as a fatal result.
func (f *Function) RunFunction(ctx context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
	log := f.log.WithValues("tag", req.GetMeta().GetTag())
	log.Info("Running function")
	rsp := response.To(req, f.ttl)

	in := &v1beta1.Input{}
	if err := request.GetInput(req, in); err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot get function input from %T", req))
		return rsp, nil
	}

	// A *From source names a field of the composite resource; it is read
	// from the observed XR on every request.
	var composite map[string]any
	if xr := req.GetObserved().GetComposite().GetResource(); xr != nil {
		composite = xr.AsMap()
	}
	src, err := module.FromComposite(in.Module, composite)
	if err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot resolve module"))
		return rsp, nil
	}
	auth, err := registryAuth(req, src)
	if err != nil {
		response.Fatal(rsp, err)
		return rsp, nil
	}
	ref, err := f.resolver.Resolve(ctx, src, auth)
	if err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot resolve module"))
		return rsp, nil
	}
	log = log.WithValues("module", ref.Description, "digest", ref.Digest)

	mod, err := f.modules.Get(ref.Digest, func() (*engine.Module, error) {
		start := time.Now()
		wasm, err := ref.Fetch(ctx)
		if err != nil {
			return nil, errors.Wrap(err, "cannot fetch module")
		}
		m, err := f.engine.Compile(wasm)
		if err != nil {
			return nil, err
		}
		log.Debug("Compiled module", "bytes", len(wasm), "duration", time.Since(start).String())
		return m, nil
	})
	if err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot load module %s", ref.Description))
		return rsp, nil
	}

	out, err := f.engine.Run(ctx, mod, req, log)
	if err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "module %s failed", ref.Description))
		return rsp, nil
	}
	// A guest that skipped the response meta (a non-Go guest, typically)
	// still gets a well-formed reply.
	if out.GetMeta() == nil {
		out.Meta = &fnv1.ResponseMeta{Tag: req.GetMeta().GetTag(), Ttl: durationpb.New(f.ttl)}
	}
	return out, nil
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
