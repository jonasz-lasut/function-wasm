package egress

import (
	"fmt"
	"net/url"
	"path"
	"strings"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

// Grant is a Composition's sandbox.egress.http rules compiled into what one
// run's requests are checked against. It is built per request from the Input,
// before the module is resolved. The operator's host allowlist is Cedar's
// (grantEgress, decided at admission); this holds only the Composition's own
// rules.
type Grant struct {
	egress *Egress
	rules  []rule
}

type rule struct {
	host       string // exact, normalized
	suffix     string // ".example.com" for a pattern rule
	methods    map[string]bool
	pathPrefix string
}

// Grant compiles a Composition's egress rules into what one run's requests are
// checked against. The rules are assumed shape-valid (internal/sandbox.Validate
// ran); the operator's host allowlist is Cedar's (grantEgress, at admission),
// so this no longer intersects a host ceiling - it only compiles the rules.
func (e *Egress) Grant(rules []v1beta1.SandboxHTTPRule) (*Grant, error) {
	g := &Grant{egress: e, rules: make([]rule, 0, len(rules))}
	for i, r := range rules {
		compiled := rule{methods: map[string]bool{}, pathPrefix: r.PathPrefix}
		for _, m := range r.Methods {
			compiled.methods[strings.ToUpper(m)] = true
		}
		if compiled.pathPrefix != "" && !NormalizedPath(compiled.pathPrefix) {
			return nil, fmt.Errorf("sandbox.egress.http[%d].pathPrefix %q must be normalized (no . or .. segments, no empty segments)", i, r.PathPrefix)
		}
		switch {
		case r.Host != "":
			// A host is a bare host name; anything else - a port, a scheme,
			// a zone, or a "." that normalizes to nothing - would be a rule
			// that never matches, or one that matches everything.
			if !ValidHost(r.Host) {
				return nil, fmt.Errorf("sandbox.egress.http[%d].host %q must be a bare host name (no scheme, port, path or zone)", i, r.Host)
			}
			compiled.host = normalizeHost(r.Host)
		default:
			suffix, ok := patternSuffix(r.HostPattern)
			if !ok {
				return nil, fmt.Errorf("sandbox.egress.http[%d].hostPattern %q must be a host name with one leading wildcard label", i, r.HostPattern)
			}
			compiled.suffix = suffix
		}
		g.rules = append(g.rules, compiled)
	}
	return g, nil
}

// admit checks one request — the first or a redirect hop — against the
// grant: an http(s) URL, a normalized path, and a rule that names the host
// and admits the method and the path. The error is what the guest sees.
func (g *Grant) admit(method string, u *url.URL) error {
	if u.Scheme != "http" && u.Scheme != "https" {
		return fmt.Errorf("sandbox.egress: only http and https URLs are allowed, not %q", u.Scheme)
	}
	host := normalizeHost(u.Hostname())
	if host == "" {
		return fmt.Errorf("sandbox.egress: the URL has no host")
	}
	if !NormalizedPath(u.Path) {
		return fmt.Errorf("sandbox.egress: the URL path %q must be normalized (no . or .. segments, no empty segments)", u.Path)
	}
	method = strings.ToUpper(method)
	hostMatched := false
	for _, r := range g.rules {
		if r.host != "" && r.host != host {
			continue
		}
		if r.suffix != "" && !matchesSuffix(host, r.suffix) {
			continue
		}
		hostMatched = true
		if !r.methods[method] {
			continue
		}
		// A URL without a path is the root: pathPrefix "/" admits it.
		if r.pathPrefix != "" && !strings.HasPrefix(pathOrRoot(u.Path), r.pathPrefix) {
			continue
		}
		return nil
	}
	if !hostMatched {
		return fmt.Errorf("sandbox.egress: no rule admits host %q", host)
	}
	return fmt.Errorf("sandbox.egress: no rule for host %q admits %s %s", host, method, pathOrRoot(u.Path))
}

// NormalizedPath reports whether p is what path.Clean would return (a
// trailing slash aside), so a rule's pathPrefix cannot be escaped with dot
// segments the server would collapse; the empty path is fine.
func NormalizedPath(p string) bool {
	if p == "" {
		return true
	}
	cleaned := path.Clean(p)
	if strings.HasSuffix(p, "/") && cleaned != "/" {
		cleaned += "/"
	}
	return cleaned == p
}

func pathOrRoot(p string) string {
	if p == "" {
		return "/"
	}
	return p
}
