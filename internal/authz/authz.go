// Package authz evaluates function-wasm's declarative admission decisions with
// Cedar (docs/one-pager-policy-engine.md, docs/one-pager-three-layer-authz.md).
// It owns the two policy layers of the three-layer capability decision, both
// compiled against one shared schema and AND-combined by the callers so
// neither can widen the other:
//
//   - The operator's grant policy (OperatorPolicy), compiled from
//     --sandbox-policy-file and immutable for the process: the ceiling. It
//     decides whether a caller may be granted a sandbox capability (private
//     /tmp, environment, egress, a spent credential), evaluates default-deny,
//     and absent (nil) denies everything - no policy file, only the default
//     sandbox. It also answers the per-repository signature requirement
//     (RequiresSignature) and compiles the SSRF CIDR rules (CompileIPRules).
//   - The composition author's policy (CompositionPolicy), compiled from the
//     Input's compositionPolicy text and cached by content hash. It narrows
//     the sandbox capabilities (scoped default-permit, via ScopesAction) and
//     is the required fence over a module the composite resource chooses
//     (pullModule, spendCredential - default-deny).
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
	"github.com/cedar-policy/cedar-go/types"
)

// pullAction is the action a module-pull decision asks about; it matches a
// policy's `action == Action::"pullModule"`.
var pullAction = types.NewEntityUID("Action", ActionPullModule)

// modulePrincipal is a placeholder for decisions that do not discriminate by
// caller (the per-repository signature requirement): their policies leave the
// principal unconstrained.
var modulePrincipal = types.NewEntityUID("Module", "module")
