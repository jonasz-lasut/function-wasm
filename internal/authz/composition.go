package authz

import (
	"crypto/sha256"
	"fmt"
	"maps"
	"sync"

	"github.com/cedar-policy/cedar-go"
	"github.com/cedar-policy/cedar-go/types"
)

// The action names of the shared schema, as a policy's action scope spells
// them. ScopesAction takes one of these; the composition layer's defaults key
// on them (docs/one-pager-three-layer-authz.md).
const (
	ActionPullModule      = "pullModule"
	ActionSpendCredential = "spendCredential"
	ActionGrantEgress     = "grantEgress"
	ActionUsePrivateTmp   = "usePrivateTmp"
	ActionSetEnv          = "setEnv"
)

// compositionActions are the actions a composition policy can scope; computed
// once at compile so ScopesAction is a map read.
var compositionActions = []string{ActionPullModule, ActionSpendCredential, ActionGrantEgress, ActionUsePrivateTmp, ActionSetEnv}

// CompositionPolicy is the composition author's own Cedar layer, compiled from
// the Input's compositionPolicy text against the same schema as the operator's
// policy. It is one of the three AND-combined layers of a capability decision
// (docs/one-pager-three-layer-authz.md), so it can only narrow: a Permits
// method reports whether a permit matches (default-deny, forbid wins), and the
// per-capability defaults - scoped default-permit for the sandbox actions, a
// required fence for a composite-chosen source - are the caller's, built from
// ScopesAction. Immutable once compiled, so it is safe for concurrent use; a
// nil *CompositionPolicy is the absent layer and every Permits method denies.
type CompositionPolicy struct {
	policy *cedar.PolicySet
	// scoped records which actions any of the policy's rules scope (the
	// `== Action::"x"` or `in [..., Action::"x", ...]` forms), the signal the
	// scoped-default-permit reads: an author who wrote any rule for an action
	// has opted into narrowing it.
	scoped map[string]bool
}

// compositionCache keeps compiled composition policies by content hash: the
// text is per-Composition, not per-request, so each distinct policy compiles
// once per process. Failures are cached too - a malformed policy reconciling in
// a loop should not re-parse every request. Entries are bounded by dropping the
// whole map at the cap: recompiles are cheap and hundreds of distinct policies
// on one runtime is churn, not steady state.
var compositionCache = struct {
	sync.Mutex
	entries map[[sha256.Size]byte]compositionEntry
}{entries: map[[sha256.Size]byte]compositionEntry{}}

const compositionCacheCap = 512

type compositionEntry struct {
	policy *CompositionPolicy
	err    error
}

// CompileCompositionPolicy compiles an Input's compositionPolicy text, cached
// by content hash. Empty text is the absent layer: a nil policy and no error.
func CompileCompositionPolicy(text string) (*CompositionPolicy, error) {
	if text == "" {
		return nil, nil
	}
	key := sha256.Sum256([]byte(text))
	compositionCache.Lock()
	defer compositionCache.Unlock()
	if e, ok := compositionCache.entries[key]; ok {
		return e.policy, e.err
	}
	p, err := NewCompositionPolicy([]byte(text))
	if len(compositionCache.entries) >= compositionCacheCap {
		compositionCache.entries = map[[sha256.Size]byte]compositionEntry{}
	}
	compositionCache.entries[key] = compositionEntry{policy: p, err: err}
	return p, err
}

// NewCompositionPolicy compiles a composition policy document, uncached.
func NewCompositionPolicy(doc []byte) (*CompositionPolicy, error) {
	ps, err := cedar.NewPolicySetFromBytes("compositionPolicy", doc)
	if err != nil {
		return nil, fmt.Errorf("cannot compile the compositionPolicy as Cedar: %w", err)
	}
	scoped := make(map[string]bool, len(compositionActions))
	for _, pol := range ps.All() {
		for _, action := range compositionActions {
			if !scoped[action] && policyScopesAction(pol, action) {
				scoped[action] = true
			}
		}
	}
	return &CompositionPolicy{policy: ps, scoped: scoped}, nil
}

// ScopesAction reports whether any of the policy's rules scope action (one of
// the Action* constants) - the composition layer's opt-in signal: an action no
// rule scopes is not narrowed by this layer. A nil policy scopes nothing.
func (p *CompositionPolicy) ScopesAction(action string) bool {
	return p != nil && p.scoped[action]
}

// PermitsPrivateTmp reports whether a permit matches usePrivateTmp for
// principal. A nil policy denies; the scoped-default-permit for an absent or
// non-scoping policy is the caller's, via ScopesAction.
func (p *CompositionPolicy) PermitsPrivateTmp(principal Principal) bool {
	if p == nil {
		return false
	}
	return decide(p.policy, principal, usePrivateTmpAction, privateTmpCapability, nil, types.NewRecord(nil))
}

// PermitsEnv reports whether a permit matches setEnv for principal. keys are
// the environment variable names asked for, offered as context.keys. A nil
// policy denies.
func (p *CompositionPolicy) PermitsEnv(principal Principal, keys []string) bool {
	if p == nil {
		return false
	}
	return decide(p.policy, principal, setEnvAction, envCapability, nil, envKeysContext(keys))
}

// PermitsEgress reports whether a permit matches grantEgress for one egress
// request, over the same HostPattern hierarchy and method/path context as the
// operator's policy. A nil policy denies.
func (p *CompositionPolicy) PermitsEgress(principal Principal, g EgressGrant) bool {
	if p == nil {
		return false
	}
	resource, entities := hostEntities(g)
	return decide(p.policy, principal, grantEgressAction, resource, entities, egressContext(g))
}

// PermitsPullModule reports whether a permit matches pullModule for a module at
// location - the normalized location the module package produces. The resource
// is the location within the boundary-correct Repository hierarchy, so a policy
// written `resource in Repository::"ghcr.io/team"` admits the location equal to
// the prefix or fenced by it at a "/", never a sibling namespace or an adjacent
// host. A nil policy denies: the fence a composite-chosen source requires.
func (p *CompositionPolicy) PermitsPullModule(principal Principal, location string) bool {
	if p == nil {
		return false
	}
	resource, entities := repositoryEntities(location)
	return decide(p.policy, principal, pullAction, resource, entities, types.NewRecord(nil))
}

// PermitsSpendCredential reports whether a permit matches spendCredential for a
// step credential. The resource is the Credential entity; location, when given
// (a composite-chosen source's repository), travels as context.repository with
// its boundary hierarchy, so a policy can co-locate both halves as the old
// credential fence did: `when { context.repository in Repository::"..." }`. A
// credential spent with no repository (a manifest env binding) carries no
// context.repository, so a policy conditioning on it does not match. A nil
// policy denies.
func (p *CompositionPolicy) PermitsSpendCredential(principal Principal, credential, location string) bool {
	if p == nil {
		return false
	}
	resource := cred(credential)
	entities := types.EntityMap{resource: {UID: resource}}
	ctx := types.RecordMap{}
	if location != "" {
		_, repoEntities := repositoryEntities(location)
		maps.Copy(entities, repoEntities)
		ctx["repository"] = repo(location)
	}
	return decide(p.policy, principal, spendCredentialAction, resource, entities, types.NewRecord(ctx))
}
