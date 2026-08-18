package authz

import (
	"testing"
)

func TestRepositoryFencePermits(t *testing.T) {
	fence, err := NewRepositoryFence()
	if err != nil {
		t.Fatalf("NewRepositoryFence(): %v", err)
	}

	cases := map[string]struct {
		reason   string
		location string
		allowed  []string
		want     bool
	}{
		"ExactRepo": {
			reason:   "A prefix equal to the location is admitted (Cedar's in is reflexive).",
			location: "ghcr.io/team/mod",
			allowed:  []string{"ghcr.io/team/mod"},
			want:     true,
		},
		"ChildOfPrefix": {
			reason:   "A location the prefix fences at a slash is admitted.",
			location: "ghcr.io/team/mod",
			allowed:  []string{"ghcr.io/team"},
			want:     true,
		},
		"GrandchildOfPrefix": {
			reason:   "The boundary holds several segments deep.",
			location: "ghcr.io/team/group/mod",
			allowed:  []string{"ghcr.io/team"},
			want:     true,
		},
		"WholeRegistry": {
			reason:   "A registry-only prefix admits any repository under it.",
			location: "ghcr.io/team/mod",
			allowed:  []string{"ghcr.io"},
			want:     true,
		},
		"TrailingSlashPrefix": {
			reason:   "A prefix with a trailing slash admits a child, the same as one without.",
			location: "ghcr.io/team/mod",
			allowed:  []string{"ghcr.io/team/"},
			want:     true,
		},
		"TrailingSlashDoesNotAdmitBareRepo": {
			reason:   "A prefix with a trailing slash fences children only, not the bare repository (parity with the boundary string rule).",
			location: "ghcr.io/team/mod",
			allowed:  []string{"ghcr.io/team/mod/"},
			want:     false,
		},
		"SiblingNamespace": {
			reason:   "A prefix without a trailing slash must not admit a sibling namespace.",
			location: "ghcr.io/team-evil/mod",
			allowed:  []string{"ghcr.io/team"},
			want:     false,
		},
		"AdjacentHost": {
			reason:   "The boundary protects the host: cdn.example.com must not admit cdn.example.com.attacker.net.",
			location: "https://cdn.example.com.attacker.net/mod.wasm",
			allowed:  []string{"https://cdn.example.com"},
			want:     false,
		},
		"HTTPChild": {
			reason:   "An HTTP location under the prefix is admitted.",
			location: "https://cdn.example.com/modules/mod.wasm",
			allowed:  []string{"https://cdn.example.com"},
			want:     true,
		},
		"SecondPrefixMatches": {
			reason:   "Any of several prefixes is enough.",
			location: "registry.example.com/team/mod",
			allowed:  []string{"ghcr.io/team", "registry.example.com/team"},
			want:     true,
		},
		"OutsideEveryPrefix": {
			reason:   "A location under no prefix is refused.",
			location: "other.example.com/mod",
			allowed:  []string{"ghcr.io/team", "registry.example.com/team"},
			want:     false,
		},
		"EmptyAllowList": {
			reason:   "An empty allow list permits nothing.",
			location: "ghcr.io/team/mod",
			allowed:  nil,
			want:     false,
		},
		"InjectionAttempt": {
			reason:   "A prefix carrying Cedar syntax is a literal repository name, not policy: it cannot widen the fence and simply fails to match.",
			location: "ghcr.io/team/mod",
			allowed:  []string{`x"] || true || ["`},
			want:     false,
		},
		"InjectionAttemptDoesNotBlockLegit": {
			reason:   "An injection-shaped entry alongside a real one leaves the real one working.",
			location: "ghcr.io/team/mod",
			allowed:  []string{`"] || true || ["`, "ghcr.io/team"},
			want:     true,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := fence.Permits(tc.location, tc.allowed); got != tc.want {
				t.Fatalf("\n%s\nPermits(%q, %v) = %v, want %v", tc.reason, tc.location, tc.allowed, got, tc.want)
			}
		})
	}
}
