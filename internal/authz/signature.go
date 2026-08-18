package authz

import (
	"github.com/cedar-policy/cedar-go"
	"github.com/cedar-policy/cedar-go/types"
)

// requireSignatureAction is the action a per-repository signature requirement
// asks about. A policy that permits it for a repository requires a cosign
// signature on every module pulled from within that repository. The decision is
// pre-crypto: Cedar chooses whether to demand a signature, the cosign verifier
// in internal/module performs it (docs/one-pager-policy-engine.md, the
// per-repository signature requirement).
var requireSignatureAction = types.NewEntityUID("Action", "requireSignature")

// RequiresSignature reports whether the operator policy demands a cosign
// signature for a module at location - the normalized location the module
// package produces (registry/repository for OCI, scheme://host/path for HTTP).
// It reuses the boundary-correct Repository hierarchy the repository fence uses,
// so a policy's `resource in Repository::"p"` matches location exactly or where
// p fences it at a "/", and never a sibling namespace ("ghcr.io/team" vs
// "ghcr.io/team-evil") or an adjacent host.
//
// Unlike the sandbox-capability decisions, a nil policy returns false: absent a
// --policy-file the per-repository requirement adds nothing, so the caller keeps
// today's all-or-nothing --cosign-key behaviour unchanged. The requirement is
// caller-independent - a signed module is trusted whoever pulls it - so the
// request principal is a placeholder, as the built-in fences leave it.
func (p *OperatorPolicy) RequiresSignature(location string) bool {
	if p == nil {
		return false
	}
	// The resource's ancestors are its path-boundary prefixes, so `resource in
	// Repository::"p"` is true when p equals the location (Cedar's `in` is
	// reflexive) or p fences it at a "/".
	resource, entities := repositoryEntities(location)
	decision, _ := cedar.Authorize(p.policy, entities, cedar.Request{
		Principal: modulePrincipal,
		Action:    requireSignatureAction,
		Resource:  resource,
		Context:   types.NewRecord(nil),
	})
	return decision == cedar.Allow
}
