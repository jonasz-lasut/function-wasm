package main

import (
	"context"
	"encoding/json"

	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/request"
	"github.com/crossplane/function-sdk-go/resource"
	"github.com/crossplane/function-sdk-go/resource/composed"
	"github.com/crossplane/function-sdk-go/response"
)

// Config is what the Composition passes under input.config. The runtime
// holds it to the manifest's config.schema before the run.
type Config struct {
	// Greeting is written into the ConfigMap; defaults to "hello".
	Greeting string `json:"greeting,omitempty"`
}

// Function composes a ConfigMap greeting the composite resource.
type Function struct {
	fnv1.UnimplementedFunctionRunnerServiceServer

	log logging.Logger
}

// RunFunction adds a ConfigMap named after the composite resource to the
// desired state. Nothing here knows about the runtime: the same function
// serves a gRPC Function or a WebAssembly module.
func (f *Function) RunFunction(_ context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
	f.log.Info("Running function", "tag", req.GetMeta().GetTag())
	rsp := response.To(req, response.DefaultTTL)

	cfg := Config{Greeting: "hello"}
	if err := getConfig(req, &cfg); err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot read config"))
		return rsp, nil
	}

	xr, err := request.GetObservedCompositeResource(req)
	if err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot get observed composite resource from %T", req))
		return rsp, nil
	}
	desired, err := request.GetDesiredComposedResources(req)
	if err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot get desired composed resources from %T", req))
		return rsp, nil
	}

	cm := composed.New()
	cm.SetAPIVersion("v1")
	cm.SetKind("ConfigMap")
	if err := cm.SetString("data.greeting", cfg.Greeting+" "+xr.Resource.GetName()); err != nil {
		response.Fatal(rsp, errors.Wrap(err, "cannot set greeting"))
		return rsp, nil
	}
	desired["greeting"] = &resource.DesiredComposed{Resource: cm}

	if err := response.SetDesiredComposedResources(rsp, desired); err != nil {
		response.Fatal(rsp, errors.Wrapf(err, "cannot set desired composed resources in %T", rsp))
		return rsp, nil
	}
	response.Normal(rsp, "greeted "+xr.Resource.GetName())
	response.ConditionTrue(rsp, "FunctionSuccess", "Success").TargetCompositeAndClaim()
	return rsp, nil
}

// getConfig decodes input.config, the Input's configuration block, into v.
// An Input with no config leaves v as it is.
func getConfig(req *fnv1.RunFunctionRequest, v any) error {
	cfg, ok := req.GetInput().GetFields()["config"]
	if !ok {
		return nil
	}
	raw, err := cfg.MarshalJSON()
	if err != nil {
		return errors.Wrap(err, "cannot encode input.config")
	}
	return errors.Wrap(json.Unmarshal(raw, v), "cannot decode input.config")
}
