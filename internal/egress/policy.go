// Package egress performs HTTP requests on behalf of a running module — the
// host side of the wasmfn.http import (docs/abi.md) — within the operator's
// ceiling (--enable-sandbox-egress, --sandbox-egress-policy) narrowed by the
// Composition's sandbox.egress grant. The guest never opens a socket: the
// host resolves the name, refuses addresses on the block list, terminates
// TLS with its own roots, applies the host, method and path rules and the
// budgets, and hands the response back through guest memory.
package egress

import (
	"fmt"
	"net/http"
	"net/netip"
	"os"
	"slices"
	"strings"
	"sync"
	"time"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"sigs.k8s.io/yaml"
)

// Defaults of the Policy budgets, applied for zero fields.
const (
	DefaultTimeout          = 10 * time.Second
	DefaultMaxRequests      = 16
	DefaultMaxResponseBytes = 4 << 20
	DefaultMaxRedirects     = 5
)

// DefaultBlockedCIDRs are refused whatever the grant unless the Policy's
// AllowedCIDRs make an exception: loopback, link-local (the cloud metadata
// endpoint lives there), RFC 1918, carrier-grade NAT (a common pod range),
// IPv6 unique-local, the NAT64 well-known prefix (an IPv4 address written as
// IPv6), and the unspecified, multicast and reserved ranges. A module reaches
// the operator's cluster only where the operator says so.
var DefaultBlockedCIDRs = []string{
	"0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16",
	"172.16.0.0/12", "192.168.0.0/16", "224.0.0.0/4", "240.0.0.0/4",
	"::/128", "::1/128", "64:ff9b::/96", "fc00::/7", "fe80::/10", "ff00::/8",
}

// Policy is the operator's egress ceiling — the --sandbox-egress-policy file
// (YAML or JSON, unknown fields refused). Every field is optional: without a
// file, or for a zero field, the defaults apply, so an enabled runtime
// without a file lets a Composition grant any public host within the default
// budgets.
type Policy struct {
	// Hosts a Composition may grant, exact names. With HostPatterns empty
	// too, any host is grantable.
	Hosts []string `json:"hosts,omitempty"`
	// HostPatterns a Composition may grant: a name with one leading wildcard
	// label, "*.example.com", covering every name under example.com (not
	// example.com itself). A Composition's hostPattern must sit under one of
	// them.
	HostPatterns []string `json:"hostPatterns,omitempty"`
	// BlockedCIDRs are refused whatever the grant, on top of
	// DefaultBlockedCIDRs; a CIDR listed here stays refused even when
	// AllowedCIDRs covers it.
	BlockedCIDRs []string `json:"blockedCIDRs,omitempty"`
	// AllowedCIDRs are exceptions to DefaultBlockedCIDRs — the cluster's
	// service range an operator means modules to reach, say.
	AllowedCIDRs []string `json:"allowedCIDRs,omitempty"`
	// Timeout of one request, from the name lookup to the last body byte,
	// e.g. "10s". The run's own deadline still applies if sooner.
	Timeout metav1.Duration `json:"timeout,omitempty"`
	// MaxRequests one run may make; the rest are refused.
	MaxRequests int `json:"maxRequests,omitempty"`
	// MaxResponseBytes a response body may carry; a longer one is an error,
	// not a truncated body.
	MaxResponseBytes int64 `json:"maxResponseBytes,omitempty"`
	// MaxRedirects followed for one request, each hop checked like the
	// first.
	MaxRedirects int `json:"maxRedirects,omitempty"`
	// RateLimit is a process-wide token bucket per module digest: a module
	// that exceeds it reads a budget error, never a trap.
	RateLimit *RateLimitPolicy `json:"rateLimit,omitempty"`
}

// RateLimitPolicy is a process-wide token bucket per module digest.
type RateLimitPolicy struct {
	// RequestsPerMinute is the sustained rate.
	RequestsPerMinute float64 `json:"requestsPerMinute"`
	// Burst is the maximum tokens available at once.
	Burst int `json:"burst,omitempty"`
}

// LoadPolicy reads a Policy file (YAML or JSON).
func LoadPolicy(path string) (Policy, error) {
	raw, err := os.ReadFile(path) //nolint:gosec // The operator's own flag names the file.
	if err != nil {
		return Policy{}, fmt.Errorf("cannot read egress policy: %w", err)
	}
	var p Policy
	if err := yaml.UnmarshalStrict(raw, &p); err != nil {
		return Policy{}, fmt.Errorf("cannot parse egress policy %s: %w", path, err)
	}
	return p, nil
}

// Egress is the compiled ceiling: what a Composition may grant, which
// addresses are never dialled, and the budgets of every run. One per runtime.
type Egress struct {
	hosts    map[string]bool
	patterns []string // ".example.com" for "*.example.com"
	blocked  []netip.Prefix
	allowed  []netip.Prefix
	explicit []netip.Prefix // the file's blockedCIDRs, which allowed never override
	budget   budget

	rateLimits *rateLimiters // nil when the policy sets no rate limit

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

// New compiles a Policy, applying the defaults for its zero fields.
func New(p Policy) (*Egress, error) {
	e := &Egress{
		hosts: map[string]bool{},
		budget: budget{
			timeout:          DefaultTimeout,
			maxRequests:      DefaultMaxRequests,
			maxResponseBytes: DefaultMaxResponseBytes,
			maxRedirects:     DefaultMaxRedirects,
		},
	}
	for _, h := range p.Hosts {
		if !ValidHost(h) {
			return nil, fmt.Errorf("egress policy: hosts entry %q is not a host name", h)
		}
		h = normalizeHost(h)
		e.hosts[h] = true
	}
	for _, pat := range p.HostPatterns {
		suffix, ok := patternSuffix(pat)
		if !ok {
			return nil, fmt.Errorf("egress policy: hostPatterns entry %q must be a host name with one leading wildcard label, e.g. *.example.com", pat)
		}
		e.patterns = append(e.patterns, suffix)
	}
	var err error
	if e.blocked, err = prefixes(DefaultBlockedCIDRs); err != nil {
		return nil, err
	}
	if e.explicit, err = prefixes(p.BlockedCIDRs); err != nil {
		return nil, fmt.Errorf("egress policy: blockedCIDRs: %w", err)
	}
	if e.allowed, err = prefixes(p.AllowedCIDRs); err != nil {
		return nil, fmt.Errorf("egress policy: allowedCIDRs: %w", err)
	}
	if p.Timeout.Duration < 0 || p.MaxRequests < 0 || p.MaxResponseBytes < 0 || p.MaxRedirects < 0 {
		return nil, fmt.Errorf("egress policy: budgets must not be negative")
	}
	if p.Timeout.Duration > 0 {
		e.budget.timeout = p.Timeout.Duration
	}
	if p.MaxRequests > 0 {
		e.budget.maxRequests = p.MaxRequests
	}
	if p.MaxResponseBytes > 0 {
		e.budget.maxResponseBytes = p.MaxResponseBytes
	}
	if p.MaxRedirects > 0 {
		e.budget.maxRedirects = p.MaxRedirects
	}
	if p.RateLimit != nil {
		if p.RateLimit.RequestsPerMinute <= 0 {
			return nil, fmt.Errorf("egress policy: rateLimit.requestsPerMinute must be positive")
		}
		rl := &rateLimitPolicy{
			requestsPerMinute: p.RateLimit.RequestsPerMinute,
			burst:             p.RateLimit.Burst,
		}
		if rl.burst <= 0 {
			rl.burst = max(1, int(rl.requestsPerMinute))
		}
		e.rateLimits = newRateLimiters(*rl)
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
// dialled. The file's blockedCIDRs win over allowedCIDRs, which win over the
// defaults; an IPv4-mapped IPv6 address is judged as the IPv4 it carries.
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

// admitsHost reports whether the ceiling lets a Composition grant host.
func (e *Egress) admitsHost(host string) bool {
	if len(e.hosts) == 0 && len(e.patterns) == 0 {
		return true
	}
	if e.hosts[host] {
		return true
	}
	for _, suffix := range e.patterns {
		if matchesSuffix(host, suffix) {
			return true
		}
	}
	return false
}

// admitsPattern reports whether the ceiling lets a Composition grant a whole
// pattern: only a ceiling pattern at or above it does — an exact host never
// covers a pattern.
func (e *Egress) admitsPattern(suffix string) bool {
	if len(e.hosts) == 0 && len(e.patterns) == 0 {
		return true
	}
	for _, s := range e.patterns {
		if suffix == s || strings.HasSuffix(suffix, s) {
			return true
		}
	}
	return false
}

// describe lists what the ceiling admits, for a refusal message.
func (e *Egress) describe() string {
	names := make([]string, 0, len(e.hosts)+len(e.patterns))
	for h := range e.hosts {
		names = append(names, h)
	}
	for _, s := range e.patterns {
		names = append(names, "*"+s)
	}
	slices.Sort(names)
	return strings.Join(names, ", ")
}

// Describe names the ceiling for the startup log: the hosts and patterns a
// Composition may grant ("any host" on an open ceiling) and the budgets.
func (e *Egress) Describe() string {
	hosts := e.describe()
	if hosts == "" {
		hosts = "any host"
	}
	b := e.budget
	desc := fmt.Sprintf("%s; timeout %s, maxRequests %d, maxResponseBytes %d, maxRedirects %d", hosts, b.timeout, b.maxRequests, b.maxResponseBytes, b.maxRedirects)
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

// ValidHost reports whether h names one host the way a rule or a policy
// entry must: a bare host name — no scheme, port, path, wildcard, zone or
// whitespace — that is not empty once normalized. "." and " " normalize to
// "" and would otherwise compile into a rule that matches every host.
func ValidHost(h string) bool {
	n := normalizeHost(h)
	return n != "" && !strings.ContainsAny(n, "*/:%@ \t\n[]")
}

// ValidHostPattern reports whether p is a host pattern a rule or a policy
// entry may carry: one leading wildcard label over a valid host.
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

// PatternUnder reports whether pattern sits at or under granted - the rule
// the ceiling applies between a Composition's pattern and the policy's:
// "*.a.example.com" is under "*.example.com", and a pattern is under itself.
// An invalid pattern on either side is not.
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
