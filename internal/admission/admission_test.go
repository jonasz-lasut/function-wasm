package admission

import (
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	"k8s.io/apimachinery/pkg/api/resource"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

const manifestRef = "ghcr.io/example/fn@sha256:0000000000000000000000000000000000000000000000000000000000000000"

func TestAdmit(t *testing.T) {
	open, err := sandbox.NewCeiling(sandbox.Options{EnablePrivateTmp: true, EnableEnv: true})
	if err != nil {
		t.Fatal(err)
	}
	fenced, err := egress.New(egress.Policy{Hosts: []string{"api.example.com"}})
	if err != nil {
		t.Fatal(err)
	}
	engineCfg := engine.Config{Timeout: 10 * time.Second, MemoryLimit: 256 << 20}
	closed := Ceilings{Engine: engineCfg}
	all := Ceilings{Engine: engineCfg, Sandbox: open, Egress: fenced}
	static := v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}

	type args struct {
		in *v1beta1.Input
		c  Ceilings
	}
	type want struct {
		options engine.RunOptions
		grant   sandbox.Grant
		http    bool
		err     string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"Default": {
			reason: "An Input asking for nothing gets the default sandbox and the ceilings.",
			args:   args{in: &v1beta1.Input{Module: static}, c: closed},
		},
		"Everything": {
			reason: "Grants and limits within the ceilings are what the run gets.",
			args: args{in: &v1beta1.Input{
				Module: static,
				Limits: &v1beta1.Limits{Timeout: &metav1.Duration{Duration: 5 * time.Second}, Memory: resource.NewQuantity(64<<20, resource.BinarySI)},
				Sandbox: &v1beta1.Sandbox{
					Filesystem: &v1beta1.SandboxFilesystem{PrivateTmp: true},
					Env:        []v1beta1.EnvVar{{Name: "GREETING", Value: new("hi")}},
					Egress:     &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}}}},
				},
			}, c: all},
			want: want{
				options: engine.RunOptions{Timeout: 5 * time.Second, MemoryLimit: 64 << 20, PrivateTmp: true},
				grant:   sandbox.Grant{PrivateTmp: true},
				http:    true,
			},
		},
		"BadSandboxShape": {
			reason: "The sandbox's shape is judged first, before any ceiling.",
			args:   args{in: &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "1x", Value: new("y")}}}}, c: all},
			want:   want{err: `sandbox.env[0].name "1x" is not an identifier`},
		},
		"PrivateTmpRefused": {
			reason: "A grant outside the ceiling names the grant and the flag.",
			args:   args{in: &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Filesystem: &v1beta1.SandboxFilesystem{PrivateTmp: true}}}, c: closed},
			want:   want{err: "sandbox.filesystem.privateTmp is refused: the runtime was started without --enable-sandbox-private-tmp"},
		},
		"EnvRefused": {
			reason: "The same for the environment.",
			args:   args{in: &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "A", Value: new("b")}}}}, c: closed},
			want:   want{err: "sandbox.env is refused: the runtime was started without --enable-sandbox-env"},
		},
		"EgressDisabled": {
			reason: "Without the egress flag the capability does not exist.",
			args:   args{in: &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}}}}}}, c: closed},
			want:   want{err: "sandbox.egress is refused: the runtime was started without --enable-sandbox-egress"},
		},
		"EgressOutsidePolicy": {
			reason: "A rule outside the egress policy is refused with what the policy admits.",
			args:   args{in: &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "evil.example.com", Methods: []string{"GET"}}}}}}, c: all},
			want:   want{err: `sandbox.egress.http[0].host "evil.example.com" is outside the runtime's egress policy (allowed: api.example.com)`},
		},
		"TimeoutOverCeiling": {
			reason: "A limit above its ceiling names both.",
			args:   args{in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Timeout: &metav1.Duration{Duration: time.Minute}}}, c: all},
			want:   want{err: "limits.timeout 1m0s exceeds the runtime's --module-timeout of 10s"},
		},
		"MemoryOverCeiling": {
			reason: "The same for memory.",
			args:   args{in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Memory: resource.NewQuantity(1<<30, resource.BinarySI)}}, c: all},
			want:   want{err: "limits.memory 1Gi exceeds the runtime's --module-memory-limit of 256Mi"},
		},
		"NonPositiveTimeout": {
			reason: "A zero or negative limit is refused.",
			args:   args{in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Timeout: &metav1.Duration{}}}, c: all},
			want:   want{err: "limits.timeout 0s must be positive"},
		},
		"BadModule": {
			reason: "The module's shape is judged last, in the runtime's words.",
			args:   args{in: &v1beta1.Input{Module: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI}}, c: all},
			want:   want{err: "cannot resolve module: module.type OCI needs exactly one of module.oci and module.from"},
		},
		"BadPolicy": {
			reason: "So is the policy's.",
			args:   args{in: &v1beta1.Input{Module: static, Policy: &v1beta1.Policy{CredentialsAllowList: []string{"x"}}}, c: all},
			want:   want{err: "cannot resolve module: policy.credentialsAllowList requires policy.repositoryAllowList"},
		},
		"FromUnread": {
			reason: "A module.from source passes shape checks here; the composite resource is FromComposite's business.",
			args:   args{in: &v1beta1.Input{Module: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "status.module"}}, c: all},
		},
		"InstructionsWithoutFuel": {
			reason: "limits.instructions is refused without --enable-fuel.",
			args:   args{in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Instructions: new(int64(1000000))}}, c: all},
			want:   want{err: "limits.instructions is refused: the runtime was started without --enable-fuel"},
		},
		"InstructionsOverCeiling": {
			reason: "An instruction limit above the ceiling is refused.",
			args: args{
				in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Instructions: new(int64(2_000_000))}},
				c:  Ceilings{Engine: engine.Config{Timeout: 10 * time.Second, MemoryLimit: 256 << 20, Fuel: true, InstructionLimit: 1_000_000}, Sandbox: open, Egress: fenced},
			},
			want: want{err: "limits.instructions 2000000 exceeds the runtime's --module-instruction-limit of 1000000"},
		},
		"InstructionsWithinCeiling": {
			reason: "An instruction limit within the ceiling sets it on RunOptions.",
			args: args{
				in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Instructions: new(int64(500_000))}},
				c:  Ceilings{Engine: engine.Config{Timeout: 10 * time.Second, MemoryLimit: 256 << 20, Fuel: true, InstructionLimit: 1_000_000}, Sandbox: open, Egress: fenced},
			},
			want: want{options: engine.RunOptions{Instructions: 500_000}},
		},
		"InstructionsUnboundedCeiling": {
			reason: "With fuel on and no ceiling, any instruction limit is accepted.",
			args: args{
				in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Instructions: new(int64(10_000_000))}},
				c:  Ceilings{Engine: engine.Config{Timeout: 10 * time.Second, MemoryLimit: 256 << 20, Fuel: true}, Sandbox: open, Egress: fenced},
			},
			want: want{options: engine.RunOptions{Instructions: 10_000_000}},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := Admit(tc.args.in, tc.args.c)
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nAdmit(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nAdmit(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.options, got.Options, cmpopts.EquateEmpty()); diff != "" {
				t.Errorf("\n%s\nAdmit() options: -want, +got:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.grant, got.Grant, cmpopts.EquateEmpty()); diff != "" {
				t.Errorf("\n%s\nAdmit() grant: -want, +got:\n%s", tc.reason, diff)
			}
			if (got.HTTP != nil) != tc.want.http {
				t.Errorf("\n%s\nAdmit() HTTP grant present: %v, want %v", tc.reason, got.HTTP != nil, tc.want.http)
			}
		})
	}
}
