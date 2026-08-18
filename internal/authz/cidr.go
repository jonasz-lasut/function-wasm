package authz

import (
	"encoding/json"
	"fmt"
	"maps"
	"net/netip"
	"sort"

	"github.com/cedar-policy/cedar-go"
	"github.com/cedar-policy/cedar-go/types"
)

// dialActionID is the Cedar action an SSRF CIDR rule is scoped to. A policy
// written `forbid(principal, action == Action::"dialAddress", resource) when {
// context.ip.isInRange(ip("10.0.0.0/8")) };` authors a block rule; a `permit`
// authors an allow rule (a hole in the runtime's default block list). The
// dialed address is context.ip because .isInRange operates on an IP value, not
// on an entity, and the dial has no principal or resource identity to key on.
const dialActionID = "dialAddress"

// IPRules is the SSRF CIDR decision table compiled from the operator's Cedar
// policy: the prefixes a forbid rule refuses and the prefixes a permit rule
// admits, in the exact shape internal/egress evaluates on the dial hot path.
// It carries only stdlib netip values, so egress consumes it without importing
// cedar-go and Cedar never runs per resolved IP - the rules are authored in
// Cedar and compiled here once at load, then a few Prefix.Contains decide every
// dial.
type IPRules struct {
	// Blocked are the prefixes an operator forbid rule refuses. They win over
	// Allowed, mirroring Cedar's forbid-wins precedence, exactly as the egress
	// policy file's blockedCIDRs win over its allowedCIDRs.
	Blocked []netip.Prefix
	// Allowed are the prefixes an operator permit rule admits: a hole punched
	// in the runtime's default block list, exactly as the egress policy file's
	// allowedCIDRs.
	Allowed []netip.Prefix
}

// CompileIPRules compiles the operator policy's Action::"dialAddress" rules into
// the SSRF decision table egress evaluates. Every policy scoped to that action
// contributes prefixes: a forbid to Blocked, a permit to Allowed, so a rule
// authored once in Cedar decides the dial with no Cedar on the hot path.
// Policies scoped to any other action (the grant policy) are left to the PDP
// and ignored here. A nil policy has no rules.
//
// The dial path is security-critical, so an unrecognized ip-rule shape is a
// load error rather than a silently mis-compiled table: a dialAddress policy
// must not constrain the principal or resource (the dial has neither), may use
// at most one when condition, and that condition must be an ip test over
// context.ip - isInRange(ip("CIDR")), isLoopback(), or a || of those. Anything
// else fails closed at startup, where function validate surfaces it too.
func (p *OperatorPolicy) CompileIPRules() (IPRules, error) {
	var out IPRules
	if p == nil {
		return out, nil
	}
	policies := maps.Collect(p.policy.All())
	ids := make([]cedar.PolicyID, 0, len(policies))
	for id := range policies {
		ids = append(ids, id)
	}
	// Sort so a file with more than one malformed rule always reports the same
	// one, keeping the load error deterministic.
	sort.Slice(ids, func(i, j int) bool { return string(ids[i]) < string(ids[j]) })
	for _, id := range ids {
		prefixes, effect, isIPRule, err := compileDialRule(policies[id])
		if err != nil {
			return IPRules{}, fmt.Errorf("operator policy: dialAddress rule %q: %w", string(id), err)
		}
		if !isIPRule {
			continue
		}
		if effect == cedar.Forbid {
			out.Blocked = append(out.Blocked, prefixes...)
		} else {
			out.Allowed = append(out.Allowed, prefixes...)
		}
	}
	return out, nil
}

// The Cedar JSON policy format (stable across cedar-go versions, the
// cross-language interchange shape) is walked rather than the internal AST: it
// is the documented representation and needs no dependency on cedar-go's
// evolving node types. Only the fields an ip rule uses are decoded.
type cedarJSONPolicy struct {
	Principal  cedarJSONScope       `json:"principal"`
	Action     cedarJSONScope       `json:"action"`
	Resource   cedarJSONScope       `json:"resource"`
	Conditions []cedarJSONCondition `json:"conditions"`
}

type cedarJSONScope struct {
	Op       string            `json:"op"`
	Entity   *cedarJSONEntity  `json:"entity,omitempty"`
	Entities []cedarJSONEntity `json:"entities,omitempty"`
}

type cedarJSONEntity struct {
	Type string `json:"type"`
	ID   string `json:"id"`
}

type cedarJSONCondition struct {
	Kind string          `json:"kind"`
	Body json.RawMessage `json:"body"`
}

// compileDialRule reports whether pol is an Action::"dialAddress" rule and, if
// so, the prefixes its condition names and its effect. A policy for another
// action returns isIPRule false with no error; a dialAddress policy with an
// unrecognized shape returns an error.
func compileDialRule(pol *cedar.Policy) (prefixes []netip.Prefix, effect cedar.Effect, isIPRule bool, err error) {
	raw, err := pol.MarshalJSON()
	if err != nil {
		return nil, false, false, fmt.Errorf("cannot read policy: %w", err)
	}
	var jp cedarJSONPolicy
	if err := json.Unmarshal(raw, &jp); err != nil {
		return nil, false, false, fmt.Errorf("cannot read policy: %w", err)
	}
	if !isDialActionScope(jp.Action) {
		// A dialAddress rule written `action in [Action::"dialAddress"]` would
		// otherwise be silently skipped and compile to nothing - a fail-open
		// no-op for a forbid meant as a block. Refuse it so the operator uses
		// the one form the compiler recognizes.
		if dialActionInScope(jp.Action) {
			return nil, false, false, fmt.Errorf(`must scope the action as == Action::"dialAddress", not an "in" set`)
		}
		return nil, false, false, nil
	}
	if jp.Principal.Op != "All" || jp.Resource.Op != "All" {
		return nil, false, false, fmt.Errorf("must not constrain the principal or resource: the dial has neither identity")
	}
	prefixes, err = conditionPrefixes(jp.Conditions)
	if err != nil {
		return nil, false, false, err
	}
	return prefixes, pol.Effect(), true, nil
}

// isDialActionScope reports whether an action scope is exactly `== Action::
// "dialAddress"`. An `in` set is not matched: an ip rule states one action so
// its shape is unambiguous.
func dialActionInScope(s cedarJSONScope) bool {
	if s.Op != "in" {
		return false
	}
	for _, e := range s.Entities {
		if e.Type == "Action" && e.ID == dialActionID {
			return true
		}
	}
	return false
}

func isDialActionScope(s cedarJSONScope) bool {
	return s.Op == "==" && s.Entity != nil && s.Entity.Type == "Action" && s.Entity.ID == dialActionID
}

// conditionPrefixes turns a dialAddress rule's conditions into the prefixes it
// names. A scope-only rule (no condition) applies to every address, so a bare
// `forbid(... dialAddress ...)` blocks the whole space and a `permit` opens it.
func conditionPrefixes(conds []cedarJSONCondition) ([]netip.Prefix, error) {
	if len(conds) == 0 {
		return []netip.Prefix{netip.MustParsePrefix("0.0.0.0/0"), netip.MustParsePrefix("::/0")}, nil
	}
	if len(conds) != 1 {
		return nil, fmt.Errorf("takes at most one when condition")
	}
	if conds[0].Kind != "when" {
		return nil, fmt.Errorf("may only use a when condition, not %s", conds[0].Kind)
	}
	return ipTestPrefixes(conds[0].Body)
}

// ipTestPrefixes walks an ip-test expression: context.ip.isInRange(ip("CIDR")),
// context.ip.isLoopback(), or a || of those. Any other node is refused so the
// compiled table cannot silently mean less than the operator wrote.
func ipTestPrefixes(body json.RawMessage) ([]netip.Prefix, error) {
	var node map[string]json.RawMessage
	if err := json.Unmarshal(body, &node); err != nil {
		return nil, fmt.Errorf("cannot read the condition: %w", err)
	}
	if len(node) != 1 {
		return nil, fmt.Errorf("the condition must be a single ip test")
	}
	for op, arg := range node {
		switch op {
		case "||":
			var branch struct {
				Left  json.RawMessage `json:"left"`
				Right json.RawMessage `json:"right"`
			}
			if err := json.Unmarshal(arg, &branch); err != nil {
				return nil, fmt.Errorf("cannot read the || condition: %w", err)
			}
			left, err := ipTestPrefixes(branch.Left)
			if err != nil {
				return nil, err
			}
			right, err := ipTestPrefixes(branch.Right)
			if err != nil {
				return nil, err
			}
			return append(left, right...), nil
		case "isInRange":
			var args []json.RawMessage
			if err := json.Unmarshal(arg, &args); err != nil || len(args) != 2 {
				return nil, fmt.Errorf("isInRange takes context.ip and one ip(\"CIDR\") literal")
			}
			if err := requireContextIP(args[0]); err != nil {
				return nil, err
			}
			prefix, err := ipLiteralPrefix(args[1])
			if err != nil {
				return nil, err
			}
			return []netip.Prefix{prefix}, nil
		case "isLoopback":
			var args []json.RawMessage
			if err := json.Unmarshal(arg, &args); err != nil || len(args) != 1 {
				return nil, fmt.Errorf("isLoopback takes context.ip and no argument")
			}
			if err := requireContextIP(args[0]); err != nil {
				return nil, err
			}
			// Cedar's isLoopback is 127.0.0.0/8 for IPv4 and ::1 for IPv6.
			return []netip.Prefix{netip.MustParsePrefix("127.0.0.0/8"), netip.MustParsePrefix("::1/128")}, nil
		default:
			return nil, fmt.Errorf("unsupported operation %q (an ip test uses isInRange, isLoopback or ||)", op)
		}
	}
	return nil, fmt.Errorf("the condition must be a single ip test")
}

// requireContextIP refuses any ip test whose subject is not context.ip: the
// dialed address is the only value the dial supplies, so a rule over anything
// else could not be honoured by the decision table.
func requireContextIP(raw json.RawMessage) error {
	var access struct {
		Get *struct {
			Left struct {
				Var string `json:"Var"`
			} `json:"left"`
			Attr string `json:"attr"`
		} `json:"."`
	}
	if err := json.Unmarshal(raw, &access); err != nil || access.Get == nil || access.Get.Left.Var != "context" || access.Get.Attr != "ip" {
		return fmt.Errorf("an ip test may only test context.ip")
	}
	return nil
}

// ipLiteralPrefix reads the ip("CIDR") literal of an isInRange call and parses
// it the way Cedar does: a bare address becomes a host prefix (/32 or /128), a
// CIDR its network. The prefix is masked so it matches internal/egress's own
// compiled block list.
func ipLiteralPrefix(raw json.RawMessage) (netip.Prefix, error) {
	var lit struct {
		IP []struct {
			Value string `json:"Value"`
		} `json:"ip"`
	}
	if err := json.Unmarshal(raw, &lit); err != nil || len(lit.IP) != 1 {
		return netip.Prefix{}, fmt.Errorf("isInRange takes one ip(\"CIDR\") literal")
	}
	addr, err := types.ParseIPAddr(lit.IP[0].Value)
	if err != nil {
		return netip.Prefix{}, fmt.Errorf("%q is not an ip literal: %w", lit.IP[0].Value, err)
	}
	return addr.Prefix().Masked(), nil
}
