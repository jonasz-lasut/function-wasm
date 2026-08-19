package module

import (
	"fmt"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/authz"
)

// ValidateFrom checks what can be known of a source without the composite
// resource: its shape (Validate) and, for a module.from source of type OCI or
// HTTP, that the Input carries a compositionPolicy at all - the fence
// FromComposite evaluates once the value is read (pullModule, default-deny),
// applied here to the Input alone, so a Composition can be checked without an
// XR. A source without From needs no more than its shape.
func ValidateFrom(src v1beta1.ModuleSource, comp *authz.CompositionPolicy) error {
	if err := Validate(src); err != nil {
		return err
	}
	if src.From == "" {
		return nil
	}
	return requireCompositionPolicy(src.From, src.Type, comp)
}

// requireCompositionPolicy is the rule that a source the composite resource
// chooses must be fenced: without a composition policy to permit its
// repository (pullModule), its author would point the runtime at any host and
// read what its answer says. Path sources have no host.
func requireCompositionPolicy(from string, t v1beta1.ModuleType, comp *authz.CompositionPolicy) error {
	if t == v1beta1.ModuleTypePath {
		return nil
	}
	if comp == nil {
		return fmt.Errorf("module.from: %s of the composite resource names a %s source, but the Input has no compositionPolicy: a module the composite resource chooses must be permitted by the compositionPolicy's pullModule rules, or its author could point the runtime at any host", from, t)
	}
	return nil
}

// admit applies the composition policy layer to a concrete source the
// composite resource chose through the Input field from - the source fence of
// the three-layer model (docs/one-pager-three-layer-authz.md), default-deny:
// the ref's (or url's) normalized location (registry/repository for OCI,
// scheme://host/path for HTTP; dot segments refused by Validate) must be
// permitted by a pullModule rule, over the boundary-correct repository
// hierarchy (internal/authz) so a permitted prefix never admits a sibling
// namespace or an adjacent host; and credentials may be named only where a
// spendCredential rule permits them for that location - the pull check has
// passed by then, so the credential only ever reaches a repository the policy
// admitted. Path sources have neither a repository nor credentials. A static
// source is the Composition's own choice and never reaches this fence.
func admit(from string, src v1beta1.ModuleSource, comp *authz.CompositionPolicy, principal authz.Principal) error {
	var field, location string
	var err error
	switch src.Type {
	case v1beta1.ModuleTypeOCI:
		field = "ref"
		location, err = ociLocation(src.OCI.Ref)
	case v1beta1.ModuleTypeHTTP:
		field = "url"
		location, err = httpLocation("module.http.url", src.HTTP.URL)
	case v1beta1.ModuleTypePath:
		return nil
	}
	if err != nil {
		return fmt.Errorf("module.from: %s of the composite resource: %w", from, err)
	}
	if err := requireCompositionPolicy(from, src.Type, comp); err != nil {
		return err
	}
	if !comp.PermitsPullModule(principal, location) {
		return fmt.Errorf("module.from: %s of the composite resource names %s %q, which the compositionPolicy does not permit (pullModule)", from, field, location)
	}
	// A manifest the composite resource chose to fetch by URL is fenced like
	// the module: its own location must be pullModule-permitted, or its author
	// could point the runtime at any host. A static manifestPath (Path) has no
	// host and never reaches here.
	if src.Type == v1beta1.ModuleTypeHTTP && src.HTTP.ManifestURL != "" {
		manifestLoc, err := httpLocation("module.http.manifestURL", src.HTTP.ManifestURL)
		if err != nil {
			return fmt.Errorf("module.from: %s of the composite resource: %w", from, err)
		}
		if !comp.PermitsPullModule(principal, manifestLoc) {
			return fmt.Errorf("module.from: %s of the composite resource names manifestURL %q, which the compositionPolicy does not permit (pullModule)", from, manifestLoc)
		}
	}
	if src.Type != v1beta1.ModuleTypeOCI || src.OCI.Credentials == "" {
		return nil
	}
	if !comp.PermitsSpendCredential(principal, src.OCI.Credentials, location) {
		return fmt.Errorf("module.from: %s of the composite resource names credentials %q, which the compositionPolicy does not permit (spendCredential) for %q: a module chosen by the composite resource cannot spend a step credential (the registry host would be its author's) unless the compositionPolicy permits it for that repository; otherwise pull it with the runtime's Docker config or anonymously", from, src.OCI.Credentials, location)
	}
	return nil
}
