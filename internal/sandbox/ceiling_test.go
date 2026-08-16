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
		"Nothing":    {reason: "No flags is the default sandbox: a ceiling that allows nothing."},
		"PrivateTmp": {reason: "The private /tmp probe passes under a writable TMPDIR.", opts: Options{EnablePrivateTmp: true}},
		"Env":        {reason: "Environment needs no probing.", opts: Options{EnableEnv: true}},
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
	_, err := NewCeiling(Options{EnablePrivateTmp: true})
	if err == nil || !strings.Contains(err.Error(), "--enable-sandbox-private-tmp: cannot create a private /tmp under") {
		t.Fatalf("NewCeiling(): want the private /tmp probe to fail under an unusable TMPDIR, got %v", err)
	}
}

func TestGrant(t *testing.T) {
	all, err := NewCeiling(Options{EnablePrivateTmp: true, EnableEnv: true})
	if err != nil {
		t.Fatal(err)
	}
	none, err := NewCeiling(Options{})
	if err != nil {
		t.Fatal(err)
	}

	type args struct {
		ceiling *Ceiling
		sandbox *v1beta1.Sandbox
	}
	cases := map[string]struct {
		reason string
		args   args
		want   Grant
		err    string
	}{
		"Nil":                {reason: "No sandbox is the default sandbox.", args: args{ceiling: all}},
		"NilCeiling":         {reason: "A nil ceiling allows nothing but the default sandbox.", args: args{ceiling: nil, sandbox: &v1beta1.Sandbox{}}},
		"Empty":              {reason: "An empty sandbox is the default sandbox whatever the ceiling.", args: args{ceiling: nil, sandbox: &v1beta1.Sandbox{Filesystem: &v1beta1.SandboxFilesystem{}, Env: map[string]string{}}}},
		"PrivateTmp":         {reason: "The private /tmp is granted when enabled.", args: args{ceiling: all, sandbox: &v1beta1.Sandbox{Filesystem: &v1beta1.SandboxFilesystem{PrivateTmp: true}}}, want: Grant{PrivateTmp: true}},
		"PrivateTmpDisabled": {reason: "The private /tmp when not enabled names the flag.", args: args{ceiling: none, sandbox: &v1beta1.Sandbox{Filesystem: &v1beta1.SandboxFilesystem{PrivateTmp: true}}}, err: "sandbox.filesystem.privateTmp is refused: the runtime was started without --enable-sandbox-private-tmp"},
		"Env":                {reason: "The environment is granted when enabled.", args: args{ceiling: all, sandbox: &v1beta1.Sandbox{Env: map[string]string{"GREETING": "hello"}}}, want: Grant{Env: map[string]string{"GREETING": "hello"}}},
		"EnvDisabled":        {reason: "Environment variables when not enabled name the flag.", args: args{ceiling: none, sandbox: &v1beta1.Sandbox{Env: map[string]string{"GREETING": "hello"}}}, err: "sandbox.env is refused: the runtime was started without --enable-sandbox-env"},
		"EgressIgnored": {
			reason: "Egress is not this method's business: it neither grants nor refuses it.",
			args:   args{ceiling: nil, sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}}}}}},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := tc.args.ceiling.Grant(tc.args.sandbox)
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\nGrant(): want error containing %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nGrant(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Errorf("\n%s\nGrant(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}
