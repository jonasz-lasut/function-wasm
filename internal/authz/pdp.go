package authz

import (
	_ "embed"
	"fmt"
	"os"
	"strings"

	"github.com/cedar-policy/cedar-go"
	"github.com/cedar-policy/cedar-go/types"
)

// The grant-policy actions a policy document authorizes: the sandbox
// capabilities and the credential spend both layers gate.
var (
	grantEgressAction     = types.NewEntityUID("Action", "grantEgress")
	usePrivateTmpAction   = types.NewEntityUID("Action", "usePrivateTmp")
	setEnvAction          = types.NewEntityUID("Action", "setEnv")
	spendCredentialAction = types.NewEntityUID("Action", "spendCredential")
)

// The Capability entities a sandbox-capability request names as its resource.
var (
	privateTmpCapability = types.NewEntityUID(capabilityType, "privateTmp")
	envCapability        = types.NewEntityUID(capabilityType, "env")
)

// principalUID is the single Request principal every grant-policy request
// carries; its attributes describe the caller.
var principalUID = types.NewEntityUID(requestType, "self")

// Principal identifies the caller of a grant-policy decision: the observed
// composite resource's namespace and kind, and the Composition's name. Any
// field may be empty - a bare `function validate` has no XR, and a
// RunFunctionRequest does not carry the Composition's name - in which case a
// policy condition that tests it simply does not match. That is safe because
// the operator policy only narrows: an unmatched permit refuses, it never
// widens.
type Principal struct {
	Namespace   string
	XRKind      string
	Composition string
}

// entity is the Cedar Request entity for a principal. All three attributes are
// always set (empty string when unknown) so a policy referencing one evaluates
// cleanly rather than erroring on a missing attribute.
func (p Principal) entity() types.Entity {
	return types.Entity{
		UID: principalUID,
		Attributes: types.NewRecord(types.RecordMap{
			"namespace":   types.String(p.Namespace),
			"xrKind":      types.String(p.XRKind),
			"composition": types.String(p.Composition),
		}),
	}
}

// EgressGrant is one egress request the operator policy judges: a single host
// or host pattern, one method, and the rule's path prefix. A Composition's
// sandbox.egress.http rule with several methods is judged once per method, so a
// policy can key on context.method.
type EgressGrant struct {
	Host        string
	HostPattern string
	Method      string
	Path        string
}

// OperatorPolicy is the operator's grant policy, compiled from --sandbox-policy-file
// and immutable for the process, so it is safe for concurrent use. It is the sole
// authority that enables a sandbox capability: a nil *OperatorPolicy is the
// no-policy-file case and every sandbox-capability Permits method returns false,
// so a runtime with no --sandbox-policy-file grants nothing but the default
// sandbox. A capability is granted only where a permit matches; the policy
// evaluates default-deny (forbid wins) and is AND-combined with the
// composition layer, which can only narrow it further.
type OperatorPolicy struct {
	policy *cedar.PolicySet
}

// LoadOperatorPolicy reads and compiles a Cedar grant policy from a file. A
// mounted ConfigMap satisfies the path.
func LoadOperatorPolicy(path string) (*OperatorPolicy, error) {
	raw, err := os.ReadFile(path) //nolint:gosec // The operator's own flag names the file.
	if err != nil {
		return nil, fmt.Errorf("cannot read operator policy: %w", err)
	}
	return NewOperatorPolicy(path, raw)
}

// NewOperatorPolicy compiles a Cedar grant policy document, naming it for
// error messages.
func NewOperatorPolicy(name string, doc []byte) (*OperatorPolicy, error) {
	ps, err := cedar.NewPolicySetFromBytes(name, doc)
	if err != nil {
		return nil, fmt.Errorf("cannot compile the operator policy %s: %w", name, err)
	}
	return &OperatorPolicy{policy: ps}, nil
}

// PermitsPrivateTmp reports whether the operator policy lets principal be
// granted a private /tmp (action usePrivateTmp). A nil policy denies it.
func (p *OperatorPolicy) PermitsPrivateTmp(principal Principal) bool {
	return p.authorize(principal, usePrivateTmpAction, privateTmpCapability, nil, types.NewRecord(nil))
}

// HasPrivateTmpRules reports whether the operator policy contains any
// usePrivateTmp rule, so the runtime probes $TMPDIR at startup only when a
// private /tmp can ever be granted: a misconfigured $TMPDIR then stops the
// runtime rather than failing the first request that asks for one. A nil policy
// has none.
func (p *OperatorPolicy) HasPrivateTmpRules() bool {
	if p == nil {
		return false
	}
	for _, pol := range p.policy.All() {
		if policyScopesAction(pol, string(usePrivateTmpAction.ID)) {
			return true
		}
	}
	return false
}

// PermitsEnv reports whether the operator policy lets principal set environment
// variables (action setEnv). keys are the environment variable names asked for,
// offered as context.keys for a policy that discriminates by variable name. A
// nil policy denies it.
func (p *OperatorPolicy) PermitsEnv(principal Principal, keys []string) bool {
	return p.authorize(principal, setEnvAction, envCapability, nil, envKeysContext(keys))
}

// PermitsSpendCredential reports whether the operator policy lets principal
// spend a step credential (action spendCredential) - the operator half of a
// manifest env binding's gate. The resource is the Credential entity. A nil
// policy denies it.
func (p *OperatorPolicy) PermitsSpendCredential(principal Principal, credential string) bool {
	return p.authorize(principal, spendCredentialAction, cred(credential), nil, types.NewRecord(nil))
}

// PermitsEgress reports whether the operator policy lets principal be granted
// one egress request (action grantEgress). The resource is the host or pattern
// within the HostPattern hierarchy (so a policy can write `resource in
// HostPattern::"example.com"` or `resource.host like "*.example.com"`), and
// context carries the method and path. A nil policy denies it.
func (p *OperatorPolicy) PermitsEgress(principal Principal, g EgressGrant) bool {
	if p == nil {
		return false
	}
	resource, entities := hostEntities(g)
	return p.authorize(principal, grantEgressAction, resource, entities, egressContext(g))
}

// authorize evaluates one grant-policy request against the operator's document.
// A nil policy is the no-policy-file case and denies, so a runtime with no
// --sandbox-policy-file grants no sandbox capability.
func (p *OperatorPolicy) authorize(principal Principal, action, resource types.EntityUID, entities types.EntityMap, ctx types.Record) bool {
	if p == nil {
		return false
	}
	return decide(p.policy, principal, action, resource, entities, ctx)
}

// decide evaluates one request against a compiled policy set - the evaluation
// the operator and composition layers share, so a policy means the same in
// both. The resource entity is added to the store when the caller did not
// supply it (a flat Capability or Credential), so both `resource == ...` and
// `resource in ...` conditions evaluate.
func decide(ps *cedar.PolicySet, principal Principal, action, resource types.EntityUID, entities types.EntityMap, ctx types.Record) bool {
	if entities == nil {
		entities = types.EntityMap{}
	}
	if _, ok := entities[resource]; !ok {
		entities[resource] = types.Entity{UID: resource}
	}
	entities[principalUID] = principal.entity()
	decision, _ := cedar.Authorize(ps, entities, cedar.Request{
		Principal: principalUID,
		Action:    action,
		Resource:  resource,
		Context:   ctx,
	})
	return decision == cedar.Allow
}

// envKeysContext is the setEnv context: the variable names as context.keys.
func envKeysContext(keys []string) types.Record {
	vals := make([]types.Value, 0, len(keys))
	for _, k := range keys {
		vals = append(vals, types.String(k))
	}
	return types.NewRecord(types.RecordMap{"keys": types.NewSet(vals...)})
}

// egressContext is the grantEgress context: the method and the rule's path.
func egressContext(g EgressGrant) types.Record {
	return types.NewRecord(types.RecordMap{
		"method": types.String(strings.ToUpper(g.Method)),
		"path":   types.String(g.Path),
	})
}
