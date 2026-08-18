// Package authz evaluates function-wasm's declarative admission decisions with
// Cedar (docs/one-pager-policy-engine.md). It owns two kinds of decision over
// one shared schema:
//
//   - The always-on built-in fences a Composition's own policy authorizes: the
//     repository fence (RepositoryFence) for a module the composite resource
//     chooses (module.from), and the credential fence (CredentialFence) for a
//     step credential such a module may spend. Their allow lists travel in the
//     request context, never in the policy text, so a Composition-authored
//     entry cannot inject Cedar policy.
//   - The operator's grant policy (OperatorPolicy), compiled from --sandbox-policy-file
//     and immutable for the process. It decides whether a caller may be granted
//     a sandbox capability (private /tmp, environment, egress). It is a separate
//     PolicySet, AND-combined with the built-in fences and the --enable-sandbox-*
//     floor: it can only tighten, never widen. Absent, it adds no constraint.
//
// Repositories and host patterns are modelled as Cedar entity hierarchies: a
// location's ancestors are its path-boundary prefixes (a repository) or its DNS
// suffixes (a host), so `in` an allowed entity is true exactly when that entity
// equals the location or fences it at a boundary, and never a sibling namespace
// ("ghcr.io/team" vs "ghcr.io/team-evil") or an adjacent host
// ("cdn.example.com" vs "cdn.example.com.attacker.net"). The decision is
// boundary-correct by construction - the property a raw string prefix cannot
// guarantee.
//
// Cedar owns the decision only; callers keep their own refusal strings.
package authz

import (
	_ "embed"
	"fmt"

	"github.com/cedar-policy/cedar-go"
	"github.com/cedar-policy/cedar-go/types"
)

var (
	// pullAction is the action a repository-fence request asks about; it
	// matches the policy's `action == Action::"pullModule"`.
	pullAction = types.NewEntityUID("Action", "pullModule")
	// modulePrincipal is a placeholder: the built-in fences do not discriminate
	// by principal, their policies leave it unconstrained.
	modulePrincipal = types.NewEntityUID("Module", "module")
	// allowedRepositoriesKey names the context set the repository fence reads.
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
	// The resource's ancestors are its path-boundary prefixes, so `resource in
	// Repository::"p"` is true when p equals the location (Cedar's `in` is
	// reflexive) or p fences it at a "/".
	resource, entities := repositoryEntities(location)
	req := cedar.Request{
		Principal: modulePrincipal,
		Action:    pullAction,
		Resource:  resource,
		Context:   types.NewRecord(types.RecordMap{allowedRepositoriesKey: repoSet(allowed)}),
	}
	decision, _ := cedar.Authorize(f.policy, entities, req)
	return decision == cedar.Allow
}
