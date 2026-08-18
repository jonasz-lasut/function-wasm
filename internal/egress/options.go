package egress

import (
	"net/netip"
	"time"
)

// Option configures an Egress at construction. The CIDR options carry the
// operator's Cedar-authored SSRF rules, compiled to prefixes by internal/authz,
// into the ceiling; the rate-limit option carries the --egress-rate-limit-*
// flags. egress consumes only stdlib values, never cedar-go, so the dial hot
// path (blockedBy) stays a few Prefix.Contains and Cedar never runs per
// resolved IP.
type Option func(*Egress)

// WithBlockedCIDRs adds prefixes an operator forbid rule refuses to the
// ceiling's explicit block list. They win over the allow list and the defaults:
// an explicit block is never overridden.
func WithBlockedCIDRs(prefixes []netip.Prefix) Option {
	return func(e *Egress) { e.explicit = append(e.explicit, prefixes...) }
}

// WithAllowedCIDRs adds prefixes an operator permit rule admits to the
// ceiling's allow list. They punch a hole in the default block list but never
// override an explicit block.
func WithAllowedCIDRs(prefixes []netip.Prefix) Option {
	return func(e *Egress) { e.allowed = append(e.allowed, prefixes...) }
}

// WithRateLimit sets the per-module-digest token bucket from the operator's
// --egress-rate-limit-per-minute / --egress-rate-limit-burst flags: a sustained
// requestsPerMinute with the given burst, or a derived burst of max(1, rpm) when
// burst is not positive. A non-positive requestsPerMinute leaves rate limiting
// off, so New with no rate-limit flag is unlimited.
func WithRateLimit(requestsPerMinute float64, burst int) Option {
	return func(e *Egress) {
		if requestsPerMinute <= 0 {
			return
		}
		if burst <= 0 {
			burst = max(1, int(requestsPerMinute))
		}
		e.rateLimits = newRateLimiters(rateLimitPolicy{requestsPerMinute: requestsPerMinute, burst: burst})
	}
}

// WithBudgets overrides the fixed per-run egress budgets. The budgets are fixed
// defaults for operators in this release - no flag exposes them - but this
// option keeps them injectable for tests and for a future flag. A zero (or
// negative) field keeps the default.
func WithBudgets(timeout time.Duration, maxRequests int, maxResponseBytes int64, maxRedirects int) Option {
	return func(e *Egress) {
		if timeout > 0 {
			e.budget.timeout = timeout
		}
		if maxRequests > 0 {
			e.budget.maxRequests = maxRequests
		}
		if maxResponseBytes > 0 {
			e.budget.maxResponseBytes = maxResponseBytes
		}
		if maxRedirects > 0 {
			e.budget.maxRedirects = maxRedirects
		}
	}
}
