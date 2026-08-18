package egress

import (
	"fmt"
	"net/url"
	"path"
	"strings"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

// Grant is a Composition's sandbox.egress.http rules admitted by the ceiling
// — what one run's requests are checked against. It is built per request
// from the Input, before the module is resolved, so a rule outside the
// ceiling is refused before anything runs.
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

// Grant intersects rules with the ceiling: every host must be one the ceiling
// admits and every hostPattern must sit under a ceiling pattern. The rules
// are assumed shape-valid (internal/sandbox.Validate ran); the error names
// the rule and what the ceiling admits so the Composition author can act on
// it.
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
			// A host is a bare host name; anything else — a port, a scheme,
			// a zone, or a "." that normalizes to nothing — would be a rule
			// that never matches, or one that matches everything.
			if !ValidHost(r.Host) {
				return nil, fmt.Errorf("sandbox.egress.http[%d].host %q must be a bare host name (no scheme, port, path or zone)", i, r.Host)
			}
			compiled.host = normalizeHost(r.Host)
			if !e.admitsHost(compiled.host) {
				return nil, fmt.Errorf("sandbox.egress.http[%d].host %q is outside the runtime's egress policy (allowed: %s)", i, r.Host, e.describe())
			}
		default:
			suffix, ok := patternSuffix(r.HostPattern)
			if !ok {
				return nil, fmt.Errorf("sandbox.egress.http[%d].hostPattern %q must be a host name with one leading wildcard label", i, r.HostPattern)
			}
			if !e.admitsPattern(suffix) {
				return nil, fmt.Errorf("sandbox.egress.http[%d].hostPattern %q is outside the runtime's egress policy (allowed: %s)", i, r.HostPattern, e.describe())
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
