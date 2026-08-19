package authz

import (
	"strings"

	"github.com/cedar-policy/cedar-go/types"
)

// The Cedar entity types of the shared schema. Repository and HostPattern are
// boundary-correct hierarchies (a location's ancestors are its boundary
// prefixes, so `in` respects the boundary); Capability and Credential are flat.
const (
	repositoryType  = types.EntityType("Repository")
	hostPatternType = types.EntityType("HostPattern")
	capabilityType  = types.EntityType("Capability")
	credentialType  = types.EntityType("Credential")
	requestType     = types.EntityType("Request")
)

// repo is the Repository entity for a location or a prefix.
func repo(s string) types.EntityUID {
	return types.NewEntityUID(repositoryType, types.String(s))
}

// repositoryEntities returns the Repository entity for location and the entity
// map that gives it its path-boundary ancestors, so `resource in
// Repository::"p"` is true when p equals location or fences it at a "/". The
// location itself is handled by the reflexivity of Cedar's `in`, so it is the
// map's resource entity with its prefixes as parents.
func repositoryEntities(location string) (types.EntityUID, types.EntityMap) {
	resource := repo(location)
	entities := types.EntityMap{}
	prefixes := boundaryPrefixes(location)
	parents := make([]types.EntityUID, 0, len(prefixes))
	for _, p := range prefixes {
		uid := repo(p)
		parents = append(parents, uid)
		entities[uid] = types.Entity{UID: uid}
	}
	entities[resource] = types.Entity{UID: resource, Parents: types.NewEntityUIDSet(parents...)}
	return resource, entities
}

// boundaryPrefixes returns the path-boundary ancestors of a repository
// location. For every prefix ending immediately before a "/" it emits both
// forms - "ghcr.io/team" and "ghcr.io/team/" - so an allowlist entry matches
// whether or not it carries a trailing slash, exactly as a boundary-aware
// string prefix would, while a sibling namespace or adjacent host (which is not
// such a prefix) never does. The location itself is not included: Cedar's `in`
// is reflexive.
func boundaryPrefixes(location string) []string {
	var out []string
	for i, c := range location {
		if c != '/' {
			continue
		}
		p := location[:i]
		// A leading "/" would produce an empty prefix; a location the module
		// package emits never starts with one, this keeps the function total.
		if p == "" {
			continue
		}
		// A NUL in an entity id is refused upstream (ociLocation/httpLocation);
		// drop the prefix defensively rather than carry it into Cedar.
		if strings.ContainsRune(p, 0) {
			continue
		}
		out = append(out, p, p+"/")
	}
	return out
}

// host is the HostPattern entity for a host, a host pattern, or a DNS suffix.
func host(s string) types.EntityUID {
	return types.NewEntityUID(hostPatternType, types.String(s))
}

// hostEntities returns the HostPattern entity for an egress grant and the map
// giving it its DNS-suffix ancestors, so `resource in HostPattern::"example.com"`
// is true for an exact host under example.com and for the pattern
// "*.example.com". The entity id is the literal host or pattern (so an operator
// can pin one exactly), and it carries a `host` attribute for `like` conditions.
func hostEntities(g EgressGrant) (types.EntityUID, types.EntityMap) {
	var label, boundary string
	if g.Host != "" {
		label = normalizeHost(g.Host)
		boundary = label
	} else {
		// A pattern "*.example.com" is bounded by example.com, which becomes an
		// ancestor so the pattern is `in HostPattern::"example.com"`.
		label = normalizeHost(g.HostPattern)
		boundary = strings.TrimPrefix(label, "*.")
	}
	resource := host(label)
	entities := types.EntityMap{}
	ancestors := dnsSuffixes(boundary)
	if g.Host == "" {
		ancestors = append([]string{boundary}, ancestors...)
	}
	parents := make([]types.EntityUID, 0, len(ancestors))
	for _, a := range ancestors {
		uid := host(a)
		parents = append(parents, uid)
		entities[uid] = types.Entity{UID: uid}
	}
	entities[resource] = types.Entity{
		UID:        resource,
		Parents:    types.NewEntityUIDSet(parents...),
		Attributes: types.NewRecord(types.RecordMap{"host": types.String(label)}),
	}
	return resource, entities
}

// dnsSuffixes returns the proper label-boundary suffixes of a host, longest
// first: "api.example.com" yields "example.com" and "com". A host with no dot
// has none. Every suffix ends at a label boundary, so a suffix never admits an
// adjacent host ("cdn.example.com.attacker.net" is not under "example.com").
func dnsSuffixes(h string) []string {
	labels := strings.Split(h, ".")
	out := make([]string, 0, len(labels))
	for i := 1; i < len(labels); i++ {
		out = append(out, strings.Join(labels[i:], "."))
	}
	return out
}

// normalizeHost lowercases a host name and drops surrounding space and a
// trailing dot, so a rule and a policy compare it the way DNS does. It mirrors
// egress.normalizeHost, which authz cannot import (it is unexported); hosts
// reach here already shape-checked by internal/sandbox.
func normalizeHost(h string) string {
	return strings.TrimSuffix(strings.ToLower(strings.TrimSpace(h)), ".")
}

// cred is the Credential entity for a step credential name.
func cred(s string) types.EntityUID {
	return types.NewEntityUID(credentialType, types.String(s))
}
