package main

import (
	"bytes"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/testing/protocmp"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"
)

func TestRun(t *testing.T) {
	request, err := proto.Marshal(&fnv1.RunFunctionRequest{
		Meta:     &fnv1.RequestMeta{Tag: "hello"},
		Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: resource.MustStructJSON(xrJSON)}},
	})
	if err != nil {
		t.Fatal(err)
	}

	type args struct {
		stdin []byte
	}
	type want struct {
		status  int
		desired *fnv1.State
		stderr  string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"Greets": {
			reason: "The request on standard input yields the response on standard output and a zero status.",
			args:   args{stdin: request},
			want: want{status: 0, desired: &fnv1.State{Resources: map[string]*fnv1.Resource{
				"greeting": {Resource: resource.MustStructJSON(`{"apiVersion":"v1","kind":"ConfigMap","data":{"greeting":"hello my-xr"}}`)},
			}}},
		},
		"NotARequest": {
			reason: "Bytes that are not a RunFunctionRequest end the process with status 1 and one line saying so, which the runtime quotes in its fatal result.",
			args:   args{stdin: []byte{0x08}},
			want:   want{status: 1, stderr: "cannot decode RunFunctionRequest from standard input: "},
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var stdout, stderr bytes.Buffer
			status := run(logging.NewNopLogger(), bytes.NewReader(tc.args.stdin), &stdout, &stderr)
			if diff := cmp.Diff(tc.want.status, status); diff != "" {
				t.Errorf("%s\nrun(...): -want status, +got status:\n%s", tc.reason, diff)
			}
			if !strings.HasPrefix(stderr.String(), tc.want.stderr) {
				t.Errorf("%s\nrun(...): want stderr starting with %q, got %q", tc.reason, tc.want.stderr, stderr.String())
			}
			if tc.want.desired == nil {
				return
			}
			rsp := &fnv1.RunFunctionResponse{}
			if err := proto.Unmarshal(stdout.Bytes(), rsp); err != nil {
				t.Fatalf("%s\nrun(...) wrote no RunFunctionResponse: %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.desired, rsp.GetDesired(), protocmp.Transform()); diff != "" {
				t.Errorf("%s\nrun(...): -want desired, +got desired:\n%s", tc.reason, diff)
			}
		})
	}
}
