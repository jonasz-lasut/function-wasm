package authz

import (
	_ "embed"
	"fmt"
	"os"
	"strings"

	"github.com/cedar-policy/cedar-go"
	"github.com/cedar-policy/cedar-go/types"
)

// The grant-policy actions the operator's document authorizes. pullModule and
// spendCredential are the always-on fences' actions; the rest are the sandbox
// capabilities the operator's grant policy narrows.
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
// and immutable for the process, so it is safe for concurrent use. A nil
// *OperatorPolicy is the no-policy-file case: every Permits method returns true,
// so admission is identical to a runtime started without --sandbox-policy-file. The
// policy can only tighten - it is AND-combined with the --enable-sandbox-*
// floor and the built-in fences, and evaluates default-deny (forbid wins).
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
// granted a private /tmp (action usePrivateTmp). A nil policy permits it.
func (p *OperatorPolicy) PermitsPrivateTmp(principal Principal) bool {
	return p.authorize(principal, usePrivateTmpAction, privateTmpCapability, nil, types.NewRecord(nil))
}

// PermitsEnv reports whether the operator policy lets principal set environment
// variables (action setEnv). keys are the explicit sandbox.env names, offered
// as context.keys for a policy that discriminates by variable name; a bulk
// envFrom import contributes no keys (they are not known until the run), so a
// key-level condition applies to sandbox.env only. A nil policy permits it.
func (p *OperatorPolicy) PermitsEnv(principal Principal, keys []string) bool {
	vals := make([]types.Value, 0, len(keys))
	for _, k := range keys {
		vals = append(vals, types.String(k))
	}
	ctx := types.NewRecord(types.RecordMap{"keys": types.NewSet(vals...)})
	return p.authorize(principal, setEnvAction, envCapability, nil, ctx)
}

// PermitsEgress reports whether the operator policy lets principal be granted
// one egress request (action grantEgress). The resource is the host or pattern
// within the HostPattern hierarchy (so a policy can write `resource in
// HostPattern::"example.com"` or `resource.host like "*.example.com"`), and
// context carries the method and path. A nil policy permits it.
func (p *OperatorPolicy) PermitsEgress(principal Principal, g EgressGrant) bool {
	if p == nil {
		return true
	}
	resource, entities := hostEntities(g)
	ctx := types.NewRecord(types.RecordMap{
		"method": types.String(strings.ToUpper(g.Method)),
		"path":   types.String(g.Path),
	})
	return p.authorize(principal, grantEgressAction, resource, entities, ctx)
}

// authorize evaluates one grant-policy request against the operator's document.
// A nil policy is the no-policy-file case and permits. The resource entity is
// added to the store when the caller did not supply it (a flat Capability), so
// both `resource == ...` and `resource in ...` conditions evaluate.
func (p *OperatorPolicy) authorize(principal Principal, action, resource types.EntityUID, entities types.EntityMap, ctx types.Record) bool {
	if p == nil {
		return true
	}
	if entities == nil {
		entities = types.EntityMap{}
	}
	if _, ok := entities[resource]; !ok {
		entities[resource] = types.Entity{UID: resource}
	}
	entities[principalUID] = principal.entity()
	decision, _ := cedar.Authorize(p.policy, entities, cedar.Request{
		Principal: principalUID,
		Action:    action,
		Resource:  resource,
		Context:   ctx,
	})
	return decision == cedar.Allow
}

//go:embed credential_fence.cedar
var credentialFenceText []byte

// CredentialFence is the always-on built-in fence over a Composition's own
// policy allow lists: a step credential a composite-chosen module names may be
// spent only when the Composition's credentialsAllowList lists it and the
// module's repository is within its repositoryAllowList (co-located, as the
// design's `spendCredential when { credential in allowedCreds && resource in
// allowedRepos }`). Set membership carries no boundary subtlety, so the
// credential half is exact; the repository half reuses the boundary-correct
// Repository hierarchy. The allow lists travel in context, never in the policy
// text. The compiled policy is immutable, so a fence is safe for concurrent use.
type CredentialFence struct {
	policy *cedar.PolicySet
}

// NewCredentialFence compiles the embedded credential fence policy.
func NewCredentialFence() (*CredentialFence, error) {
	ps, err := cedar.NewPolicySetFromBytes("credential_fence.cedar", credentialFenceText)
	if err != nil {
		return nil, fmt.Errorf("cannot compile the credential fence policy: %w", err)
	}
	return &CredentialFence{policy: ps}, nil
}

// Permits reports whether credential may be spent pulling a module at location:
// credential must be in allowedCredentials and location within one of
// allowedRepositories. Empty lists permit nothing.
func (f *CredentialFence) Permits(credential, location string, allowedCredentials, allowedRepositories []string) bool {
	resource := cred(credential)
	// The location's repository entity and its path-boundary ancestors give
	// `context.repository in context.allowedRepositories` its meaning.
	_, entities := repositoryEntities(location)
	entities[resource] = types.Entity{UID: resource}
	ctx := types.NewRecord(types.RecordMap{
		"allowedCredentials":  credSet(allowedCredentials),
		"repository":          repo(location),
		"allowedRepositories": repoSet(allowedRepositories),
	})
	decision, _ := cedar.Authorize(f.policy, entities, cedar.Request{
		Principal: modulePrincipal,
		Action:    spendCredentialAction,
		Resource:  resource,
		Context:   ctx,
	})
	return decision == cedar.Allow
}
