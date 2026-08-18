package authz

import (
	"net/netip"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

// prefixString compares netip.Prefix values by their text form: cmp cannot
// reach netip's unexported fields.
var prefixString = cmp.Transformer("prefix", func(p netip.Prefix) string { return p.String() })

func TestCompileIPRules(t *testing.T) {
	type want struct {
		rules IPRules
		err   string
	}
	cases := map[string]struct {
		reason string
		doc    string
		want   want
	}{
		"None": {
			reason: "A document with no dialAddress rule compiles to an empty table.",
			doc:    `permit (principal, action == Action::"grantEgress", resource);`,
			want:   want{},
		},
		"ForbidRange": {
			reason: "A forbid isInRange rule is an explicit block.",
			doc:    `forbid (principal, action == Action::"dialAddress", resource) when { context.ip.isInRange(ip("203.0.113.0/24")) };`,
			want:   want{rules: IPRules{Blocked: []netip.Prefix{netip.MustParsePrefix("203.0.113.0/24")}}},
		},
		"PermitRange": {
			reason: "A permit isInRange rule punches a hole in the default block list.",
			doc:    `permit (principal, action == Action::"dialAddress", resource) when { context.ip.isInRange(ip("10.96.0.0/12")) };`,
			want:   want{rules: IPRules{Allowed: []netip.Prefix{netip.MustParsePrefix("10.96.0.0/12")}}},
		},
		"BareAddress": {
			reason: "A bare ip literal becomes a host prefix, the way Cedar reads it.",
			doc:    `forbid (principal, action == Action::"dialAddress", resource) when { context.ip.isInRange(ip("192.168.1.1")) };`,
			want:   want{rules: IPRules{Blocked: []netip.Prefix{netip.MustParsePrefix("192.168.1.1/32")}}},
		},
		"Loopback": {
			reason: "isLoopback expands to Cedar's loopback prefixes.",
			doc:    `permit (principal, action == Action::"dialAddress", resource) when { context.ip.isLoopback() };`,
			want:   want{rules: IPRules{Allowed: []netip.Prefix{netip.MustParsePrefix("127.0.0.0/8"), netip.MustParsePrefix("::1/128")}}},
		},
		"Union": {
			reason: "A || of ip tests names every prefix it lists.",
			doc:    `forbid (principal, action == Action::"dialAddress", resource) when { context.ip.isInRange(ip("10.0.0.0/8")) || context.ip.isInRange(ip("192.168.0.0/16")) };`,
			want:   want{rules: IPRules{Blocked: []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8"), netip.MustParsePrefix("192.168.0.0/16")}}},
		},
		"ScopeOnlyForbid": {
			reason: "A conditionless forbid blocks the whole address space.",
			doc:    `forbid (principal, action == Action::"dialAddress", resource);`,
			want:   want{rules: IPRules{Blocked: []netip.Prefix{netip.MustParsePrefix("0.0.0.0/0"), netip.MustParsePrefix("::/0")}}},
		},
		"IgnoresGrantPolicy": {
			reason: "Grant-policy rules for other actions do not contribute prefixes.",
			doc: `permit (principal, action == Action::"grantEgress", resource) when { principal.namespace == "team-a" };
forbid (principal, action == Action::"dialAddress", resource) when { context.ip.isInRange(ip("172.16.0.0/12")) };`,
			want: want{rules: IPRules{Blocked: []netip.Prefix{netip.MustParsePrefix("172.16.0.0/12")}}},
		},
		"BlockAndAllow": {
			reason: "A forbid and a permit fill both lists.",
			doc: `forbid (principal, action == Action::"dialAddress", resource) when { context.ip.isInRange(ip("10.0.0.0/8")) };
permit (principal, action == Action::"dialAddress", resource) when { context.ip.isInRange(ip("10.96.0.0/12")) };`,
			want: want{rules: IPRules{
				Blocked: []netip.Prefix{netip.MustParsePrefix("10.0.0.0/8")},
				Allowed: []netip.Prefix{netip.MustParsePrefix("10.96.0.0/12")},
			}},
		},
		"PrincipalScoped": {
			reason: "A dialAddress rule cannot key on the principal: the dial has no identity.",
			doc:    `forbid (principal == Request::"self", action == Action::"dialAddress", resource) when { context.ip.isInRange(ip("10.0.0.0/8")) };`,
			want:   want{err: "must not constrain the principal or resource"},
		},
		"UnlessCondition": {
			reason: "An unless condition is not a block/allow table.",
			doc:    `forbid (principal, action == Action::"dialAddress", resource) unless { context.ip.isInRange(ip("10.0.0.0/8")) };`,
			want:   want{err: "may only use a when condition"},
		},
		"WrongSubject": {
			reason: "An ip test must be over context.ip, the only value the dial supplies.",
			doc:    `forbid (principal, action == Action::"dialAddress", resource) when { context.other.isInRange(ip("10.0.0.0/8")) };`,
			want:   want{err: "may only test context.ip"},
		},
		"UnsupportedOp": {
			reason: "An operation the table cannot compile fails closed.",
			doc:    `forbid (principal, action == Action::"dialAddress", resource) when { context.ip.isMulticast() };`,
			want:   want{err: `unsupported operation "isMulticast"`},
		},
		"AndCondition": {
			reason: "An && of ip tests is an intersection, not a decision table.",
			doc:    `forbid (principal, action == Action::"dialAddress", resource) when { context.ip.isInRange(ip("10.0.0.0/8")) && context.ip.isInRange(ip("10.1.0.0/16")) };`,
			want:   want{err: `unsupported operation "&&"`},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			p, err := NewOperatorPolicy("cidr_test.cedar", []byte(tc.doc))
			if err != nil {
				t.Fatalf("\n%s\nNewOperatorPolicy(): %v", tc.reason, err)
			}
			got, err := p.CompileIPRules()
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nCompileIPRules(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nCompileIPRules(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.rules, got, prefixString); diff != "" {
				t.Errorf("\n%s\nCompileIPRules(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

func TestCompileIPRulesNil(t *testing.T) {
	got, err := (*OperatorPolicy)(nil).CompileIPRules()
	if err != nil {
		t.Fatalf("CompileIPRules() on a nil policy: %v", err)
	}
	if diff := cmp.Diff(IPRules{}, got, prefixString); diff != "" {
		t.Errorf("CompileIPRules() on a nil policy: -want, +got:\n%s", diff)
	}
}
