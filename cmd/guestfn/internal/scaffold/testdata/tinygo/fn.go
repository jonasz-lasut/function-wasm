// Package main is the my-fn guest built with TinyGo: a ConfigMap
// greeting the composite resource, without function-sdk-go. It works on the raw
// protobuf messages (internal/fnv1, generated from the vendored proto with
// reflection-free vtprotobuf codecs) and implements the function-wasm ABI in
// abi_wasip1.go itself, so the module is about a megabyte.
package main

import (
	"errors"
	"time"

	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/structpb"

	"github.com/example/my-fn/internal/fnv1"
)

const defaultTTL = 60 * time.Second

// RunFunction adds a ConfigMap greeting the composite resource to the desired
// state. It is a plain function so it can be unit-tested natively.
func RunFunction(req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
	logInfo("Running function", "tag", req.GetMeta().GetTag())
	rsp := &fnv1.RunFunctionResponse{
		Meta:    &fnv1.ResponseMeta{Tag: req.GetMeta().GetTag(), Ttl: durationpb.New(defaultTTL)},
		Desired: req.GetDesired(),
	}

	greeting := "hello"
	if cfg := req.GetInput().GetFields()["config"].GetStructValue(); cfg != nil {
		if g, ok := cfg.GetFields()["greeting"]; ok {
			s, isString := g.GetKind().(*structpb.Value_StringValue)
			if !isString {
				return nil, errors.New("cannot read config: greeting must be a string")
			}
			greeting = s.StringValue
		}
		// greetingUrl fetches the greeting through the host instead — the
		// sandbox.egress grant of the Composition decides whether it may.
		if u, ok := cfg.GetFields()["greetingUrl"]; ok {
			s, isString := u.GetKind().(*structpb.Value_StringValue)
			if !isString {
				return nil, errors.New("cannot read config: greetingUrl must be a string")
			}
			text, err := httpGetText(s.StringValue)
			if err != nil {
				return nil, errors.New("cannot fetch greeting: " + err.Error())
			}
			greeting = text
		}
	}

	xr := req.GetObserved().GetComposite().GetResource()
	if xr == nil {
		return nil, errors.New("cannot get observed composite resource: none in request")
	}
	name := xr.GetFields()["metadata"].GetStructValue().GetFields()["name"].GetStringValue()

	cm, err := structpb.NewStruct(map[string]any{
		"apiVersion": "v1",
		"kind":       "ConfigMap",
		"data":       map[string]any{"greeting": greeting + " " + name},
	})
	if err != nil {
		return nil, err
	}
	if rsp.GetDesired() == nil {
		rsp.Desired = &fnv1.State{}
	}
	if rsp.GetDesired().GetResources() == nil {
		rsp.Desired.Resources = map[string]*fnv1.Resource{}
	}
	rsp.Desired.Resources["greeting"] = &fnv1.Resource{Resource: cm}
	rsp.Results = append(rsp.Results, &fnv1.Result{
		Severity: fnv1.Severity_SEVERITY_NORMAL,
		Message:  "greeted " + name,
		Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
	})
	rsp.Conditions = append(rsp.Conditions, &fnv1.Condition{
		Type:   "FunctionSuccess",
		Status: fnv1.Status_STATUS_CONDITION_TRUE,
		Reason: "Success",
		Target: fnv1.Target_TARGET_COMPOSITE_AND_CLAIM.Enum(),
	})
	return rsp, nil
}

// handle is the guest half of ABI v1: decode, run, encode; a returned error
// becomes a fatal result so the host can always decode the reply.
func handle(in []byte) []byte {
	req := &fnv1.RunFunctionRequest{}
	if err := req.UnmarshalVT(in); err != nil {
		return encode(fatal(nil, "cannot decode RunFunctionRequest: "+err.Error()))
	}
	rsp, err := RunFunction(req)
	if err != nil {
		rsp = fatal(req, err.Error())
	}
	return encode(rsp)
}

func fatal(req *fnv1.RunFunctionRequest, msg string) *fnv1.RunFunctionResponse {
	rsp := &fnv1.RunFunctionResponse{}
	if req != nil {
		rsp.Meta = &fnv1.ResponseMeta{Tag: req.GetMeta().GetTag(), Ttl: durationpb.New(defaultTTL)}
	}
	rsp.Results = append(rsp.Results, &fnv1.Result{
		Severity: fnv1.Severity_SEVERITY_FATAL,
		Message:  msg,
		Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
	})
	return rsp
}

func encode(rsp *fnv1.RunFunctionResponse) []byte {
	out, err := rsp.MarshalVT()
	if err != nil {
		out, _ = fatal(nil, "cannot encode RunFunctionResponse: "+err.Error()).MarshalVT()
	}
	return out
}

func main() {}
