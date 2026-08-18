package authz

import (
	"testing"
)

// signatureDoc requires a signature for everything under ghcr.io/secure and,
// separately, for one exact HTTP location, so the tests exercise both the
// boundary hierarchy (a prefix, a sibling, an adjacent host) and an exact match.
const signatureDoc = `
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/secure" };

permit (principal, action == Action::"requireSignature", resource)
when { resource == Repository::"https://cdn.example.com/modules/fn.wasm" };
`

func TestOperatorPolicyRequiresSignature(t *testing.T) {
	policy := mustOperatorPolicy(t, signatureDoc)
	cases := map[string]struct {
		reason   string
		policy   *OperatorPolicy
		location string
		want     bool
	}{
		"NilPolicyRequiresNothing": {
			reason:   "No --policy-file adds no requirement, so today's --cosign-key behaviour stands: a nil policy requires nothing.",
			policy:   nil,
			location: "ghcr.io/secure/greeter",
			want:     false,
		},
		"WithinPrefix": {
			reason:   "A repository under the required prefix is required, at the path boundary.",
			policy:   policy,
			location: "ghcr.io/secure/greeter",
			want:     true,
		},
		"ExactPrefix": {
			reason:   "The prefix itself, named exactly, is required (Cedar's `in` is reflexive).",
			policy:   policy,
			location: "ghcr.io/secure",
			want:     true,
		},
		"SiblingNamespace": {
			reason:   "A sibling namespace that merely shares the prefix's characters is not within it, so it is not required.",
			policy:   policy,
			location: "ghcr.io/secure-evil/greeter",
			want:     false,
		},
		"AdjacentHost": {
			reason:   "A different registry is not required.",
			policy:   policy,
			location: "quay.io/secure/greeter",
			want:     false,
		},
		"ExactHTTPLocation": {
			reason:   "An exact HTTP location the policy names is required.",
			policy:   policy,
			location: "https://cdn.example.com/modules/fn.wasm",
			want:     true,
		},
		"UnlistedRepository": {
			reason:   "A repository no rule names is not required: default-deny for requireSignature means no requirement, not a refusal.",
			policy:   policy,
			location: "ghcr.io/public/greeter",
			want:     false,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := tc.policy.RequiresSignature(tc.location); got != tc.want {
				t.Fatalf("\n%s\nRequiresSignature(%q) = %v, want %v", tc.reason, tc.location, got, tc.want)
			}
		})
	}
}
