package main

import (
	"context"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"
)

func TestRunFunction(t *testing.T) {
	type args struct {
		ctx context.Context
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
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: resource.MustStructJSON(`{"apiVersion":"example.org/v1","kind":"XR","metadata":{"name":"my-xr"}}`)}},
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(60 * time.Second)},
				Desired: &fnv1.State{Resources: map[string]*fnv1.Resource{
					"greeting": {Resource: resource.MustStructJSON(`{"apiVersion":"v1","kind":"ConfigMap","data":{"greeting":"hello my-xr"}}`)},
				}},
				Results:    []*fnv1.Result{{Severity: fnv1.Severity_SEVERITY_NORMAL, Message: "greeted my-xr", Target: fnv1.Target_TARGET_COMPOSITE.Enum()}},
				Conditions: []*fnv1.Condition{{Type: "FunctionSuccess", Status: fnv1.Status_STATUS_CONDITION_TRUE, Reason: "Success", Target: fnv1.Target_TARGET_COMPOSITE_AND_CLAIM.Enum()}},
			}},
		},
		"ConfiguredGreeting": {
			reason: "input.config.greeting replaces the default.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    resource.MustStructJSON(`{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{"path":"fn.wasm"},"config":{"greeting":"hi"}}`),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: resource.MustStructJSON(`{"apiVersion":"example.org/v1","kind":"XR","metadata":{"name":"my-xr"}}`)}},
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(60 * time.Second)},
				Desired: &fnv1.State{Resources: map[string]*fnv1.Resource{
					"greeting": {Resource: resource.MustStructJSON(`{"apiVersion":"v1","kind":"ConfigMap","data":{"greeting":"hi my-xr"}}`)},
				}},
				Results:    []*fnv1.Result{{Severity: fnv1.Severity_SEVERITY_NORMAL, Message: "greeted my-xr", Target: fnv1.Target_TARGET_COMPOSITE.Enum()}},
				Conditions: []*fnv1.Condition{{Type: "FunctionSuccess", Status: fnv1.Status_STATUS_CONDITION_TRUE, Reason: "Success", Target: fnv1.Target_TARGET_COMPOSITE_AND_CLAIM.Enum()}},
			}},
		},
		"BadConfig": {
			reason: "A config of the wrong shape is a fatal result.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: resource.MustStructJSON(`{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","config":{"greeting":7}}`),
			}},
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(60 * time.Second)},
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_FATAL,
					Message:  "cannot read config: cannot decode input.config: json: cannot unmarshal number into Go struct field Config.greeting of type string",
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			f := &Function{log: logging.NewNopLogger()}
			rsp, err := f.RunFunction(tc.args.ctx, tc.args.req)

			if diff := cmp.Diff(tc.want.rsp, rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want rsp, +got rsp:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.err, err, cmpopts.EquateErrors()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want err, +got err:\n%s", tc.reason, diff)
			}
		})
	}
}
