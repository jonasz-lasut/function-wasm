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
	"github.com/jonasz-lasut/function-wasm/internal/authz"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

const manifestRef = "ghcr.io/example/fn@sha256:0000000000000000000000000000000000000000000000000000000000000000"

func TestAdmit(t *testing.T) {
	open, err := sandbox.NewCeiling(sandbox.Options{})
	if err != nil {
		t.Fatal(err)
	}
	// The egress mechanism: the operator host allowlist is Cedar's (grantEgress),
	// so New itself admits any host - the host gate is the policy, exercised in
	// the operator-policy cases below.
	egressMech, err := egress.New()
	if err != nil {
		t.Fatal(err)
	}
	engineCfg := engine.Config{Timeout: 10 * time.Second, MemoryLimit: 256 << 20}
	static := v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}

	// The operator's grant policy is the sole enabler. base has none, so every
	// sandbox grant is refused; allowAll permits every capability unconditionally;
	// teamA permits them only for namespace team-a (default-deny for the rest).
	base := Ceilings{Engine: engineCfg, Sandbox: open, Egress: egressMech}
	allowAll, err := authz.NewOperatorPolicy("test.cedar", []byte(`
permit (principal, action == Action::"usePrivateTmp", resource);
permit (principal, action == Action::"setEnv", resource);
permit (principal, action == Action::"grantEgress", resource);
`))
	if err != nil {
		t.Fatal(err)
	}
	teamA, err := authz.NewOperatorPolicy("test.cedar", []byte(`
permit (principal, action == Action::"usePrivateTmp", resource) when { principal.namespace == "team-a" };
permit (principal, action == Action::"setEnv", resource) when { principal.namespace == "team-a" };
permit (principal, action == Action::"grantEgress", resource) when { principal.namespace == "team-a" };
`))
	if err != nil {
		t.Fatal(err)
	}
	allowAllC := base
	allowAllC.Policy = allowAll
	teamAC := base
	teamAC.Policy = teamA

	type args struct {
		in        *v1beta1.Input
		c         Ceilings
		principal authz.Principal
	}
	type want struct {
		options     engine.RunOptions
		grant       sandbox.Grant
		http        bool
		concurrency int
		err         string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"Default": {
			reason: "An Input asking for nothing gets the default sandbox and the ceilings.",
			args:   args{in: &v1beta1.Input{Module: static}, c: base},
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
			}, c: allowAllC},
			want: want{
				options: engine.RunOptions{Timeout: 5 * time.Second, MemoryLimit: 64 << 20, PrivateTmp: true},
				grant:   sandbox.Grant{PrivateTmp: true},
				http:    true,
			},
		},
		"BadSandboxShape": {
			reason: "The sandbox's shape is judged first, before any ceiling.",
			args:   args{in: &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "1x", Value: new("y")}}}}, c: base},
			want:   want{err: `sandbox.env[0].name "1x" is not an identifier`},
		},
		"PrivateTmpRefusedNoPolicy": {
			reason: "With no --sandbox-policy-file the policy enables nothing: the grant is refused.",
			args:   args{in: &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Filesystem: &v1beta1.SandboxFilesystem{PrivateTmp: true}}}, c: base},
			want:   want{err: "sandbox.filesystem.privateTmp is refused: the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities"},
		},
		"EnvRefusedNoPolicy": {
			reason: "The same for the environment.",
			args:   args{in: &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "A", Value: new("b")}}}}, c: base},
			want:   want{err: "sandbox.env is refused: the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities"},
		},
		"EgressRefusedNoPolicy": {
			reason: "With no --sandbox-policy-file egress is not grantable.",
			args:   args{in: &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}}}}}}, c: base},
			want:   want{err: "sandbox.egress is refused: the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)"},
		},
		"EgressNoMechanism": {
			reason: "The policy grants egress but no egress mechanism was built: refused before a run.",
			args: args{
				in:        &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}}}}}},
				c:         Ceilings{Engine: engineCfg, Sandbox: open, Policy: allowAll},
				principal: authz.Principal{Namespace: "team-a"},
			},
			want: want{err: "sandbox.egress is refused: the runtime has no egress mechanism"},
		},
		"TimeoutOverCeiling": {
			reason: "A limit above its ceiling names both.",
			args:   args{in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Timeout: &metav1.Duration{Duration: time.Minute}}}, c: base},
			want:   want{err: "limits.timeout 1m0s exceeds the runtime's --module-timeout of 10s"},
		},
		"MemoryOverCeiling": {
			reason: "The same for memory.",
			args:   args{in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Memory: resource.NewQuantity(1<<30, resource.BinarySI)}}, c: base},
			want:   want{err: "limits.memory 1Gi exceeds the runtime's --module-memory-limit of 256Mi"},
		},
		"NonPositiveTimeout": {
			reason: "A zero or negative limit is refused.",
			args:   args{in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Timeout: &metav1.Duration{}}}, c: base},
			want:   want{err: "limits.timeout 0s must be positive"},
		},
		"BadModule": {
			reason: "The module's shape is judged last, in the runtime's words.",
			args:   args{in: &v1beta1.Input{Module: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI}}, c: base},
			want:   want{err: "cannot resolve module: module.type OCI needs exactly one of module.oci and module.from"},
		},
		"BadPolicy": {
			reason: "So is the policy's.",
			args:   args{in: &v1beta1.Input{Module: static, Policy: &v1beta1.Policy{CredentialsAllowList: []string{"x"}}}, c: base},
			want:   want{err: "cannot resolve module: policy.credentialsAllowList requires policy.repositoryAllowList"},
		},
		"FromUnread": {
			reason: "A module.from source passes shape checks here; the composite resource is FromComposite's business.",
			args:   args{in: &v1beta1.Input{Module: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "status.module"}}, c: base},
		},
		"ConcurrencySet": {
			reason: "A concurrency limit is passed through.",
			args: args{
				in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Concurrency: new(int32(4))}},
				c:  base,
			},
			want: want{concurrency: 4},
		},
		"ConcurrencyCapped": {
			reason: "Concurrency above --max-concurrent-runs is silently capped.",
			args: args{
				in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Concurrency: new(int32(10))}},
				c:  Ceilings{Engine: engine.Config{Timeout: 10 * time.Second, MemoryLimit: 256 << 20, MaxConcurrentRuns: 5}, Sandbox: open, Egress: egressMech},
			},
			want: want{concurrency: 5},
		},
		"ConcurrencyNoBound": {
			reason: "Concurrency without --max-concurrent-runs is uncapped.",
			args: args{
				in: &v1beta1.Input{Module: static, Limits: &v1beta1.Limits{Concurrency: new(int32(100))}},
				c:  base,
			},
			want: want{concurrency: 100},
		},
		"OperatorPolicyPrivateTmpAllowed": {
			reason: "The operator policy enables a capability for a matching principal.",
			args: args{
				in:        &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Filesystem: &v1beta1.SandboxFilesystem{PrivateTmp: true}}},
				c:         teamAC,
				principal: authz.Principal{Namespace: "team-a"},
			},
			want: want{options: engine.RunOptions{PrivateTmp: true}, grant: sandbox.Grant{PrivateTmp: true}},
		},
		"OperatorPolicyPrivateTmpDenied": {
			reason: "Default-deny: another principal is refused.",
			args: args{
				in:        &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Filesystem: &v1beta1.SandboxFilesystem{PrivateTmp: true}}},
				c:         teamAC,
				principal: authz.Principal{Namespace: "team-b"},
			},
			want: want{err: "sandbox.filesystem.privateTmp is refused: the operator policy (--sandbox-policy-file) does not permit it for this request"},
		},
		"OperatorPolicyEnvDenied": {
			reason: "The environment grant is gated too.",
			args: args{
				in:        &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Env: []v1beta1.EnvVar{{Name: "A", Value: new("b")}}}},
				c:         teamAC,
				principal: authz.Principal{Namespace: "team-b"},
			},
			want: want{err: "sandbox.env is refused: the operator policy (--sandbox-policy-file) does not permit it for this request"},
		},
		"OperatorPolicyEgressDenied": {
			reason: "An egress rule is refused by the operator policy, naming the method and host.",
			args: args{
				in:        &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}}}}}},
				c:         teamAC,
				principal: authz.Principal{Namespace: "team-b"},
			},
			want: want{err: `sandbox.egress.http[0] GET to host "api.example.com" is refused: the operator policy (--sandbox-policy-file) does not permit it`},
		},
		"OperatorPolicyEgressAllowed": {
			reason: "A matching principal keeps the egress grant.",
			args: args{
				in:        &v1beta1.Input{Module: static, Sandbox: &v1beta1.Sandbox{Egress: &v1beta1.SandboxEgress{HTTP: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: []string{"GET"}}}}}},
				c:         teamAC,
				principal: authz.Principal{Namespace: "team-a"},
			},
			want: want{http: true},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := Admit(tc.args.in, tc.args.c, tc.args.principal)
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
			if got.Concurrency != tc.want.concurrency {
				t.Errorf("\n%s\nAdmit() concurrency: got %d, want %d", tc.reason, got.Concurrency, tc.want.concurrency)
			}
		})
	}
}
