package egress

import "net/netip"

// Option configures an Egress at construction. The CIDR options carry the
// operator's Cedar-authored SSRF rules, compiled to prefixes by internal/authz,
// into the ceiling. egress consumes only these stdlib netip prefixes, never
// cedar-go, so the dial hot path (blockedBy) stays a few Prefix.Contains and
// Cedar never runs per resolved IP.
type Option func(*Egress)

// WithBlockedCIDRs adds prefixes an operator forbid rule refuses to the
// ceiling's explicit block list. Like the policy file's blockedCIDRs, they win
// over the allow list and the defaults: an explicit block is never overridden.
func WithBlockedCIDRs(prefixes []netip.Prefix) Option {
	return func(e *Egress) { e.explicit = append(e.explicit, prefixes...) }
}

// WithAllowedCIDRs adds prefixes an operator permit rule admits to the
// ceiling's allow list. Like the policy file's allowedCIDRs, they punch a hole
// in the default block list but never override an explicit block.
func WithAllowedCIDRs(prefixes []netip.Prefix) Option {
	return func(e *Egress) { e.allowed = append(e.allowed, prefixes...) }
}
