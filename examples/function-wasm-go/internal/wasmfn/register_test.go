package wasmfn

import (
	"context"
	"errors"
	"io"
	"os"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/response"
)

type runnerFunc func(ctx context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error)

func (f runnerFunc) RunFunction(ctx context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
	return f(ctx, req)
}

func encodeRequest(t *testing.T, req *fnv1.RunFunctionRequest) []byte {
	t.Helper()
	b, err := proto.Marshal(req)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func fatalResponse(msg string) *fnv1.RunFunctionResponse {
	return &fnv1.RunFunctionResponse{
		Meta: &fnv1.ResponseMeta{Tag: "t", Ttl: durationpb.New(response.DefaultTTL)},
		Results: []*fnv1.Result{{
			Severity: fnv1.Severity_SEVERITY_FATAL,
			Message:  msg,
			Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
		}},
	}
}

func TestHandle(t *testing.T) {
	req := &fnv1.RunFunctionRequest{Meta: &fnv1.RequestMeta{Tag: "t"}}
	ok := &fnv1.RunFunctionResponse{Meta: &fnv1.ResponseMeta{Tag: "t", Ttl: durationpb.New(response.DefaultTTL)}}
	response.Normal(ok, "done")

	type args struct {
		runner Runner
		in     []byte
	}
	cases := map[string]struct {
		reason string
		args   args
		want   *fnv1.RunFunctionResponse
	}{
		"Success": {
			reason: "The runner's response is encoded as is.",
			args: args{
				runner: runnerFunc(func(_ context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
					rsp := response.To(req, response.DefaultTTL)
					response.Normal(rsp, "done")
					return rsp, nil
				}),
				in: encodeRequest(t, req),
			},
			want: ok,
		},
		"Error": {
			reason: "A returned error becomes a fatal result on a fresh response, as gRPC would drop the returned one.",
			args: args{
				runner: runnerFunc(func(_ context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
					rsp := response.To(req, response.DefaultTTL)
					response.Normal(rsp, "ignored")
					return rsp, errors.New("boom")
				}),
				in: encodeRequest(t, req),
			},
			want: fatalResponse("boom"),
		},
		"Panic": {
			reason: "A panic is recovered into a fatal result.",
			args: args{
				runner: runnerFunc(func(context.Context, *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
					panic("kaboom")
				}),
				in: encodeRequest(t, req),
			},
			want: fatalResponse("RunFunction panicked: kaboom"),
		},
		"NilResponse": {
			reason: "A nil response without error becomes an empty response.",
			args: args{
				runner: runnerFunc(func(context.Context, *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
					return nil, nil
				}),
				in: encodeRequest(t, req),
			},
			want: &fnv1.RunFunctionResponse{Meta: &fnv1.ResponseMeta{Tag: "t", Ttl: durationpb.New(response.DefaultTTL)}},
		},
		"BadRequest": {
			reason: "Bytes that are not a request become a fatal result without meta, there being no tag to echo.",
			args:   args{runner: runnerFunc(nil), in: []byte{0xff, 0xff}},
			want: &fnv1.RunFunctionResponse{Results: []*fnv1.Result{{
				Severity: fnv1.Severity_SEVERITY_FATAL,
				Message:  "cannot decode RunFunctionRequest: proto: cannot parse invalid wire-format data",
				Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
			}}},
		},
		"NoRunner": {
			reason: "Forgetting Register is reported instead of crashing.",
			args:   args{runner: nil, in: encodeRequest(t, req)},
			want:   fatalResponse("no Runner registered: call wasmfn.Register from an init function"),
		},
	}
	stderr = io.Discard
	t.Cleanup(func() { stderr = os.Stderr })
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			runner = tc.args.runner
			out := handle(context.Background(), tc.args.in)
			got := &fnv1.RunFunctionResponse{}
			if err := proto.Unmarshal(out, got); err != nil {
				t.Fatalf("\n%s\nhandle() returned undecodable bytes: %v", tc.reason, err)
			}
			// protobuf-go varies the whitespace in its error messages on
			// purpose; normalise before comparing.
			for _, r := range got.GetResults() {
				r.Message = strings.ReplaceAll(r.GetMessage(), "\u00a0", " ")
			}
			if diff := cmp.Diff(tc.want, got, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nhandle(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}
