package main

import (
	"context"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/structpb"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"
)

const xrJSON = `{"apiVersion":"example.crossplane.io/v1","kind":"XR","metadata":{"name":"my-xr"}}`

// input is the step's Input with the given config, as the runtime forwards
// it: the module block is the Composition's and the function never reads it.
func input(config string) *structpb.Struct {
	return resource.MustStructJSON(`{"apiVersion":"guestfn.crossplane.io/v1","kind":"Input","module":{"type":"OCI","oci":{"ref":"ghcr.io/example/function-oci-go@sha256:0000000000000000000000000000000000000000000000000000000000000000"}},"config":` + config + `}`)
}

func TestRunFunction(t *testing.T) {
	meta := &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(60 * time.Second)}
	observed := &fnv1.State{Composite: &fnv1.Resource{Resource: resource.MustStructJSON(xrJSON)}}
	greeting := func(text string) *fnv1.State {
		return &fnv1.State{Resources: map[string]*fnv1.Resource{
			"greeting": {Resource: resource.MustStructJSON(`{"apiVersion":"v1","kind":"ConfigMap","data":{"greeting":"` + text + `"}}`), Ready: fnv1.Ready_READY_TRUE},
		}}
	}
	success := []*fnv1.Condition{{Type: "FunctionSuccess", Status: fnv1.Status_STATUS_CONDITION_TRUE, Reason: "Success", Target: fnv1.Target_TARGET_COMPOSITE_AND_CLAIM.Enum()}}

	type args struct {
		req *fnv1.RunFunctionRequest
	}
	type want struct {
		rsp *fnv1.RunFunctionResponse
		err error
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"DefaultGreeting": {
			reason: "Without config the ConfigMap says hello to the XR.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Observed: observed,
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta:       meta,
				Desired:    greeting("hello my-xr"),
				Results:    []*fnv1.Result{{Severity: fnv1.Severity_SEVERITY_NORMAL, Message: "greeted my-xr", Target: fnv1.Target_TARGET_COMPOSITE.Enum()}},
				Conditions: success,
			}},
		},
		"ConfiguredGreeting": {
			reason: "input.config.greeting replaces the default.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Observed: observed,
				Input:    input(`{"greeting":"howdy"}`),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta:       meta,
				Desired:    greeting("howdy my-xr"),
				Results:    []*fnv1.Result{{Severity: fnv1.Severity_SEVERITY_NORMAL, Message: "greeted my-xr", Target: fnv1.Target_TARGET_COMPOSITE.Enum()}},
				Conditions: success,
			}},
		},
		"BadConfig": {
			reason: "A config that does not decode is a fatal result, not an error.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Observed: observed,
				Input:    input(`{"greeting":7}`),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta:    meta,
				Results: []*fnv1.Result{{Severity: fnv1.Severity_SEVERITY_FATAL, Message: "cannot read config: cannot decode input.config: json: cannot unmarshal number into Go struct field Config.greeting of type string", Target: fnv1.Target_TARGET_COMPOSITE.Enum()}},
			}},
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			f := &Function{log: logging.NewNopLogger()}
			rsp, err := f.RunFunction(context.Background(), tc.args.req)
			if diff := cmp.Diff(tc.want.rsp, rsp, protocmp.Transform()); diff != "" {
				t.Errorf("%s\nf.RunFunction(...): -want rsp, +got rsp:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.err, err, cmpopts.EquateErrors()); diff != "" {
				t.Errorf("%s\nf.RunFunction(...): -want err, +got err:\n%s", tc.reason, diff)
			}
		})
	}
}
