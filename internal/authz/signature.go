package authz

import (
	"encoding/json"

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
// --sandbox-policy-file the per-repository requirement adds nothing, so the caller keeps
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

// HasSignatureRules reports whether the operator policy contains any
// requireSignature rule. When --cosign-key is set but this is false, the
// per-repository requirement names no repository - so no module is required to
// be signed - and the caller warns loudly rather than let --cosign-key's
// all-or-nothing lapse silently. A nil policy has none.
func (p *OperatorPolicy) HasSignatureRules() bool {
	if p == nil {
		return false
	}
	for _, pol := range p.policy.All() {
		if policyScopesAction(pol, string(requireSignatureAction.ID)) {
			return true
		}
	}
	return false
}

// policyScopesAction reports whether a policy's action scope names id, in either
// the `== Action::"id"` or `in [..., Action::"id", ...]` form. The Cedar JSON
// policy format is the stable, documented representation (as the CIDR compiler
// also relies on); only the action scope is decoded here.
func policyScopesAction(pol *cedar.Policy, id string) bool {
	raw, err := pol.MarshalJSON()
	if err != nil {
		return false
	}
	var jp struct {
		Action struct {
			Op     string `json:"op"`
			Entity *struct {
				Type string `json:"type"`
				ID   string `json:"id"`
			} `json:"entity"`
			Entities []struct {
				Type string `json:"type"`
				ID   string `json:"id"`
			} `json:"entities"`
		} `json:"action"`
	}
	if json.Unmarshal(raw, &jp) != nil {
		return false
	}
	if jp.Action.Entity != nil && jp.Action.Entity.Type == "Action" && jp.Action.Entity.ID == id {
		return true
	}
	for _, e := range jp.Action.Entities {
		if e.Type == "Action" && e.ID == id {
			return true
		}
	}
	return false
}
