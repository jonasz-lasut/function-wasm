package authz

import (
	"testing"
)

// operatorDoc exercises every grant-policy action: a per-tenant private /tmp, a
// key-conditional setEnv, a boundary-hierarchy egress grant and a like-pattern
// egress grant conditioned on the method.
const operatorDoc = `
permit (principal, action == Action::"usePrivateTmp", resource)
when { principal.namespace == "team-a" };

permit (principal, action == Action::"setEnv", resource)
when { context.keys.contains("SAFE") };

permit (principal, action == Action::"grantEgress", resource)
when { resource in HostPattern::"example.com" };

permit (principal, action == Action::"grantEgress", resource)
when { resource.host like "*.trusted.net" && context.method == "GET" };
`

func mustOperatorPolicy(t *testing.T, doc string) *OperatorPolicy {
	t.Helper()
	p, err := NewOperatorPolicy("test.cedar", []byte(doc))
	if err != nil {
		t.Fatalf("NewOperatorPolicy(): %v", err)
	}
	return p
}

func TestOperatorPolicyPrivateTmp(t *testing.T) {
	policy := mustOperatorPolicy(t, operatorDoc)
	cases := map[string]struct {
		reason    string
		policy    *OperatorPolicy
		principal Principal
		want      bool
	}{
		"NilPolicyDenies": {
			reason:    "The policy is the sole enabler: no --sandbox-policy-file grants no private /tmp.",
			policy:    nil,
			principal: Principal{Namespace: "team-b"},
			want:      false,
		},
		"MatchingTenant": {
			reason:    "The per-tenant permit admits its namespace.",
			policy:    policy,
			principal: Principal{Namespace: "team-a"},
			want:      true,
		},
		"OtherTenantDenied": {
			reason:    "Default-deny leaves another namespace refused.",
			policy:    policy,
			principal: Principal{Namespace: "team-b"},
			want:      false,
		},
		"EmptyPrincipalDenied": {
			reason:    "A zero principal (validate without --xr) matches no namespace condition, so a default-deny policy refuses.",
			policy:    policy,
			principal: Principal{},
			want:      false,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := tc.policy.PermitsPrivateTmp(tc.principal); got != tc.want {
				t.Fatalf("\n%s\nPermitsPrivateTmp(%+v) = %v, want %v", tc.reason, tc.principal, got, tc.want)
			}
		})
	}
}

func TestOperatorPolicyHasPrivateTmpRules(t *testing.T) {
	cases := map[string]struct {
		reason string
		policy *OperatorPolicy
		want   bool
	}{
		"Nil": {
			reason: "A nil policy has no usePrivateTmp rule, so the runtime does not probe $TMPDIR.",
			policy: nil,
			want:   false,
		},
		"HasPrivateTmp": {
			reason: "A policy that grants a private /tmp is detected, so $TMPDIR is probed at startup.",
			policy: mustOperatorPolicy(t, operatorDoc),
			want:   true,
		},
		"OnlyOtherRules": {
			reason: "A policy with no usePrivateTmp rule never grants a /tmp, so probing $TMPDIR would be spurious.",
			policy: mustOperatorPolicy(t, `permit (principal, action == Action::"grantEgress", resource);`),
			want:   false,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := tc.policy.HasPrivateTmpRules(); got != tc.want {
				t.Fatalf("\n%s\nHasPrivateTmpRules() = %v, want %v", tc.reason, got, tc.want)
			}
		})
	}
}

func TestOperatorPolicyEnv(t *testing.T) {
	policy := mustOperatorPolicy(t, operatorDoc)
	cases := map[string]struct {
		reason string
		policy *OperatorPolicy
		keys   []string
		want   bool
	}{
		"NilPolicyDenies": {reason: "The policy is the sole enabler: a nil policy grants no env.", policy: nil, keys: nil, want: false},
		"KeyPermitted":    {reason: "The setEnv permit reads context.keys.", policy: policy, keys: []string{"SAFE", "OTHER"}, want: true},
		"KeyDenied":       {reason: "Keys the permit does not name are refused.", policy: policy, keys: []string{"SECRET"}, want: false},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := tc.policy.PermitsEnv(Principal{Namespace: "team-a"}, tc.keys); got != tc.want {
				t.Fatalf("\n%s\nPermitsEnv(%v) = %v, want %v", tc.reason, tc.keys, got, tc.want)
			}
		})
	}
}

func TestOperatorPolicyEgress(t *testing.T) {
	policy := mustOperatorPolicy(t, operatorDoc)
	cases := map[string]struct {
		reason string
		policy *OperatorPolicy
		grant  EgressGrant
		want   bool
	}{
		"NilPolicyDenies": {
			reason: "The policy is the sole enabler: a nil policy grants no egress.",
			policy: nil,
			grant:  EgressGrant{Host: "anything.example.org", Method: "GET"},
			want:   false,
		},
		"HostUnderBoundary": {
			reason: "A host under the granted boundary is `in` the HostPattern entity.",
			policy: policy,
			grant:  EgressGrant{Host: "api.example.com", Method: "POST"},
			want:   true,
		},
		"BoundaryHostItself": {
			reason: "The boundary host itself is admitted (Cedar's in is reflexive).",
			policy: policy,
			grant:  EgressGrant{Host: "example.com", Method: "GET"},
			want:   true,
		},
		"PatternUnderBoundary": {
			reason: "A wildcard pattern bounded by the granted host is admitted.",
			policy: policy,
			grant:  EgressGrant{HostPattern: "*.example.com", Method: "GET"},
			want:   true,
		},
		"AdjacentHostRefused": {
			reason: "The boundary protects the host: example.com.attacker.net is not under example.com.",
			policy: policy,
			grant:  EgressGrant{Host: "example.com.attacker.net", Method: "GET"},
			want:   false,
		},
		"LikePatternGet": {
			reason: "The like-pattern permit admits a matching host with the named method.",
			policy: policy,
			grant:  EgressGrant{Host: "a.trusted.net", Method: "GET"},
			want:   true,
		},
		"LikePatternWrongMethod": {
			reason: "The same host with a method the permit does not name is refused.",
			policy: policy,
			grant:  EgressGrant{Host: "a.trusted.net", Method: "POST"},
			want:   false,
		},
		"OutsideEverything": {
			reason: "A host under no permit is refused.",
			policy: policy,
			grant:  EgressGrant{Host: "evil.example.org", Method: "GET"},
			want:   false,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := tc.policy.PermitsEgress(Principal{Namespace: "team-a"}, tc.grant); got != tc.want {
				t.Fatalf("\n%s\nPermitsEgress(%+v) = %v, want %v", tc.reason, tc.grant, got, tc.want)
			}
		})
	}
}

func TestOperatorPolicyForbidWins(t *testing.T) {
	// A forbid overrides a permit: an operator can carve a tenant out of an
	// otherwise open capability.
	const doc = `
permit (principal, action == Action::"usePrivateTmp", resource);
forbid (principal, action == Action::"usePrivateTmp", resource)
when { principal.namespace == "team-evil" };
`
	policy := mustOperatorPolicy(t, doc)
	if !policy.PermitsPrivateTmp(Principal{Namespace: "team-a"}) {
		t.Error("an unconditional permit should admit team-a")
	}
	if policy.PermitsPrivateTmp(Principal{Namespace: "team-evil"}) {
		t.Error("a forbid should override the permit for team-evil")
	}
}

func TestCredentialFencePermits(t *testing.T) {
	fence, err := NewCredentialFence()
	if err != nil {
		t.Fatalf("NewCredentialFence(): %v", err)
	}
	cases := map[string]struct {
		reason      string
		credential  string
		location    string
		allowedCred []string
		allowedRepo []string
		want        bool
	}{
		"CredentialAndRepositoryAllowed": {
			reason:      "A listed credential on a repository within the allow list is admitted.",
			credential:  "pull",
			location:    "ghcr.io/team/mod",
			allowedCred: []string{"pull"},
			allowedRepo: []string{"ghcr.io/team"},
			want:        true,
		},
		"CredentialNotAllowed": {
			reason:      "A credential the list omits is refused even on an allowed repository.",
			credential:  "other",
			location:    "ghcr.io/team/mod",
			allowedCred: []string{"pull"},
			allowedRepo: []string{"ghcr.io/team"},
			want:        false,
		},
		"RepositoryOutsideAllowList": {
			reason:      "An allowed credential on a repository outside the list is refused (co-located).",
			credential:  "pull",
			location:    "ghcr.io/team-evil/mod",
			allowedCred: []string{"pull"},
			allowedRepo: []string{"ghcr.io/team"},
			want:        false,
		},
		"EmptyCredentialList": {
			reason:      "An empty credential list permits nothing.",
			credential:  "pull",
			location:    "ghcr.io/team/mod",
			allowedCred: nil,
			allowedRepo: []string{"ghcr.io/team"},
			want:        false,
		},
		"InjectionAttempt": {
			reason:      "A credential name carrying Cedar syntax is a literal id, not policy.",
			credential:  `x"] || true || ["`,
			location:    "ghcr.io/team/mod",
			allowedCred: []string{"pull"},
			allowedRepo: []string{"ghcr.io/team"},
			want:        false,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := fence.Permits(tc.credential, tc.location, tc.allowedCred, tc.allowedRepo); got != tc.want {
				t.Fatalf("\n%s\nPermits(%q, %q, %v, %v) = %v, want %v", tc.reason, tc.credential, tc.location, tc.allowedCred, tc.allowedRepo, got, tc.want)
			}
		})
	}
}
