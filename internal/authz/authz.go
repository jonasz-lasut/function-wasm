// Package authz evaluates function-wasm's declarative admission decisions with
// Cedar (docs/one-pager-policy-engine.md). It currently owns the repository
// fence for a module the composite resource chooses (module.from): whether the
// module's normalized location lies within the repositories a Composition's
// policy.repositoryAllowList admits.
//
// Repositories are modelled as a Cedar entity hierarchy: a location's ancestors
// are its path-boundary prefixes, so `in` an allowed repository is true exactly
// when that repository equals the location or fences it with a following "/".
// The decision is therefore boundary-correct by construction - it can never
// admit a sibling namespace ("ghcr.io/team" vs "ghcr.io/team-evil") or an
// adjacent host ("https://cdn.example.com" vs
// "https://cdn.example.com.attacker.net") - the property a raw string prefix
// cannot guarantee. The allow list travels in the request context, never in the
// policy text, so a Composition-authored entry cannot inject Cedar policy.
//
// Cedar owns the decision only; callers keep their own refusal strings. The
// credential fence stays exact set membership in the module package: it has no
// boundary subtlety Cedar would improve.
package authz

import (
	_ "embed"
	"fmt"
	"strings"

	"github.com/cedar-policy/cedar-go"
	"github.com/cedar-policy/cedar-go/types"
)

const repositoryType = types.EntityType("Repository")

var (
	// pullAction is the action a repository-fence request asks about; it
	// matches the policy's `action == Action::"pullModule"`.
	pullAction = types.NewEntityUID("Action", "pullModule")
	// modulePrincipal is a placeholder: the fence does not discriminate by
	// principal, the policy leaves it unconstrained.
	modulePrincipal = types.NewEntityUID("Module", "module")
	// allowedRepositoriesKey names the context set the policy reads.
	allowedRepositoriesKey = types.String("allowedRepositories")
)

//go:embed repository_fence.cedar
var fencePolicyText []byte

// RepositoryFence decides whether a normalized module location is admitted by a
// set of repository prefixes. The compiled policy is immutable, so a Fence is
// safe for concurrent use.
type RepositoryFence struct {
	policy *cedar.PolicySet
}

// NewRepositoryFence compiles the embedded fence policy.
func NewRepositoryFence() (*RepositoryFence, error) {
	ps, err := cedar.NewPolicySetFromBytes("repository_fence.cedar", fencePolicyText)
	if err != nil {
		return nil, fmt.Errorf("cannot compile the repository fence policy: %w", err)
	}
	return &RepositoryFence{policy: ps}, nil
}

// Permits reports whether location lies within one of the allowed repository
// prefixes. location and allowed are the normalized locations the module
// package produces: registry/repository for OCI, scheme://host/path for HTTP.
// An empty allow list permits nothing.
func (f *RepositoryFence) Permits(location string, allowed []string) bool {
	resource := repo(location)

	// The resource's ancestors are its path-boundary prefixes, so `resource in
	// Repository::"p"` is true when p equals the location (Cedar's `in` is
	// reflexive) or p fences it at a "/".
	prefixes := boundaryPrefixes(location)
	parents := make([]types.EntityUID, 0, len(prefixes))
	entities := types.EntityMap{}
	for _, p := range prefixes {
		uid := repo(p)
		parents = append(parents, uid)
		entities[uid] = types.Entity{UID: uid}
	}
	entities[resource] = types.Entity{UID: resource, Parents: types.NewEntityUIDSet(parents...)}

	allowedSet := make([]types.Value, 0, len(allowed))
	for _, a := range allowed {
		allowedSet = append(allowedSet, repo(a))
	}
	req := cedar.Request{
		Principal: modulePrincipal,
		Action:    pullAction,
		Resource:  resource,
		Context:   types.NewRecord(types.RecordMap{allowedRepositoriesKey: types.NewSet(allowedSet...)}),
	}
	decision, _ := cedar.Authorize(f.policy, entities, req)
	return decision == cedar.Allow
}

// repo is the Repository entity for a location or a prefix.
func repo(s string) types.EntityUID {
	return types.NewEntityUID(repositoryType, types.String(s))
}

// boundaryPrefixes returns the path-boundary ancestors of location. For every
// prefix ending immediately before a "/" it emits both forms - "ghcr.io/team"
// and "ghcr.io/team/" - so an allowlist entry matches whether or not it carries
// a trailing slash, exactly as a boundary-aware string prefix would, while a
// sibling namespace or adjacent host (which is not such a prefix) never does.
// The location itself is handled by the reflexivity of Cedar's `in`, so it is
// not included here.
func boundaryPrefixes(location string) []string {
	var out []string
	for i, c := range location {
		if c != '/' {
			continue
		}
		p := location[:i]
		// A leading "/" would produce an empty prefix; a location the module
		// package emits never starts with one, this keeps the function total.
		if p == "" {
			continue
		}
		// A NUL in an entity id is refused upstream (ociLocation/httpLocation);
		// drop the prefix defensively rather than carry it into Cedar.
		if strings.ContainsRune(p, 0) {
			continue
		}
		out = append(out, p, p+"/")
	}
	return out
}
