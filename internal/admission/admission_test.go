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
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

const manifestRef = "ghcr.io/example/fn@sha256:0000000000000000000000000000000000000000000000000000000000000000"

func mustOperator(t *testing.T, doc string) *authz.OperatorPolicy {
	t.Helper()
	p, err := authz.NewOperatorPolicy("test.cedar", []byte(doc))
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func mustComposition(t *testing.T, doc string) *authz.CompositionPolicy {
	t.Helper()
	p, err := authz.NewCompositionPolicy([]byte(doc))
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func TestAdmit(t *testing.T) {
	engineCfg := engine.Config{Timeout: 10 * time.Second, MemoryLimit: 256 << 20}
	static := v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}
	base := Ceilings{Engine: engineCfg}

	type args struct {
		in *v1beta1.Input
		c  Ceilings
	}
	type want struct {
		options     engine.RunOptions
		concurrency int
		composition bool
		err         string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"Default": {
			reason: "An Input asking for nothing is admitted with the ceilings.",
			args:   args{in: &v1beta1.Input{Module: static}, c: base},
		},
		"Limits": {
			reason: "Limits within the ceilings are what the run gets.",
			args: args{in: &v1beta1.Input{
				Module: static,
				Limits: &v1beta1.Limits{Timeout: &metav1.Duration{Duration: 5 * time.Second}, Memory: resource.NewQuantity(64<<20, resource.BinarySI)},
			}, c: base},
			want: want{options: engine.RunOptions{Timeout: 5 * time.Second, MemoryLimit: 64 << 20}},
		},
		"CompositionPolicyCompiled": {
			reason: "The Input's compositionPolicy compiles into the composition layer.",
			args: args{in: &v1beta1.Input{
				Module:            static,
				CompositionPolicy: `permit (principal, action == Action::"grantEgress", resource);`,
			}, c: base},
			want: want{composition: true},
		},
		"CompositionPolicyMalformed": {
			reason: "Malformed Cedar is a refusal naming the field, before anything is resolved.",
			args: args{in: &v1beta1.Input{
				Module:            static,
				CompositionPolicy: `permit (principal`,
			}, c: base},
			want: want{err: "compositionPolicy is invalid: cannot compile the compositionPolicy as Cedar"},
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
				c:  Ceilings{Engine: engine.Config{Timeout: 10 * time.Second, MemoryLimit: 256 << 20, MaxConcurrentRuns: 5}},
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
			if (got.Composition != nil) != tc.want.composition {
				t.Errorf("\n%s\nAdmit() composition policy present: %v, want %v", tc.reason, got.Composition != nil, tc.want.composition)
			}
			if got.Concurrency != tc.want.concurrency {
				t.Errorf("\n%s\nAdmit() concurrency: got %d, want %d", tc.reason, got.Concurrency, tc.want.concurrency)
			}
		})
	}
}

// TestAdmitRequires pins the three-layer decision and its per-layer defaults
// (docs/one-pager-three-layer-authz.md): the manifest requests, the
// composition layer narrows only what it scopes (scoped default-permit), the
// operator layer enables (default-deny, nil refuses everything).
func TestAdmitRequires(t *testing.T) {
	egressMech, err := egress.New()
	if err != nil {
		t.Fatal(err)
	}
	// The operator layer used by most cases: every capability, any caller.
	allowAll := mustOperator(t, `
permit (principal, action == Action::"usePrivateTmp", resource);
permit (principal, action == Action::"setEnv", resource);
permit (principal, action == Action::"grantEgress", resource);
permit (principal, action == Action::"spendCredential", resource);
`)
	// A tenant-scoped operator layer, to pin default-deny by principal.
	teamA := mustOperator(t, `
permit (principal, action == Action::"usePrivateTmp", resource) when { principal.namespace == "team-a" };
permit (principal, action == Action::"grantEgress", resource) when { principal.namespace == "team-a" };
`)
	open := Ceilings{Egress: egressMech, Policy: allowAll}

	tmp := &manifest.Requires{Filesystem: &manifest.Filesystem{PrivateTmp: true}}
	egressReq := &manifest.Requires{Egress: &manifest.Egress{HTTP: []egress.HTTPRule{{Host: "api.example.com", Methods: []string{"GET"}}}}}
	envReq := &manifest.Requires{Env: []sandbox.EnvBinding{{Name: "TOKEN", FromCredential: sandbox.CredentialKey{Name: "api", Key: "token"}}}}

	type args struct {
		r         *manifest.Requires
		c         Ceilings
		comp      *authz.CompositionPolicy
		principal authz.Principal
	}
	type want struct {
		privateTmp bool
		http       bool
		env        int
		err        string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"NilRequires": {
			reason: "No manifest, no request: the default sandbox, whatever the policies would permit.",
			args:   args{r: nil, c: open},
		},
		"EmptyRequires": {
			reason: "A manifest requiring nothing gets the default sandbox.",
			args:   args{r: &manifest.Requires{}, c: open},
		},
		"NoOperatorPolicyTmp": {
			reason: "The operator layer is the enabler: no --sandbox-policy-file, no private /tmp.",
			args:   args{r: tmp, c: Ceilings{}},
			want:   want{err: "requires a private /tmp (requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities"},
		},
		"NoOperatorPolicyEgress": {
			reason: "Nor egress.",
			args:   args{r: egressReq, c: Ceilings{Egress: egressMech}},
			want:   want{err: "requires egress (requires.egress.http), but the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)"},
		},
		"NoOperatorPolicyEnv": {
			reason: "Nor env bindings.",
			args:   args{r: envReq, c: Ceilings{}},
			want:   want{err: "requires env [TOKEN] (requires.env), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities"},
		},
		"OperatorPermitsTmp": {
			reason: "Manifest requests, no composition narrowing, operator permits: granted.",
			args:   args{r: tmp, c: open},
			want:   want{privateTmp: true},
		},
		"OperatorDeniesTmp": {
			reason: "The operator layer is default-deny by principal.",
			args:   args{r: tmp, c: Ceilings{Policy: teamA}, principal: authz.Principal{Namespace: "team-b"}},
			want:   want{err: "requires a private /tmp (requires.filesystem.privateTmp), which the operator policy (--sandbox-policy-file) does not permit for this request"},
		},
		"OperatorPermitsTenantTmp": {
			reason: "The matching tenant is granted.",
			args:   args{r: tmp, c: Ceilings{Policy: teamA}, principal: authz.Principal{Namespace: "team-a"}},
			want:   want{privateTmp: true},
		},
		"CompositionAbsentPermits": {
			reason: "Scoped default-permit: an absent composition policy does not narrow a sandbox capability.",
			args:   args{r: egressReq, c: open, comp: nil},
			want:   want{http: true},
		},
		"CompositionNotScopingPermits": {
			reason: "A composition policy scoping other actions does not narrow this one.",
			args:   args{r: tmp, c: open, comp: mustComposition(t, `permit (principal, action == Action::"grantEgress", resource);`)},
			want:   want{privateTmp: true},
		},
		"CompositionScopesAndPermits": {
			reason: "A composition policy scoping the action must permit the request.",
			args:   args{r: egressReq, c: open, comp: mustComposition(t, `permit (principal, action == Action::"grantEgress", resource in HostPattern::"example.com");`)},
			want:   want{http: true},
		},
		"CompositionScopesAndDenies": {
			reason: "Scoping the action opts into default-deny within the composition layer.",
			args:   args{r: egressReq, c: open, comp: mustComposition(t, `permit (principal, action == Action::"grantEgress", resource in HostPattern::"other.net");`)},
			want:   want{err: `requires egress GET to host "api.example.com" (requires.egress.http[0]), which the compositionPolicy does not permit`},
		},
		"CompositionForbidWins": {
			reason: "A composition forbid wins over its own permit.",
			args: args{r: tmp, c: open, comp: mustComposition(t, `
permit (principal, action == Action::"usePrivateTmp", resource);
forbid (principal, action == Action::"usePrivateTmp", resource);
`)},
			want: want{err: "requires a private /tmp (requires.filesystem.privateTmp), which the compositionPolicy does not permit for this request"},
		},
		"CompositionBeforeOperator": {
			reason: "The composition layer is judged first: its refusal names it even where the operator would also deny.",
			args: args{r: tmp, c: Ceilings{Policy: teamA}, principal: authz.Principal{Namespace: "team-b"},
				comp: mustComposition(t, `permit (principal, action == Action::"usePrivateTmp", resource) when { principal.namespace == "team-c" };`)},
			want: want{err: "which the compositionPolicy does not permit"},
		},
		"OperatorDeniesEgress": {
			reason: "An egress rule the operator does not permit names the method and host.",
			args:   args{r: egressReq, c: Ceilings{Egress: egressMech, Policy: teamA}, principal: authz.Principal{Namespace: "team-b"}},
			want:   want{err: `requires egress GET to host "api.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit`},
		},
		"NoEgressMechanism": {
			reason: "Egress permitted by both layers but no mechanism built (tests): refused before a run.",
			args:   args{r: egressReq, c: Ceilings{Policy: allowAll}},
			want:   want{err: "requires egress (requires.egress.http), but the runtime has no egress mechanism"},
		},
		"EnvGranted": {
			reason: "An env binding needs setEnv and spendCredential at the operator layer; both permitted, it is granted.",
			args:   args{r: envReq, c: open},
			want:   want{env: 1},
		},
		"EnvOperatorDeniesSetEnv": {
			reason: "An operator layer without setEnv refuses the binding (default-deny).",
			args:   args{r: envReq, c: Ceilings{Policy: teamA}, principal: authz.Principal{Namespace: "team-a"}},
			want:   want{err: "requires env [TOKEN] (requires.env), which the operator policy (--sandbox-policy-file) does not permit (setEnv)"},
		},
		"EnvOperatorDeniesSpend": {
			reason: "setEnv alone is not enough: the operator layer must also permit spending the named credential.",
			args: args{r: envReq, c: Ceilings{Policy: mustOperator(t, `
permit (principal, action == Action::"setEnv", resource);
permit (principal, action == Action::"spendCredential", resource == Credential::"other");
`)}},
			want: want{err: `requires env TOKEN from credential "api", which the operator policy (--sandbox-policy-file) does not permit (spendCredential)`},
		},
		"EnvCompositionNarrowsSpend": {
			reason: "A composition policy scoping spendCredential narrows env bindings too.",
			args: args{r: envReq, c: open,
				comp: mustComposition(t, `permit (principal, action == Action::"spendCredential", resource == Credential::"db");`)},
			want: want{err: `requires env TOKEN from credential "api", which the compositionPolicy does not permit (spendCredential)`},
		},
		"EnvCompositionPermitsSpend": {
			reason: "A composition spendCredential permit for the binding's credential admits it.",
			args: args{r: envReq, c: open,
				comp: mustComposition(t, `permit (principal, action == Action::"spendCredential", resource == Credential::"api");`)},
			want: want{env: 1},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := AdmitRequires(tc.args.r, tc.args.c, tc.args.comp, tc.args.principal)
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nAdmitRequires(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nAdmitRequires(): unexpected error %v", tc.reason, err)
			}
			if got.PrivateTmp != tc.want.privateTmp {
				t.Errorf("\n%s\nAdmitRequires() PrivateTmp = %v, want %v", tc.reason, got.PrivateTmp, tc.want.privateTmp)
			}
			if (got.HTTP != nil) != tc.want.http {
				t.Errorf("\n%s\nAdmitRequires() HTTP grant present: %v, want %v", tc.reason, got.HTTP != nil, tc.want.http)
			}
			if len(got.Env) != tc.want.env {
				t.Errorf("\n%s\nAdmitRequires() env bindings: %d, want %d", tc.reason, len(got.Env), tc.want.env)
			}
			// What the layers granted is exactly what the manifest required,
			// so the manifest's own coverage check passes.
			if tc.args.r != nil {
				m := &manifest.Manifest{ABI: 1, Requires: tc.args.r}
				if err := m.Check(got.Grants(), nil, ""); err != nil {
					t.Errorf("\n%s\nCheck(Grants()): %v", tc.reason, err)
				}
			}
		})
	}
}
