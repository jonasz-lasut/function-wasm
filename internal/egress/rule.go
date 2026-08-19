package egress

import "fmt"

// HTTPRule admits requests to one host or host pattern - the shape a module
// manifest's requires.egress.http carries (docs/one-pager-module-manifest.md).
// This package owns it because compiling rules into a Grant is its business;
// the manifest embeds it so a requirement and a grant compare like with like.
type HTTPRule struct {
	// Host is an exact host name, e.g. api.example.com. Exactly one of Host
	// and HostPattern is set.
	Host string `json:"host,omitempty"`

	// HostPattern is a host name with a leading wildcard label, e.g.
	// "*.internal.example.com".
	HostPattern string `json:"hostPattern,omitempty"`

	// Methods the rule admits, e.g. [GET, POST]; at least one - nothing is
	// admitted implicitly.
	Methods []string `json:"methods"`

	// PathPrefix the request path must start with, e.g. /v1/; empty admits
	// any path.
	PathPrefix string `json:"pathPrefix,omitempty"`
}

// methods an egress rule may list.
var methods = map[string]bool{"GET": true, "HEAD": true, "POST": true, "PUT": true, "PATCH": true, "DELETE": true, "OPTIONS": true}

// ValidateRules checks the shape of HTTP egress rules - a module manifest's
// requires.egress.http - naming a wrong rule as field[i]: exactly one of host
// and hostPattern, at least one known method, an absolute normalized
// pathPrefix.
func ValidateRules(field string, rules []HTTPRule) error {
	for i, r := range rules {
		if (r.Host == "") == (r.HostPattern == "") {
			return fmt.Errorf("%s[%d] must set exactly one of host and hostPattern", field, i)
		}
		if r.Host != "" && !ValidHost(r.Host) {
			return fmt.Errorf("%s[%d].host %q must be a bare host name (no scheme, port, path or zone)", field, i, r.Host)
		}
		if r.HostPattern != "" && !ValidHostPattern(r.HostPattern) {
			return fmt.Errorf("%s[%d].hostPattern %q must be a host name with one leading wildcard label, e.g. *.example.com", field, i, r.HostPattern)
		}
		if len(r.Methods) == 0 {
			return fmt.Errorf("%s[%d].methods must list at least one method", field, i)
		}
		for _, m := range r.Methods {
			if !methods[m] {
				return fmt.Errorf("%s[%d].methods: %q is not one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS", field, i, m)
			}
		}
		if r.PathPrefix != "" && r.PathPrefix[0] != '/' {
			return fmt.Errorf("%s[%d].pathPrefix %q must start with /", field, i, r.PathPrefix)
		}
		if !NormalizedPath(r.PathPrefix) {
			return fmt.Errorf("%s[%d].pathPrefix %q must be normalized (no . or .. segments, no empty segments)", field, i, r.PathPrefix)
		}
	}
	return nil
}
