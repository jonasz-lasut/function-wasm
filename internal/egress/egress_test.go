package egress

import (
	"context"
	"errors"
	"net/netip"
	"net/url"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
)

// mustPrefixes parses CIDRs for the option-based tests, mirroring the prefixes
// internal/authz hands New for the operator's Cedar dialAddress rules.
func mustPrefixes(t *testing.T, cidrs ...string) []netip.Prefix {
	t.Helper()
	out := make([]netip.Prefix, 0, len(cidrs))
	for _, c := range cidrs {
		out = append(out, netip.MustParsePrefix(c).Masked())
	}
	return out
}

func TestNew(t *testing.T) {
	// New always compiles: the budgets are fixed defaults and the block list is
	// the default set; the operator's CIDR rules and rate limit arrive as options.
	e, err := New()
	if err != nil {
		t.Fatalf("New(): %v", err)
	}
	want := budget{timeout: DefaultTimeout, maxRequests: DefaultMaxRequests, maxResponseBytes: DefaultMaxResponseBytes, maxRedirects: DefaultMaxRedirects}
	if diff := cmp.Diff(want, e.budget, cmp.AllowUnexported(budget{})); diff != "" {
		t.Errorf("New() budget: -want, +got:\n%s", diff)
	}
	if e.rateLimits != nil {
		t.Error("New() with no rate-limit option should leave egress unrated")
	}
}

func TestBlockedBy(t *testing.T) {
	// The default block list, with no operator CIDR options.
	cases := map[string]struct {
		reason string
		ip     string
		want   string
	}{
		"Public":         {reason: "A public address is not blocked.", ip: "93.184.216.34"},
		"Loopback":       {reason: "Loopback is blocked by default.", ip: "127.0.0.1", want: "127.0.0.0/8"},
		"Metadata":       {reason: "The link-local range holds the cloud metadata endpoint.", ip: "169.254.169.254", want: "169.254.0.0/16"},
		"RFC1918":        {reason: "Private ranges are blocked by default.", ip: "10.1.2.3", want: "10.0.0.0/8"},
		"CGNAT":          {reason: "Carrier-grade NAT (a common pod range) is blocked by default.", ip: "100.64.0.1", want: "100.64.0.0/10"},
		"ULA":            {reason: "IPv6 unique-local is blocked by default.", ip: "fd00::1", want: "fc00::/7"},
		"Mapped":         {reason: "An IPv4-mapped IPv6 address is judged as its IPv4.", ip: "::ffff:10.0.0.1", want: "10.0.0.0/8"},
		"NAT64":          {reason: "The NAT64 prefix carries an IPv4 address.", ip: "64:ff9b::a00:1", want: "64:ff9b::/96"},
		"ZonedLoopback":  {reason: "A zone must not blind the block list: netip's Contains is false for zoned addresses, so the zone is stripped before judging.", ip: "::1%lo0", want: "::/96"},
		"IPv4Compatible": {reason: "The deprecated IPv4-compatible range (::7f00:1 for 127.0.0.1) is blocked so it cannot smuggle a private address past the block list.", ip: "::7f00:1", want: "::/96"},
		"ZonedLinkLocal": {reason: "Link-local addresses usually carry a zone; they stay blocked.", ip: "fe80::1%eth0", want: "fe80::/10"},
		"ZonedULA":       {reason: "A zoned unique-local address stays blocked.", ip: "fd00::1%eth0", want: "fc00::/7"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			e, err := New()
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, e.blockedBy(netip.MustParseAddr(tc.ip))); diff != "" {
				t.Errorf("\n%s\nblockedBy(%s): -want, +got:\n%s", tc.reason, tc.ip, diff)
			}
		})
	}
}

func TestBlockedByWithCIDRRules(t *testing.T) {
	// The operator's Cedar rules, compiled to prefixes by internal/authz; egress
	// consumes only the prefixes, never cedar-go, so the dial path stays Go. A
	// forbid joins the explicit block list (which wins), a permit the allow list.
	cases := map[string]struct {
		reason string
		block  []string
		allow  []string
		ip     string
		want   string
	}{
		"CedarForbid":       {reason: "A Cedar forbid rule blocks an extra range.", block: []string{"203.0.113.0/24"}, ip: "203.0.113.7", want: "203.0.113.0/24"},
		"CedarPermit":       {reason: "A Cedar permit rule opens a hole in the defaults.", allow: []string{"10.96.0.0/12"}, ip: "10.96.0.10"},
		"CedarPermitNarrow": {reason: "The hole is only as wide as the permit lists.", allow: []string{"10.96.0.0/12"}, ip: "10.1.2.3", want: "10.0.0.0/8"},
		"CedarForbidWins":   {reason: "A Cedar forbid wins over a Cedar permit of the same range (forbid-wins).", block: []string{"10.96.0.0/24"}, allow: []string{"10.96.0.0/12"}, ip: "10.96.0.10", want: "10.96.0.0/24"},
		"AllowEverything":   {reason: "An operator can lift the whole default block list.", allow: []string{"0.0.0.0/0", "::/0"}, ip: "127.0.0.1"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			e, err := New(WithBlockedCIDRs(mustPrefixes(t, tc.block...)), WithAllowedCIDRs(mustPrefixes(t, tc.allow...)))
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(tc.want, e.blockedBy(netip.MustParseAddr(tc.ip))); diff != "" {
				t.Errorf("\n%s\nblockedBy(%s): -want, +got:\n%s", tc.reason, tc.ip, diff)
			}
		})
	}
}

func TestGrant(t *testing.T) {
	get := []string{"GET"}
	// Grant no longer intersects a host ceiling (that is Cedar's grantEgress at
	// admission); it compiles the Composition's own rules and rejects a shape
	// that could never match or that matches everything.
	cases := map[string]struct {
		reason string
		rules  []HTTPRule
		want   string
	}{
		"OpenCeiling":    {reason: "Any shape-valid host and pattern compiles; the operator allowlist is Cedar's.", rules: []HTTPRule{{Host: "api.example.com", Methods: get}, {HostPattern: "*.example.org", Methods: get}}},
		"HostDot":        {reason: "A host that normalizes to nothing would compile into a rule matching every host; it is not a host name.", rules: []HTTPRule{{Host: ".", Methods: get}}, want: `requires.egress.http[0].host "." must be a bare host name`},
		"HostSpace":      {reason: "Whitespace normalizes away too.", rules: []HTTPRule{{Host: " ", Methods: get}}, want: `must be a bare host name`},
		"HostPort":       {reason: "A port would make a rule that never matches; a host is a bare name.", rules: []HTTPRule{{Host: "api.example.com:8443", Methods: get}}, want: `requires.egress.http[0].host "api.example.com:8443" must be a bare host name`},
		"HostScheme":     {reason: "So would a scheme.", rules: []HTTPRule{{Host: "https://api.example.com", Methods: get}}, want: `must be a bare host name`},
		"HostZone":       {reason: "A zoned literal names one of the host's interfaces; refused as a rule and by the dialer.", rules: []HTTPRule{{Host: "::1%lo0", Methods: get}}, want: `must be a bare host name`},
		"BadPattern":     {reason: "A pattern has one leading wildcard label.", rules: []HTTPRule{{HostPattern: "api.*.example.com", Methods: get}}, want: `must be a host name with one leading wildcard label`},
		"PathPrefixDots": {reason: "A prefix with dot segments could never match a normalized request path.", rules: []HTTPRule{{Host: "api.example.com", Methods: get, PathPrefix: "/v1/../x"}}, want: `requires.egress.http[0].pathPrefix "/v1/../x" must be normalized`},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			e, err := New()
			if err != nil {
				t.Fatal(err)
			}
			_, err = e.Grant(tc.rules)
			if tc.want == "" {
				if err != nil {
					t.Fatalf("\n%s\nGrant(): unexpected error %v", tc.reason, err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("\n%s\nGrant(): want error containing %q, got %v", tc.reason, tc.want, err)
			}
		})
	}
}

func TestAdmit(t *testing.T) {
	e, err := New()
	if err != nil {
		t.Fatal(err)
	}
	g, err := e.Grant([]HTTPRule{
		{Host: "api.example.com", Methods: []string{"GET", "POST"}, PathPrefix: "/v1/"},
		{Host: "api.example.com", Methods: []string{"DELETE"}, PathPrefix: "/v1/tmp/"},
		{HostPattern: "*.internal.example.com", Methods: []string{"GET"}},
		{Host: "root.internal.example.com", Methods: []string{"GET"}, PathPrefix: "/"},
	})
	if err != nil {
		t.Fatal(err)
	}
	cases := map[string]struct {
		reason string
		method string
		url    string
		want   string
	}{
		"Allowed":         {reason: "A listed method under the prefix passes.", method: "GET", url: "https://api.example.com/v1/items?x=1"},
		"HostCase":        {reason: "Host names compare case-insensitively, a port aside.", method: "post", url: "https://API.example.com:8443/v1/items"},
		"SecondRule":      {reason: "Rules for the same host add up.", method: "DELETE", url: "https://api.example.com/v1/tmp/x"},
		"PatternMatch":    {reason: "A pattern rule admits names under it.", method: "GET", url: "http://a.internal.example.com/anything"},
		"PatternApex":     {reason: "A pattern rule does not admit its apex.", method: "GET", url: "http://internal.example.com/", want: `no rule admits host "internal.example.com"`},
		"UnknownHost":     {reason: "No rule, no request.", method: "GET", url: "https://evil.example.com/", want: `no rule admits host "evil.example.com"`},
		"Method":          {reason: "A method no rule for the host lists is refused.", method: "PUT", url: "https://api.example.com/v1/items", want: `no rule for host "api.example.com" admits PUT /v1/items`},
		"Prefix":          {reason: "A path outside every prefix for the method is refused.", method: "DELETE", url: "https://api.example.com/v1/items", want: `no rule for host "api.example.com" admits DELETE /v1/items`},
		"DotSegments":     {reason: "Dot segments never sneak under a prefix.", method: "GET", url: "https://api.example.com/v1/../admin", want: "must be normalized"},
		"EncodedDots":     {reason: "Percent-encoded dots decode into dot segments and are refused too.", method: "GET", url: "https://api.example.com/v1/%2e%2e/admin", want: "must be normalized"},
		"EmptySegment":    {reason: "Empty segments are not normalized either.", method: "GET", url: "https://api.example.com/v1//items", want: "must be normalized"},
		"TrailingSlashOK": {reason: "A trailing slash is fine.", method: "GET", url: "https://api.example.com/v1/items/"},
		"Scheme":          {reason: "Only http and https.", method: "GET", url: "file:///etc/passwd", want: `only http and https URLs are allowed, not "file"`},
		"NoHost":          {reason: "A URL without a host is refused.", method: "GET", url: "https:///v1/", want: "the URL has no host"},
		"RootPrefixBare":  {reason: "A rule with pathPrefix / admits a URL without a path - the request goes out as / either way.", method: "GET", url: "https://root.internal.example.com"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			u, err := url.Parse(tc.url)
			if err != nil {
				t.Fatal(err)
			}
			err = g.admit(tc.method, u)
			if tc.want == "" {
				if err != nil {
					t.Fatalf("\n%s\nadmit(): unexpected error %v", tc.reason, err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("\n%s\nadmit(): want error containing %q, got %v", tc.reason, tc.want, err)
			}
		})
	}
}

func TestDialRefusesZonedAddresses(t *testing.T) {
	e, err := New(WithAllowedCIDRs(mustPrefixes(t, "0.0.0.0/0", "::/0")))
	if err != nil {
		t.Fatal(err)
	}
	// Even with the whole block list lifted, a zoned literal is not dialled:
	// a zone names one of the host's interfaces, and a URL like
	// http://[::1%25lo]/ used to slip past every prefix check.
	for _, addr := range []string{"[::1%lo0]:80", "[fe80::1%eth0]:80", "[::%x]:80"} {
		_, err := e.dial(context.Background(), "tcp", addr)
		var refused refusedError
		if !errors.As(err, &refused) || !strings.Contains(refused.msg, "zone") {
			t.Errorf("dial(%s): want a zone refusal, got %v", addr, err)
		}
		if refused.detail == "" {
			t.Errorf("dial(%s): the audit line needs the detail, got none (msg %q)", addr, refused.msg)
		}
	}
}

// TestPatternCovers pins the exported pattern helpers a module manifest's
// requirements are checked with: a pattern covers hosts under it, never the
// apex; a pattern is under a pattern at or above it.
func TestPatternCovers(t *testing.T) {
	cases := map[string]struct {
		pattern, host string
		want          bool
	}{
		"Under":        {pattern: "*.example.com", host: "api.example.com", want: true},
		"DeepUnder":    {pattern: "*.example.com", host: "a.b.example.com", want: true},
		"Apex":         {pattern: "*.example.com", host: "example.com", want: false},
		"Other":        {pattern: "*.example.com", host: "api.example.org", want: false},
		"Case":         {pattern: "*.Example.com", host: "API.example.com.", want: true},
		"InvalidPat":   {pattern: "example.com", host: "api.example.com", want: false},
		"LookalikeEnd": {pattern: "*.example.com", host: "notexample.com", want: false},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := PatternCovers(tc.pattern, tc.host); got != tc.want {
				t.Errorf("PatternCovers(%q, %q) = %v, want %v", tc.pattern, tc.host, got, tc.want)
			}
		})
	}
}

func TestPatternUnder(t *testing.T) {
	cases := map[string]struct {
		pattern, granted string
		want             bool
	}{
		"Equal":      {pattern: "*.example.com", granted: "*.example.com", want: true},
		"Under":      {pattern: "*.a.example.com", granted: "*.example.com", want: true},
		"Above":      {pattern: "*.example.com", granted: "*.a.example.com", want: false},
		"Other":      {pattern: "*.example.org", granted: "*.example.com", want: false},
		"HostGrant":  {pattern: "*.example.com", granted: "example.com", want: false},
		"InvalidPat": {pattern: "example.com", granted: "*.example.com", want: false},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if got := PatternUnder(tc.pattern, tc.granted); got != tc.want {
				t.Errorf("PatternUnder(%q, %q) = %v, want %v", tc.pattern, tc.granted, got, tc.want)
			}
		})
	}
}

func TestRateLimit(t *testing.T) {
	cases := map[string]struct {
		reason string
		rpm    float64
		burst  int
		calls  int
		want   int // how many should be allowed
	}{
		"BurstAllowed": {
			reason: "All requests within the burst are allowed.",
			rpm:    60, burst: 5,
			calls: 5,
			want:  5,
		},
		"BurstExceeded": {
			reason: "Requests past the burst are refused.",
			rpm:    60, burst: 3,
			calls: 5,
			want:  3,
		},
		"DefaultBurst": {
			reason: "A zero burst defaults to the rate.",
			rpm:    2, burst: 0,
			calls: 5,
			want:  2,
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			cfg := rateLimitPolicy{requestsPerMinute: tc.rpm, burst: tc.burst}
			if cfg.burst <= 0 {
				cfg.burst = max(1, int(cfg.requestsPerMinute))
			}
			rl := newRateLimiters(cfg)
			got := 0
			for range tc.calls {
				if rl.allow("sha256:abc") {
					got++
				}
			}
			if got != tc.want {
				t.Errorf("\n%s\nallow(): %d of %d allowed, want %d", tc.reason, got, tc.calls, tc.want)
			}
		})
	}
}

func TestRateLimitPerDigest(t *testing.T) {
	cfg := rateLimitPolicy{requestsPerMinute: 60, burst: 2}
	rl := newRateLimiters(cfg)

	// Each digest gets its own bucket.
	if !rl.allow("sha256:aaa") {
		t.Fatal("first call for digest aaa should be allowed")
	}
	if !rl.allow("sha256:aaa") {
		t.Fatal("second call for digest aaa should be allowed")
	}
	if rl.allow("sha256:aaa") {
		t.Fatal("third call for digest aaa should be refused")
	}
	// A different digest is independent.
	if !rl.allow("sha256:bbb") {
		t.Fatal("first call for digest bbb should be allowed")
	}
}

func TestRateLimitSweep(t *testing.T) {
	cfg := rateLimitPolicy{requestsPerMinute: 60, burst: 5}
	rl := newRateLimiters(cfg)
	rl.allow("sha256:stale")

	// Backdate the entry to trigger expiry.
	rl.mu.Lock()
	rl.entries["sha256:stale"].lastSeen = time.Now().Add(-idleExpiry - time.Second)
	rl.mu.Unlock()

	rl.sweep()

	rl.mu.Lock()
	_, exists := rl.entries["sha256:stale"]
	rl.mu.Unlock()
	if exists {
		t.Error("sweep should have removed the stale entry")
	}
}

func TestWithRateLimit(t *testing.T) {
	cases := map[string]struct {
		reason    string
		rpm       float64
		burst     int
		wantNil   bool
		wantBurst int
	}{
		"Set":          {reason: "A positive rate builds the limiter with the given burst.", rpm: 120, burst: 10, wantBurst: 10},
		"DerivedBurst": {reason: "A non-positive burst derives max(1, rpm).", rpm: 120, burst: 0, wantBurst: 120},
		"Off":          {reason: "A non-positive rate leaves egress unrated.", rpm: 0, wantNil: true},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			e, err := New(WithRateLimit(tc.rpm, tc.burst))
			if err != nil {
				t.Fatalf("New(): %v", err)
			}
			if tc.wantNil {
				if e.rateLimits != nil {
					t.Fatalf("\n%s\nWithRateLimit(%v, %v): want no limiter", tc.reason, tc.rpm, tc.burst)
				}
				return
			}
			if e.rateLimits == nil {
				t.Fatalf("\n%s\nWithRateLimit(%v, %v): want a limiter", tc.reason, tc.rpm, tc.burst)
			}
			if e.rateLimits.cfg.requestsPerMinute != tc.rpm {
				t.Errorf("requestsPerMinute = %f, want %f", e.rateLimits.cfg.requestsPerMinute, tc.rpm)
			}
			if e.rateLimits.cfg.burst != tc.wantBurst {
				t.Errorf("burst = %d, want %d", e.rateLimits.cfg.burst, tc.wantBurst)
			}
		})
	}
}

// BenchmarkBlockedBy measures the dial hot-path evaluator, which runs per
// resolved IP, per redirect hop, per reconcile. The design gates moving the
// CIDR rules into Cedar on this staying a few Prefix.Contains with no Cedar and
// no allocation added, so the two sub-benchmarks compare the default block list
// alone with the same list plus operator Cedar CIDR rules compiled to prefixes.
// Both dial a public address, the worst case that scans every list to the end
// and returns "" without allocating, so the numbers isolate the scan the Cedar
// rules lengthen from the string the match names.
func BenchmarkBlockedBy(b *testing.B) {
	public := netip.MustParseAddr("93.184.216.34")
	cases := map[string]struct {
		opts []Option
	}{
		"Defaults": {},
		"WithCedarCIDR": {opts: []Option{
			WithBlockedCIDRs([]netip.Prefix{netip.MustParsePrefix("203.0.113.0/24"), netip.MustParsePrefix("198.51.100.0/24")}),
			WithAllowedCIDRs([]netip.Prefix{netip.MustParsePrefix("10.96.0.0/12")}),
		}},
	}
	for name, tc := range cases {
		e, err := New(tc.opts...)
		if err != nil {
			b.Fatal(err)
		}
		b.Run(name, func(b *testing.B) {
			b.ReportAllocs()
			for b.Loop() {
				_ = e.blockedBy(public)
			}
		})
	}
}
