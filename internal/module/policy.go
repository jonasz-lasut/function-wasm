package module

import (
	"errors"
	"fmt"
	"slices"
	"strings"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

// ValidatePolicy checks the shape of an Input's policy: entries are non-empty
// prefixes and names, and a credentials allow list comes with a repository
// allow list — a credential must never be spendable on an arbitrary host. A
// nil policy is valid: any repository, no credentials.
func ValidatePolicy(p *v1beta1.Policy) error {
	if p == nil {
		return nil
	}
	if slices.Contains(p.RepositoryAllowList, "") {
		return errors.New("policy.repositoryAllowList entries must not be empty: an empty prefix admits every repository")
	}
	if slices.Contains(p.CredentialsAllowList, "") {
		return errors.New("policy.credentialsAllowList entries must not be empty")
	}
	if len(p.CredentialsAllowList) > 0 && len(p.RepositoryAllowList) == 0 {
		return errors.New("policy.credentialsAllowList requires policy.repositoryAllowList: a step credential must only be spent on repositories the Composition names")
	}
	return nil
}

// ValidateFrom checks what can be known of a source without the composite
// resource: its shape (Validate), the policy's shape (ValidatePolicy) and,
// for a module.from source of type OCI or HTTP, that policy.repositoryAllowList
// fences it — the rule FromComposite applies once the value is read, applied
// here to the Input alone, so a Composition can be checked without an XR. A
// source without From needs no more than its shape.
func ValidateFrom(src v1beta1.ModuleSource, policy *v1beta1.Policy) error {
	if err := Validate(src); err != nil {
		return err
	}
	if err := ValidatePolicy(policy); err != nil {
		return err
	}
	if src.From == "" {
		return nil
	}
	return requireRepositoryAllowList(src.From, src.Type, policy)
}

// requireRepositoryAllowList is the rule that a source the composite resource
// chooses must be fenced: without a repository allow list its author would
// point the runtime at any host and read what its answer says. Path sources
// have no host.
func requireRepositoryAllowList(from string, t v1beta1.ModuleType, policy *v1beta1.Policy) error {
	if t == v1beta1.ModuleTypePath {
		return nil
	}
	if policy == nil || len(policy.RepositoryAllowList) == 0 {
		return fmt.Errorf("module.from: %s of the composite resource names a %s source, but policy.repositoryAllowList is not set: a module the composite resource chooses must be fenced to repositories the Composition names, or its author could point the runtime at any host", from, t)
	}
	return nil
}

// admit applies policy to a concrete source the composite resource chose
// through the Input field from: the ref (or url) must lie within the
// repository allow list — which such a source requires: without one the
// composite resource's author would point the runtime at any host and read
// what its answer says — and credentials may be named only when the
// credentials allow list has them; the repository check has passed by then,
// so the credential only ever reaches a host the Composition admitted. Path
// sources have neither a repository nor credentials. Prefixes are matched
// against the normalized location (registry/repository for OCI,
// scheme://host/path for HTTP), never the raw string.
func admit(from string, src v1beta1.ModuleSource, policy *v1beta1.Policy) error {
	var field, location string
	var err error
	switch src.Type {
	case v1beta1.ModuleTypeOCI:
		field = "ref"
		location, err = ociLocation(src.OCI.Ref)
	case v1beta1.ModuleTypeHTTP:
		field = "url"
		location, err = httpLocation(src.HTTP.URL)
	case v1beta1.ModuleTypePath:
		return nil
	}
	if err != nil {
		return fmt.Errorf("module.from: %s of the composite resource: %w", from, err)
	}
	if err := requireRepositoryAllowList(from, src.Type, policy); err != nil {
		return err
	}
	if !hasAnyPrefix(location, policy.RepositoryAllowList) {
		return fmt.Errorf("module.from: %s of the composite resource names %s %q, which policy.repositoryAllowList does not admit (allowed prefixes: %s)", from, field, location, strings.Join(policy.RepositoryAllowList, ", "))
	}
	if src.Type != v1beta1.ModuleTypeOCI || src.OCI.Credentials == "" {
		return nil
	}
	if policy == nil || len(policy.CredentialsAllowList) == 0 {
		return fmt.Errorf("module.from: %s of the composite resource names credentials %q, but a module chosen by the composite resource cannot use the step's credentials (the registry host would be its author's) unless policy.credentialsAllowList allows them for a repository in policy.repositoryAllowList; otherwise pull it with the runtime's Docker config or anonymously", from, src.OCI.Credentials)
	}
	if !slices.Contains(policy.CredentialsAllowList, src.OCI.Credentials) {
		return fmt.Errorf("module.from: %s of the composite resource names credentials %q, which policy.credentialsAllowList does not allow (allowed: %s)", from, src.OCI.Credentials, strings.Join(policy.CredentialsAllowList, ", "))
	}
	return nil
}

// hasAnyPrefix reports whether s lies within one of the prefixes at a path or
// host boundary: a prefix admits the location equal to it, or one it fences
// with a following "/". A raw substring prefix would let "ghcr.io/team" admit
// the sibling namespace "ghcr.io/team-evil" - and, worse, "https://cdn.example.com"
// admit the adjacent host "https://cdn.example.com.attacker.net" - so the fence
// is enforced at the boundary whether or not the prefix carries a trailing slash.
func hasAnyPrefix(s string, prefixes []string) bool {
	return slices.ContainsFunc(prefixes, func(p string) bool {
		if s == p {
			return true
		}
		if !strings.HasSuffix(p, "/") {
			p += "/"
		}
		return strings.HasPrefix(s, p)
	})
}
