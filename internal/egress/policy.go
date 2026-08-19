// Package egress performs HTTP requests on behalf of a running module - the
// host side of the wasmfn.http import (docs/abi.md) - within the Composition's
// sandbox.egress grant. Egress is enabled, and its host allowlist and CIDR
// block/allow rules are authored, in the operator's Cedar --sandbox-policy-file
// (internal/authz): the host allowlist is the grantEgress decision applied at
// admission, and the CIDR rules compile to
// prefixes passed in as options. The per-run budgets are fixed and the rate
// limit is a flag. The guest never opens a socket: the host resolves the name,
// refuses addresses on the block list, terminates TLS with its own roots,
// applies the Composition's host, method and path rules and the budgets, and
// hands the response back through guest memory.
package egress

import (
	"fmt"
	"net/http"
	"net/netip"
	"strings"
	"sync"
	"time"
)

// Default per-run egress budgets. They are fixed: the declarative parts of the
// egress ceiling (hosts, CIDR rules) moved to Cedar, and the budgets are not
// configurable in this release.
const (
	DefaultTimeout          = 10 * time.Second
	DefaultMaxRequests      = 16
	DefaultMaxResponseBytes = 4 << 20
	DefaultMaxRedirects     = 5
)

// DefaultBlockedCIDRs are refused whatever the grant unless an operator allow
// rule (a Cedar dialAddress permit, WithAllowedCIDRs) makes an exception:
// loopback, link-local (the cloud metadata endpoint lives there), RFC 1918,
// carrier-grade NAT (a common pod range), IPv6 unique-local, the NAT64 and
// IPv4-compatible well-known prefixes (an IPv4 address written as IPv6), and
// the unspecified, IPv6-loopback, multicast and reserved ranges. ::/96 covers
// the deprecated IPv4-compatible range (::7f00:1 for 127.0.0.1) and subsumes
// ::/128 (unspecified) and ::1 (loopback). A module reaches the operator's
// cluster only where the operator says so.
var DefaultBlockedCIDRs = []string{
	"0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16",
	"172.16.0.0/12", "192.168.0.0/16", "224.0.0.0/4", "240.0.0.0/4",
	"::/96", "64:ff9b::/96", "fc00::/7", "fe80::/10", "ff00::/8",
}

// Egress is the compiled ceiling: which addresses are never dialled and the
// budgets of every run. The operator's host allowlist is Cedar's (grantEgress),
// applied at admission, not here. One per runtime.
type Egress struct {
	blocked  []netip.Prefix
	allowed  []netip.Prefix
	explicit []netip.Prefix // operator forbid rules, which allowed never override
	budget   budget

	rateLimits *rateLimiters // nil when no rate limit is set

	transportOnce sync.Once
	rt            *http.Transport
}

type rateLimitPolicy struct {
	requestsPerMinute float64
	burst             int
}

type budget struct {
	timeout          time.Duration
	maxRequests      int
	maxResponseBytes int64
	maxRedirects     int
}

// New compiles the egress ceiling with the fixed default budgets and the
// default block list, then applies options: the operator's Cedar-authored CIDR
// rules (WithBlockedCIDRs/WithAllowedCIDRs) and the rate limit (WithRateLimit),
// so blockedBy incorporates them with no Cedar on the dial path.
func New(opts ...Option) (*Egress, error) {
	e := &Egress{
		budget: budget{
			timeout:          DefaultTimeout,
			maxRequests:      DefaultMaxRequests,
			maxResponseBytes: DefaultMaxResponseBytes,
			maxRedirects:     DefaultMaxRedirects,
		},
	}
	var err error
	if e.blocked, err = prefixes(DefaultBlockedCIDRs); err != nil {
		return nil, err
	}
	for _, opt := range opts {
		opt(e)
	}
	return e, nil
}

// SweepRateLimiters removes rate-limit entries for modules not seen
// recently. Called by the same periodic sweep that trims the caches.
func (e *Egress) SweepRateLimiters() {
	if e.rateLimits != nil {
		e.rateLimits.sweep()
	}
}

func prefixes(cidrs []string) ([]netip.Prefix, error) {
	out := make([]netip.Prefix, 0, len(cidrs))
	for _, c := range cidrs {
		p, err := netip.ParsePrefix(c)
		if err != nil {
			return nil, fmt.Errorf("%q is not a CIDR: %w", c, err)
		}
		out = append(out, p.Masked())
	}
	return out, nil
}

// blockedBy names the block-list entry that refuses ip, or "" when ip may be
// dialled. An operator forbid rule (explicit) wins over an operator permit rule
// (allowed), which wins over the defaults; an IPv4-mapped IPv6 address is judged
// as the IPv4 it carries.
func (e *Egress) blockedBy(ip netip.Addr) string {
	// A zone would make every Contains below answer false; the dialer refuses
	// zoned addresses before asking, this keeps the check honest on its own.
	ip = ip.Unmap().WithZone("")
	for _, p := range e.explicit {
		if p.Contains(ip) {
			return p.String()
		}
	}
	for _, p := range e.allowed {
		if p.Contains(ip) {
			return ""
		}
	}
	for _, p := range e.blocked {
		if p.Contains(ip) {
			return p.String()
		}
	}
	return ""
}

// Describe names the ceiling for the startup log: the fixed budgets, the rate
// limit if set, and how many operator CIDR rules the Cedar policy contributed.
// The host allowlist is Cedar's (grantEgress), decided at admission, not shown
// here.
func (e *Egress) Describe() string {
	b := e.budget
	desc := fmt.Sprintf("timeout %s, maxRequests %d, maxResponseBytes %d, maxRedirects %d", b.timeout, b.maxRequests, b.maxResponseBytes, b.maxRedirects)
	if n := len(e.explicit) + len(e.allowed); n > 0 {
		desc += fmt.Sprintf(", %d operator CIDR rule(s)", n)
	}
	if e.rateLimits != nil {
		rl := e.rateLimits.cfg
		desc += fmt.Sprintf(", rateLimit %.0f req/min burst %d", rl.requestsPerMinute, rl.burst)
	}
	return desc
}

// normalizeHost lowercases a host name and drops a trailing dot, so rules and
// URLs compare the way DNS does.
func normalizeHost(h string) string {
	return strings.TrimSuffix(strings.ToLower(strings.TrimSpace(h)), ".")
}

// ValidHost reports whether h names one host the way a rule must: a bare host
// name - no scheme, port, path, wildcard, zone or whitespace - that is not
// empty once normalized. "." and " " normalize to "" and would otherwise
// compile into a rule that matches every host.
func ValidHost(h string) bool {
	n := normalizeHost(h)
	return n != "" && !strings.ContainsAny(n, "*/:%@ \t\n[]")
}

// ValidHostPattern reports whether p is a host pattern a rule may carry: one
// leading wildcard label over a valid host.
func ValidHostPattern(p string) bool {
	suffix, ok := patternSuffix(p)
	return ok && ValidHost(suffix[1:])
}

// PatternCovers reports whether host is under pattern the way a rule's
// hostPattern admits it: "*.example.com" covers "api.example.com" and
// "a.b.example.com", not "example.com" itself. Both sides are normalized;
// an invalid pattern covers nothing.
func PatternCovers(pattern, host string) bool {
	suffix, ok := patternSuffix(pattern)
	return ok && matchesSuffix(normalizeHost(host), suffix)
}

// PatternUnder reports whether pattern sits at or under granted - the rule a
// module manifest's requires.egress applies between its pattern and the
// Composition's grant: "*.a.example.com" is under "*.example.com", and a
// pattern is under itself. An invalid pattern on either side is not.
func PatternUnder(pattern, granted string) bool {
	suffix, ok := patternSuffix(pattern)
	if !ok {
		return false
	}
	over, ok := patternSuffix(granted)
	if !ok {
		return false
	}
	return suffix == over || strings.HasSuffix(suffix, over)
}

// patternSuffix turns "*.example.com" into ".example.com".
func patternSuffix(pattern string) (string, bool) {
	pattern = normalizeHost(pattern)
	if !strings.HasPrefix(pattern, "*.") || len(pattern) < 3 || strings.ContainsAny(pattern[1:], "*/:%@ \t\n[]") {
		return "", false
	}
	return pattern[1:], true
}

// matchesSuffix reports whether host is under the pattern whose suffix is
// given: "a.example.com" and "a.b.example.com" match ".example.com",
// "example.com" itself does not.
func matchesSuffix(host, suffix string) bool {
	return len(host) > len(suffix) && strings.HasSuffix(host, suffix)
}
