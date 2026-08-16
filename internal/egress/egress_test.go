package egress

import (
	"context"
	"errors"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

func TestLoadPolicy(t *testing.T) {
	type want struct {
		policy Policy
		err    string
	}
	cases := map[string]struct {
		reason string
		file   string
		want   want
	}{
		"YAML": {
			reason: "A YAML policy decodes with its budgets.",
			file: `hosts: [api.example.com]
hostPatterns: ["*.googleapis.com"]
blockedCIDRs: ["203.0.113.0/24"]
allowedCIDRs: ["10.96.0.0/12"]
timeout: 5s
maxRequests: 4
maxResponseBytes: 1024
maxRedirects: 1
`,
			want: want{policy: Policy{
				Hosts: []string{"api.example.com"}, HostPatterns: []string{"*.googleapis.com"},
				BlockedCIDRs: []string{"203.0.113.0/24"}, AllowedCIDRs: []string{"10.96.0.0/12"},
				Timeout: metav1.Duration{Duration: 5 * time.Second}, MaxRequests: 4, MaxResponseBytes: 1024, MaxRedirects: 1,
			}},
		},
		"JSON": {
			reason: "JSON is YAML.",
			file:   `{"hosts":["api.example.com"],"timeout":"1m"}`,
			want:   want{policy: Policy{Hosts: []string{"api.example.com"}, Timeout: metav1.Duration{Duration: time.Minute}}},
		},
		"Empty": {
			reason: "An empty file is the default policy.",
			file:   "",
			want:   want{policy: Policy{}},
		},
		"UnknownField": {
			reason: "A misspelled field is an error, not silently ignored: it is a security setting.",
			file:   "host: [api.example.com]\n",
			want:   want{err: `unknown field "host"`},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), "policy.yaml")
			if err := os.WriteFile(path, []byte(tc.file), 0o600); err != nil {
				t.Fatal(err)
			}
			got, err := LoadPolicy(path)
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nLoadPolicy(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nLoadPolicy(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.policy, got); diff != "" {
				t.Errorf("\n%s\nLoadPolicy(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
	if _, err := LoadPolicy(filepath.Join(t.TempDir(), "missing.yaml")); err == nil {
		t.Error("LoadPolicy() of a missing file must fail")
	}
}

func TestNew(t *testing.T) {
	cases := map[string]struct {
		reason string
		policy Policy
		want   string
	}{
		"Defaults":       {reason: "An empty policy compiles to the defaults."},
		"BadHost":        {reason: "A host entry is a name.", policy: Policy{Hosts: []string{"*.example.com"}}, want: `hosts entry "*.example.com" is not a host name`},
		"BadPattern":     {reason: "A pattern has one leading wildcard label.", policy: Policy{HostPatterns: []string{"api.*.example.com"}}, want: `hostPatterns entry "api.*.example.com" must be a host name with one leading wildcard label`},
		"BadCIDR":        {reason: "CIDRs are CIDRs.", policy: Policy{BlockedCIDRs: []string{"10.0.0.0"}}, want: `blockedCIDRs: "10.0.0.0" is not a CIDR`},
		"NegativeBudget": {reason: "Budgets are not negative.", policy: Policy{MaxRequests: -1}, want: "budgets must not be negative"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			e, err := New(tc.policy)
			if tc.want != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want) {
					t.Fatalf("\n%s\nNew(): want error containing %q, got %v", tc.reason, tc.want, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nNew(): unexpected error %v", tc.reason, err)
			}
			want := budget{timeout: DefaultTimeout, maxRequests: DefaultMaxRequests, maxResponseBytes: DefaultMaxResponseBytes, maxRedirects: DefaultMaxRedirects}
			if diff := cmp.Diff(want, e.budget, cmp.AllowUnexported(budget{})); diff != "" {
				t.Errorf("\n%s\nNew() budget: -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

func TestBlockedBy(t *testing.T) {
	cases := map[string]struct {
		reason string
		policy Policy
		ip     string
		want   string
	}{
		"Public":          {reason: "A public address is not blocked.", ip: "93.184.216.34"},
		"Loopback":        {reason: "Loopback is blocked by default.", ip: "127.0.0.1", want: "127.0.0.0/8"},
		"Metadata":        {reason: "The link-local range holds the cloud metadata endpoint.", ip: "169.254.169.254", want: "169.254.0.0/16"},
		"RFC1918":         {reason: "Private ranges are blocked by default.", ip: "10.1.2.3", want: "10.0.0.0/8"},
		"CGNAT":           {reason: "Carrier-grade NAT (a common pod range) is blocked by default.", ip: "100.64.0.1", want: "100.64.0.0/10"},
		"ULA":             {reason: "IPv6 unique-local is blocked by default.", ip: "fd00::1", want: "fc00::/7"},
		"Mapped":          {reason: "An IPv4-mapped IPv6 address is judged as its IPv4.", ip: "::ffff:10.0.0.1", want: "10.0.0.0/8"},
		"NAT64":           {reason: "The NAT64 prefix carries an IPv4 address.", ip: "64:ff9b::a00:1", want: "64:ff9b::/96"},
		"Allowed":         {reason: "allowedCIDRs punch a hole in the defaults.", policy: Policy{AllowedCIDRs: []string{"10.96.0.0/12"}}, ip: "10.96.0.10"},
		"AllowedNarrow":   {reason: "The hole is only as wide as listed.", policy: Policy{AllowedCIDRs: []string{"10.96.0.0/12"}}, ip: "10.1.2.3", want: "10.0.0.0/8"},
		"ExplicitBlock":   {reason: "The file's blockedCIDRs add to the defaults.", policy: Policy{BlockedCIDRs: []string{"203.0.113.0/24"}}, ip: "203.0.113.7", want: "203.0.113.0/24"},
		"ExplicitWins":    {reason: "A CIDR the file blocks stays blocked whatever allowedCIDRs says.", policy: Policy{BlockedCIDRs: []string{"10.96.0.0/24"}, AllowedCIDRs: []string{"10.96.0.0/12"}}, ip: "10.96.0.10", want: "10.96.0.0/24"},
		"AllowEverything": {reason: "An operator can lift the whole default list.", policy: Policy{AllowedCIDRs: []string{"0.0.0.0/0", "::/0"}}, ip: "127.0.0.1"},
		"ZonedLoopback":   {reason: "A zone must not blind the block list: netip's Contains is false for zoned addresses, so the zone is stripped before judging.", ip: "::1%lo0", want: "::1/128"},
		"ZonedLinkLocal":  {reason: "Link-local addresses usually carry a zone; they stay blocked.", ip: "fe80::1%eth0", want: "fe80::/10"},
		"ZonedULA":        {reason: "A zoned unique-local address stays blocked.", ip: "fd00::1%eth0", want: "fc00::/7"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			e, err := New(tc.policy)
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
	cases := map[string]struct {
		reason string
		policy Policy
		rules  []v1beta1.SandboxHTTPRule
		want   string
	}{
		"OpenCeiling":         {reason: "Without hosts or patterns in the policy any host and pattern is grantable.", rules: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: get}, {HostPattern: "*.example.org", Methods: get}}},
		"HostListed":          {reason: "A host the ceiling lists is grantable.", policy: Policy{Hosts: []string{"API.example.com."}}, rules: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: get}}},
		"HostUnderPattern":    {reason: "A host under a ceiling pattern is grantable.", policy: Policy{HostPatterns: []string{"*.example.com"}}, rules: []v1beta1.SandboxHTTPRule{{Host: "a.b.example.com", Methods: get}}},
		"HostOutside":         {reason: "A host outside the ceiling is refused, naming what is allowed.", policy: Policy{Hosts: []string{"api.example.com"}, HostPatterns: []string{"*.example.org"}}, rules: []v1beta1.SandboxHTTPRule{{Host: "evil.example.net", Methods: get}}, want: `sandbox.egress.http[0].host "evil.example.net" is outside the runtime's egress policy (allowed: *.example.org, api.example.com)`},
		"ApexNotUnderPattern": {reason: "A pattern does not cover its apex.", policy: Policy{HostPatterns: []string{"*.example.com"}}, rules: []v1beta1.SandboxHTTPRule{{Host: "example.com", Methods: get}}, want: `sandbox.egress.http[0].host "example.com" is outside`},
		"PatternUnderPattern": {reason: "A narrower pattern under a ceiling pattern is grantable.", policy: Policy{HostPatterns: []string{"*.example.com"}}, rules: []v1beta1.SandboxHTTPRule{{HostPattern: "*.internal.example.com", Methods: get}}},
		"PatternEqual":        {reason: "The same pattern is grantable.", policy: Policy{HostPatterns: []string{"*.example.com"}}, rules: []v1beta1.SandboxHTTPRule{{HostPattern: "*.example.com", Methods: get}}},
		"PatternWider":        {reason: "A wider pattern is refused.", policy: Policy{HostPatterns: []string{"*.internal.example.com"}}, rules: []v1beta1.SandboxHTTPRule{{HostPattern: "*.example.com", Methods: get}}, want: `sandbox.egress.http[0].hostPattern "*.example.com" is outside the runtime's egress policy`},
		"PatternOverHost":     {reason: "An exact host in the ceiling never covers a pattern; the index names the rule.", policy: Policy{Hosts: []string{"api.example.com"}}, rules: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: get}, {HostPattern: "*.example.com", Methods: get}}, want: `sandbox.egress.http[1].hostPattern "*.example.com" is outside`},
		"HostDot":             {reason: "A host that normalizes to nothing would compile into a rule matching every host on an open ceiling; it is not a host name.", rules: []v1beta1.SandboxHTTPRule{{Host: ".", Methods: get}}, want: `sandbox.egress.http[0].host "." must be a bare host name`},
		"HostSpace":           {reason: "Whitespace normalizes away too.", rules: []v1beta1.SandboxHTTPRule{{Host: " ", Methods: get}}, want: `must be a bare host name`},
		"HostPort":            {reason: "A port would make a rule that never matches; a host is a bare name.", rules: []v1beta1.SandboxHTTPRule{{Host: "api.example.com:8443", Methods: get}}, want: `sandbox.egress.http[0].host "api.example.com:8443" must be a bare host name`},
		"HostScheme":          {reason: "So would a scheme.", rules: []v1beta1.SandboxHTTPRule{{Host: "https://api.example.com", Methods: get}}, want: `must be a bare host name`},
		"HostZone":            {reason: "A zoned literal names one of the host's interfaces; refused as a rule and by the dialer.", rules: []v1beta1.SandboxHTTPRule{{Host: "::1%lo0", Methods: get}}, want: `must be a bare host name`},
		"PathPrefixDots":      {reason: "A prefix with dot segments could never match a normalized request path.", rules: []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: get, PathPrefix: "/v1/../x"}}, want: `sandbox.egress.http[0].pathPrefix "/v1/../x" must be normalized`},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			e, err := New(tc.policy)
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
	e, err := New(Policy{})
	if err != nil {
		t.Fatal(err)
	}
	g, err := e.Grant([]v1beta1.SandboxHTTPRule{
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
		"RootPrefixBare":  {reason: "A rule with pathPrefix / admits a URL without a path — the request goes out as / either way.", method: "GET", url: "https://root.internal.example.com"},
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
	e, err := New(Policy{AllowedCIDRs: []string{"0.0.0.0/0", "::/0"}})
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
