package authz

import (
	"strings"
	"testing"
)

// compositionDoc exercises every action a composition policy can scope: the
// from-source fence (pullModule with the boundary hierarchy, spendCredential
// co-located with its repository) and a narrowed sandbox capability.
const compositionDoc = `
permit (principal, action == Action::"pullModule", resource in Repository::"ghcr.io/team");

permit (principal, action == Action::"spendCredential", resource == Credential::"pull")
when { context.repository in Repository::"ghcr.io/team" };

permit (principal, action == Action::"grantEgress", resource in HostPattern::"example.com");
`

func mustCompositionPolicy(t *testing.T, doc string) *CompositionPolicy {
	t.Helper()
	p, err := NewCompositionPolicy([]byte(doc))
	if err != nil {
		t.Fatalf("NewCompositionPolicy(): %v", err)
	}
	return p
}

func TestCompileCompositionPolicy(t *testing.T) {
	cases := map[string]struct {
		reason string
		text   string
		nilP   bool
		err    string
	}{
		"Empty": {
			reason: "Empty text is the absent layer: nil policy, no error.",
			text:   "",
			nilP:   true,
		},
		"Valid": {
			reason: "A valid document compiles.",
			text:   compositionDoc,
		},
		"Malformed": {
			reason: "A malformed document is an error naming the field.",
			text:   `permit (principal`,
			err:    "cannot compile the compositionPolicy as Cedar",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := CompileCompositionPolicy(tc.text)
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\nCompileCompositionPolicy(): want error containing %q, got %v", tc.reason, tc.err, err)
				}
				// The failure is cached: the same text fails identically.
				if _, again := CompileCompositionPolicy(tc.text); again == nil || again.Error() != err.Error() {
					t.Fatalf("\n%s\nCompileCompositionPolicy(): a cached failure must repeat, got %v", tc.reason, again)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nCompileCompositionPolicy(): unexpected error %v", tc.reason, err)
			}
			if (got == nil) != tc.nilP {
				t.Fatalf("\n%s\nCompileCompositionPolicy(): nil = %v, want %v", tc.reason, got == nil, tc.nilP)
			}
			if got != nil {
				cached, err := CompileCompositionPolicy(tc.text)
				if err != nil || cached != got {
					t.Fatalf("\n%s\nCompileCompositionPolicy(): the same text must hit the cache (%p vs %p, %v)", tc.reason, got, cached, err)
				}
			}
		})
	}
}

func TestCompositionPolicyScopesAction(t *testing.T) {
	policy := mustCompositionPolicy(t, compositionDoc)
	cases := map[string]struct {
		reason string
		policy *CompositionPolicy
		action string
		want   bool
	}{
		"Nil":       {reason: "A nil policy scopes nothing.", policy: nil, action: ActionGrantEgress, want: false},
		"Scoped":    {reason: "An action a rule scopes is detected: the author opted into narrowing it.", policy: policy, action: ActionGrantEgress, want: true},
		"Pull":      {reason: "The pullModule fence rule is detected.", policy: policy, action: ActionPullModule, want: true},
		"NotScoped": {reason: "An action no rule scopes is not narrowed by this layer.", policy: policy, action: ActionUsePrivateTmp, want: false},
		"InSet": {
			reason: "The `in [...]` action form counts as scoping too.",
			policy: mustCompositionPolicy(t, `permit (principal, action in [Action::"setEnv", Action::"usePrivateTmp"], resource);`),
			action: ActionSetEnv,
			want:   true,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := tc.policy.ScopesAction(tc.action); got != tc.want {
				t.Fatalf("\n%s\nScopesAction(%q) = %v, want %v", tc.reason, tc.action, got, tc.want)
			}
		})
	}
}

func TestCompositionPolicyPullModule(t *testing.T) {
	policy := mustCompositionPolicy(t, compositionDoc)
	cases := map[string]struct {
		reason   string
		policy   *CompositionPolicy
		location string
		want     bool
	}{
		"NilDenies": {
			reason:   "A nil policy is the required fence's default-deny: a from source is refused.",
			policy:   nil,
			location: "ghcr.io/team/mod",
			want:     false,
		},
		"WithinPrefix": {
			reason:   "A location the permitted repository fences at a / is admitted.",
			policy:   policy,
			location: "ghcr.io/team/mod",
			want:     true,
		},
		"PrefixItself": {
			reason:   "The permitted repository itself is admitted (Cedar's in is reflexive).",
			policy:   policy,
			location: "ghcr.io/team",
			want:     true,
		},
		"SiblingNamespace": {
			reason:   "A sibling namespace sharing the string prefix is not within the boundary.",
			policy:   policy,
			location: "ghcr.io/team-evil/mod",
			want:     false,
		},
		"OtherRegistry": {
			reason:   "Another registry is refused.",
			policy:   policy,
			location: "docker.io/team/mod",
			want:     false,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := tc.policy.PermitsPullModule(Principal{}, tc.location); got != tc.want {
				t.Fatalf("\n%s\nPermitsPullModule(%q) = %v, want %v", tc.reason, tc.location, got, tc.want)
			}
		})
	}
}

func TestCompositionPolicySpendCredential(t *testing.T) {
	policy := mustCompositionPolicy(t, compositionDoc)
	cases := map[string]struct {
		reason     string
		policy     *CompositionPolicy
		credential string
		location   string
		want       bool
	}{
		"NilDenies": {
			reason:     "A nil policy denies: a from source cannot spend a credential unfenced.",
			policy:     nil,
			credential: "pull",
			location:   "ghcr.io/team/mod",
			want:       false,
		},
		"CoLocated": {
			reason:     "The permitted credential on a repository within the permitted boundary is admitted.",
			policy:     policy,
			credential: "pull",
			location:   "ghcr.io/team/mod",
			want:       true,
		},
		"OtherCredential": {
			reason:     "A credential no permit names is refused even on the permitted repository.",
			policy:     policy,
			credential: "other",
			location:   "ghcr.io/team/mod",
			want:       false,
		},
		"RepositoryOutside": {
			reason:     "The permitted credential outside the repository condition is refused (co-located).",
			policy:     policy,
			credential: "pull",
			location:   "ghcr.io/team-evil/mod",
			want:       false,
		},
		"NoRepositoryContext": {
			reason:     "With no location (an env binding) a policy conditioning on context.repository does not match.",
			policy:     policy,
			credential: "pull",
			location:   "",
			want:       false,
		},
		"UnconditionalBinding": {
			reason:     "A permit without a repository condition admits an env binding's credential.",
			policy:     mustCompositionPolicy(t, `permit (principal, action == Action::"spendCredential", resource == Credential::"db");`),
			credential: "db",
			location:   "",
			want:       true,
		},
		"InjectionAttempt": {
			reason:     "A credential name carrying Cedar syntax is a literal entity id, not policy.",
			policy:     policy,
			credential: `x"] || true || ["`,
			location:   "ghcr.io/team/mod",
			want:       false,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := tc.policy.PermitsSpendCredential(Principal{}, tc.credential, tc.location); got != tc.want {
				t.Fatalf("\n%s\nPermitsSpendCredential(%q, %q) = %v, want %v", tc.reason, tc.credential, tc.location, got, tc.want)
			}
		})
	}
}

func TestCompositionPolicySandboxActions(t *testing.T) {
	// The sandbox Permits methods evaluate exactly as the operator's do; the
	// scoped-default-permit around them is the caller's, via ScopesAction.
	policy := mustCompositionPolicy(t, compositionDoc)
	if policy.PermitsPrivateTmp(Principal{}) {
		t.Error("a policy with no usePrivateTmp permit must deny it when evaluated")
	}
	if !policy.PermitsEgress(Principal{}, EgressGrant{Host: "api.example.com", Method: "GET"}) {
		t.Error("the grantEgress permit must admit a host under its boundary")
	}
	if policy.PermitsEgress(Principal{}, EgressGrant{Host: "example.com.attacker.net", Method: "GET"}) {
		t.Error("an adjacent host is not under the boundary")
	}
	scoped := mustCompositionPolicy(t, `permit (principal, action == Action::"setEnv", resource) when { context.keys.contains("SAFE") };`)
	if !scoped.PermitsEnv(Principal{}, []string{"SAFE"}) {
		t.Error("the setEnv permit must admit its key")
	}
	if scoped.PermitsEnv(Principal{}, []string{"SECRET"}) {
		t.Error("a key the permit does not name is refused")
	}
	var nilPolicy *CompositionPolicy
	if nilPolicy.PermitsPrivateTmp(Principal{}) || nilPolicy.PermitsEnv(Principal{}, nil) || nilPolicy.PermitsEgress(Principal{}, EgressGrant{Host: "example.com", Method: "GET"}) {
		t.Error("a nil policy denies every evaluation; its defaults are the caller's")
	}
}

func TestOperatorPolicySpendCredential(t *testing.T) {
	policy := mustOperatorPolicy(t, `permit (principal, action == Action::"spendCredential", resource == Credential::"db") when { principal.namespace == "team-a" };`)
	cases := map[string]struct {
		reason     string
		policy     *OperatorPolicy
		principal  Principal
		credential string
		want       bool
	}{
		"NilDenies":  {reason: "The operator layer is default-deny: no policy, no credential spent.", policy: nil, credential: "db", want: false},
		"Permitted":  {reason: "The permit admits its credential for its principal.", policy: policy, principal: Principal{Namespace: "team-a"}, credential: "db", want: true},
		"OtherCred":  {reason: "A credential no permit names is refused.", policy: policy, principal: Principal{Namespace: "team-a"}, credential: "other", want: false},
		"OtherOwner": {reason: "Another principal is refused (default-deny).", policy: policy, principal: Principal{Namespace: "team-b"}, credential: "db", want: false},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := tc.policy.PermitsSpendCredential(tc.principal, tc.credential); got != tc.want {
				t.Fatalf("\n%s\nPermitsSpendCredential(%q) = %v, want %v", tc.reason, tc.credential, got, tc.want)
			}
		})
	}
}
