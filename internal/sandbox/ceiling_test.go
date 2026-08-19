package sandbox

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

func TestNewCeiling(t *testing.T) {
	cases := map[string]struct {
		reason string
		opts   Options
	}{
		"NoProbe":    {reason: "A policy that grants no private /tmp needs no probe."},
		"PrivateTmp": {reason: "The private /tmp probe passes under a writable TMPDIR.", opts: Options{ProbePrivateTmp: true}},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if _, err := NewCeiling(tc.opts); err != nil {
				t.Fatalf("\n%s\nNewCeiling(): unexpected error %v", tc.reason, err)
			}
		})
	}
}

func TestNewCeilingPrivateTmpUnwritable(t *testing.T) {
	t.Setenv("TMPDIR", filepath.Join(t.TempDir(), "missing"))
	_, err := NewCeiling(Options{ProbePrivateTmp: true})
	if err == nil || !strings.Contains(err.Error(), "the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot create one under") {
		t.Fatalf("NewCeiling(): want the private /tmp probe to fail under an unusable TMPDIR, got %v", err)
	}
}

func TestGrant(t *testing.T) {
	c, err := NewCeiling(Options{})
	if err != nil {
		t.Fatal(err)
	}

	cases := map[string]struct {
		reason  string
		sandbox *v1beta1.Sandbox
		want    Grant
	}{
		"Nil":        {reason: "No sandbox is the default sandbox."},
		"Empty":      {reason: "An empty sandbox is the default sandbox.", sandbox: &v1beta1.Sandbox{Filesystem: &v1beta1.SandboxFilesystem{}}},
		"PrivateTmp": {reason: "The private /tmp is read from the shape; whether the run may use it is the policy's decision.", sandbox: &v1beta1.Sandbox{Filesystem: &v1beta1.SandboxFilesystem{PrivateTmp: true}}, want: Grant{PrivateTmp: true}},
		"EnvIgnored": {
			reason:  "Env is not this method's business: Materialize resolves it, the policy enables it.",
			sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "GREETING", Value: new("hello")}}},
		},
		"EgressIgnored": {
			reason:  "Egress is not this method's business: it neither grants nor refuses it.",
			sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}}}}},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got := c.Grant(tc.sandbox)
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Errorf("\n%s\nGrant(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}
