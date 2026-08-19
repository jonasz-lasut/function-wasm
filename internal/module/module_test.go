package module

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-containerregistry/pkg/authn"
	"github.com/google/go-containerregistry/pkg/name"
	"github.com/google/go-containerregistry/pkg/registry"
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/empty"
	"github.com/google/go-containerregistry/pkg/v1/mutate"
	"github.com/google/go-containerregistry/pkg/v1/remote"
	"github.com/google/go-containerregistry/pkg/v1/static"
	"github.com/google/go-containerregistry/pkg/v1/types"
	"github.com/spf13/afero"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/authz"
	"github.com/jonasz-lasut/function-wasm/internal/cache"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

var (
	module       = []byte("\x00asm\x01\x00\x00\x00 pretend module")
	moduleDigest = digestOf(module)
	otherDigest  = "sha256:" + strings.Repeat("0", 64)
	manifestRef  = "example.com/repo@" + otherDigest
)

func TestValidate(t *testing.T) {
	cases := map[string]struct {
		reason string
		src    v1beta1.ModuleSource
		want   string
	}{
		"OCI":             {reason: "A digest-pinned reference is valid.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		"HTTP":            {reason: "A URL with a digest is valid.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://x/fn.wasm", Digest: moduleDigest}}},
		"Path":            {reason: "Path alone is valid; it carries no digest.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"}},
		"OCIFrom":         {reason: "A field under status is a valid dynamic source.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "status.module"}},
		"HTTPFromSpec":    {reason: "A field under spec is a valid dynamic source.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.module.http"}},
		"PathFromNested":  {reason: "Nested field paths are accepted.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, From: "spec.modules[0].path"}},
		"NoType":          {reason: "The type is required.", src: v1beta1.ModuleSource{Path: "fn.wasm"}, want: "module.type is required"},
		"UnknownType":     {reason: "Only the three kinds exist.", src: v1beta1.ModuleSource{Type: "S3", Path: "fn.wasm"}, want: `module.type "S3" must be OCI, HTTP or Path`},
		"None":            {reason: "A type without its object or from is invalid.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI}, want: "module.type OCI needs exactly one of module.oci and module.from"},
		"ObjectAndFrom":   {reason: "The typed object and from are exclusive.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "a", From: "status.x"}, want: "module.type Path needs exactly one of module.path and module.from"},
		"WrongObject":     {reason: "An object of another type is refused even next to the right one.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}, Path: "fn.wasm"}, want: "module.path is set but module.type is OCI"},
		"WrongObjectOnly": {reason: "An object that does not match the type is refused before the missing one is reported.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, HTTP: &v1beta1.HTTPSource{URL: "https://x", Digest: moduleDigest}}, want: "module.http is set but module.type is Path"},
		"WrongObjectFrom": {reason: "from does not excuse an object of another type.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.url", OCI: &v1beta1.OCISource{Ref: manifestRef}}, want: "module.oci is set but module.type is HTTP"},
		"OCITagAndDigest": {reason: "A tag next to the manifest digest is fine: the digest pins the manifest, the tag is human-readable context.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: "example.com/repo:v1@" + otherDigest}}},
		"OCITag":          {reason: "A tag reference is refused: tags can be moved.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: "example.com/repo:v1"}}, want: "must be a reference pinned to its manifest digest (repository@sha256:...); tags are not supported"},
		"OCINoRef":        {reason: "An OCI source needs a ref.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{}}, want: "module.oci.ref is required"},
		"HTTPNoDigest":    {reason: "The module digest is mandatory for HTTP.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://x"}}, want: "module.http.digest is required"},
		"HTTPNoURL":       {reason: "An HTTP source needs a URL.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{Digest: moduleDigest}}, want: "module.http.url is required"},
		"FromMetadata":    {reason: "Only spec and status of the composite may name a module.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "metadata.annotations.module"}, want: `module.from "metadata.annotations.module" must be a field under spec or status`},
		"FromBare":        {reason: "spec alone is not a field.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, From: "spec"}, want: "must be a field under spec or status"},
		"HTTPManifest": {
			reason: "An http source may name a manifest URL pinned by its digest.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://x/fn.wasm", Digest: moduleDigest, ManifestURL: "https://x/fn-manifest.yaml", ManifestDigest: otherDigest}},
		},
		"HTTPManifestNoDigest": {
			reason: "A manifest URL must be pinned like the module.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://x/fn.wasm", Digest: moduleDigest, ManifestURL: "https://x/fn-manifest.yaml"}},
			want:   "module.http.manifestURL is set without module.http.manifestDigest",
		},
		"HTTPManifestDigestNoURL": {
			reason: "A manifest digest without a URL is meaningless.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://x/fn.wasm", Digest: moduleDigest, ManifestDigest: otherDigest}},
			want:   "module.http.manifestDigest is set without module.http.manifestURL",
		},
		"HTTPManifestBadURL": {
			reason: "The manifest URL has the shape the module URL has, and its errors name it.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://x/fn.wasm", Digest: moduleDigest, ManifestURL: "ftp://x/m.yaml", ManifestDigest: otherDigest}},
			want:   `module.http.manifestURL "ftp://x/m.yaml" must be an http or https URL`,
		},
		"PathManifest": {
			reason: "A path source may name a manifest file under the module directory.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm", ManifestPath: "fn-manifest.yaml"},
		},
		"ManifestPathWrongType": {
			reason: "manifestPath names a file under --module-dir and belongs only to type Path.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}, ManifestPath: "fn-manifest.yaml"},
			want:   "module.manifestPath is set but module.type is OCI",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			err := Validate(tc.src)
			if tc.want == "" {
				if err != nil {
					t.Fatalf("\n%s\nValidate(): unexpected error %v", tc.reason, err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("\n%s\nValidate(): want error containing %q, got %v", tc.reason, tc.want, err)
			}
		})
	}
}

func mustCompositionPolicy(t *testing.T, doc string) *authz.CompositionPolicy {
	t.Helper()
	p, err := authz.NewCompositionPolicy([]byte(doc))
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func TestFromComposite(t *testing.T) {
	composite := map[string]any{
		"apiVersion": "example.org/v1",
		"kind":       "XR",
		"spec": map[string]any{
			"module":            map[string]any{"ref": manifestRef},
			"private":           map[string]any{"ref": manifestRef, "credentials": "registry"},
			"other":             map[string]any{"ref": "other.example.com/repo@" + otherDigest, "credentials": "registry"},
			"url":               map[string]any{"url": "https://example.com/fn.wasm", "digest": moduleDigest},
			"nopin":             map[string]any{"url": "https://example.com/fn.wasm"},
			"dotted":            map[string]any{"ref": "example.com/repo/../evil@" + otherDigest},
			"dottedurl":         map[string]any{"url": "https://example.com/pub/../secret.wasm", "digest": moduleDigest},
			"upperurl":          map[string]any{"url": "https://EXAMPLE.com/fn.wasm?x=1", "digest": moduleDigest},
			"sibling":           map[string]any{"ref": "example.com/repo-evil@" + otherDigest},
			"subrepo":           map[string]any{"ref": "example.com/repo/sub@" + otherDigest},
			"sibhost":           map[string]any{"url": "https://example.com.attacker.net/fn.wasm", "digest": moduleDigest},
			"manifest":          map[string]any{"url": "https://example.com/fn.wasm", "digest": moduleDigest, "manifestURL": "https://example.com/fn-manifest.yaml", "manifestDigest": otherDigest},
			"manifestelsewhere": map[string]any{"url": "https://example.com/fn.wasm", "digest": moduleDigest, "manifestURL": "https://other.example.com/m.yaml", "manifestDigest": otherDigest},
			"manifestnopin":     map[string]any{"url": "https://example.com/fn.wasm", "digest": moduleDigest, "manifestURL": "https://example.com/m.yaml"},
			"path":              "fn.wasm",
			"typo":              map[string]any{"reference": manifestRef},
			"number":            7,
			"modules":           []any{map[string]any{"ref": manifestRef}},
		},
		"status": map[string]any{
			"module": map[string]any{"ref": manifestRef},
		},
	}
	fenced := mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"example.com");`)
	trusted := mustCompositionPolicy(t, `
permit (principal, action == Action::"pullModule", resource in Repository::"example.com");
permit (principal, action == Action::"spendCredential", resource == Credential::"registry")
when { context.repository in Repository::"example.com" };
`)
	type args struct {
		src       v1beta1.ModuleSource
		policy    *authz.CompositionPolicy
		composite map[string]any
	}
	type want struct {
		src v1beta1.ModuleSource
		err string
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"Static": {
			reason: "A concrete source passes through untouched.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "x.wasm"}, composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "x.wasm"}},
		},
		"StaticIgnoresPolicy": {
			reason: "The composition policy fences what the composite resource chooses; a source the Composition names is not subject to it.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: "other.example.com/repo@" + otherDigest, Credentials: "registry"}}, policy: fenced, composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: "other.example.com/repo@" + otherDigest, Credentials: "registry"}}},
		},
		"OCIFromSpec": {
			reason: "An OCI source object under spec becomes the oci source, within the repositories the Composition fenced it to.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.module"}, policy: fenced, composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		},
		"OCIFromUnfenced": {
			reason: "A source the composite resource chooses requires a compositionPolicy: without one its author could point the runtime at any host and read what the answer says.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.module"}, composite: composite},
			want:   want{err: "module.from: spec.module of the composite resource names a OCI source, but the Input has no compositionPolicy"},
		},
		"HTTPFromUnfenced": {
			reason: "The same for a URL — the runtime would GET whatever the composite resource named.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.url"}, composite: composite},
			want:   want{err: "module.from: spec.url of the composite resource names a HTTP source, but the Input has no compositionPolicy"},
		},
		"OCIFromCredentials": {
			reason: "Fenced for pulling but without a spendCredential permit, a source the composite resource chooses may not spend the step's credentials.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.private"}, policy: fenced, composite: composite},
			want:   want{err: `module.from: spec.private of the composite resource names credentials "registry", which the compositionPolicy does not permit (spendCredential)`},
		},
		"OCIFromDotSegments": {
			reason: "A ref whose repository path has dot segments could escape a prefix once a registry or proxy collapses it; it is not a repository name.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.dotted"}, policy: fenced, composite: composite},
			want:   want{err: `is not a valid repository name`},
		},
		"HTTPFromDotSegments": {
			reason: "A URL path with dot segments is refused for the same reason.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.dottedurl"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"https://example.com/pub");`), composite: composite},
			want:   want{err: `must have a normalized path`},
		},
		"HTTPFromHostCase": {
			reason: "The policy judges the normalized location: the host lowercased, the query left out.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.upperurl"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"https://example.com");`), composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://EXAMPLE.com/fn.wasm?x=1", Digest: moduleDigest}}},
		},
		"OCIFromCredentialsAllowed": {
			reason: "A spendCredential permit co-located with the repository lets the composite resource spend it.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.private"}, policy: trusted, composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef, Credentials: "registry"}}},
		},
		"OCIFromCredentialsNotListed": {
			reason: "A credential no permit names is refused.",
			args: args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.private"}, policy: mustCompositionPolicy(t, `
permit (principal, action == Action::"pullModule", resource in Repository::"example.com");
permit (principal, action == Action::"spendCredential", resource == Credential::"other");
`), composite: composite},
			want: want{err: `module.from: spec.private of the composite resource names credentials "registry", which the compositionPolicy does not permit (spendCredential)`},
		},
		"OCIFromCredentialsOutsideRepositories": {
			reason: "A permitted credential is still refused for a repository outside the pull permits: the repository check comes first, so the secret never reaches a host the policy did not admit.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.other"}, policy: trusted, composite: composite},
			want:   want{err: `module.from: spec.other of the composite resource names ref "other.example.com/repo", which the compositionPolicy does not permit (pullModule)`},
		},
		"OCIFromRepositoryAllowed": {
			reason: "A ref within the allow list is admitted.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.module"}, policy: fenced, composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		},
		"OCIFromRepositoryRefused": {
			reason: "A ref no pullModule permit admits is refused naming the policy and the ref.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.module"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"ghcr.io/example-org");`), composite: composite},
			want:   want{err: `module.from: spec.module of the composite resource names ref "example.com/repo", which the compositionPolicy does not permit (pullModule)`},
		},
		"OCIFromSiblingNamespace": {
			reason: "A permitted repository fences at the path boundary: example.com/repo must not admit the sibling namespace example.com/repo-evil.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.sibling"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"example.com/repo");`), composite: composite},
			want:   want{err: `names ref "example.com/repo-evil", which the compositionPolicy does not permit (pullModule)`},
		},
		"OCIFromExactRepo": {
			reason: "The permitted repository itself is admitted.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.module"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"example.com/repo");`), composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		},
		"OCIFromChildRepo": {
			reason: "A permitted repository admits one it fences with a following slash.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.subrepo"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"example.com/repo");`), composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: "example.com/repo/sub@" + otherDigest}}},
		},
		"HTTPFromAdjacentHost": {
			reason: "The boundary fence protects the host too: https://example.com must not admit the adjacent host https://example.com.attacker.net.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.sibhost"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"https://example.com");`), composite: composite},
			want:   want{err: `names url "https://example.com.attacker.net/fn.wasm", which the compositionPolicy does not permit (pullModule)`},
		},
		"OCIFromStatus": {
			reason: "status works the same way.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "status.module"}, policy: fenced, composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		},
		"OCIFromArray": {
			reason: "Field paths may index into arrays.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.modules[0]"}, policy: fenced, composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		},
		"HTTPFrom": {
			reason: "An HTTP source object carries its own digest; a pullModule permit over its URL admits it.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.url"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"https://example.com");`), composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://example.com/fn.wasm", Digest: moduleDigest}}},
		},
		"HTTPFromRepositoryRefused": {
			reason: "A URL no permit admits is refused.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.url"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"https://modules.example.com");`), composite: composite},
			want:   want{err: `module.from: spec.url of the composite resource names url "https://example.com/fn.wasm", which the compositionPolicy does not permit (pullModule)`},
		},
		"HTTPFromNoDigest": {
			reason: "A dynamic http source without a digest is refused like a static one.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.nopin"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"https://example.com");`), composite: composite},
			want:   want{err: "module.from: spec.nopin of the composite resource: module.http.digest is required"},
		},
		"HTTPFromManifest": {
			reason: "A manifest URL the composite resource chose is admitted when a pullModule permit covers its own location.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.manifest"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"https://example.com");`), composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://example.com/fn.wasm", Digest: moduleDigest, ManifestURL: "https://example.com/fn-manifest.yaml", ManifestDigest: otherDigest}}},
		},
		"HTTPFromManifestFenced": {
			reason: "The manifest URL is fenced like the module: a manifest on a host no permit admits is refused even when the module URL is.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.manifestelsewhere"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"https://example.com");`), composite: composite},
			want:   want{err: `module.from: spec.manifestelsewhere of the composite resource names manifestURL "https://other.example.com/m.yaml", which the compositionPolicy does not permit (pullModule)`},
		},
		"HTTPFromManifestNoDigest": {
			reason: "A manifest URL the composite resource chose must be pinned like a static one.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.manifestnopin"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"https://example.com");`), composite: composite},
			want:   want{err: "module.from: spec.manifestnopin of the composite resource: module.http.manifestURL is set without module.http.manifestDigest"},
		},
		"PrincipalKindMatches": {
			reason: "The policy's principal comes from the composite resource itself: a permit conditioned on its kind matches.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.module"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"example.com") when { principal.xrKind == "XR" };`), composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		},
		"PrincipalKindMismatch": {
			reason: "A permit conditioned on another kind does not match: default-deny.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.module"}, policy: mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"example.com") when { principal.xrKind == "Other" };`), composite: composite},
			want:   want{err: `which the compositionPolicy does not permit (pullModule)`},
		},
		"PathFrom": {
			reason: "A string under spec becomes the path.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, From: "spec.path"}, composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"}},
		},
		"PathFromIgnoresRepositories": {
			reason: "A served file has no repository; the fence does not apply.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, From: "spec.path"}, policy: fenced, composite: composite},
			want:   want{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"}},
		},
		"Missing": {
			reason: "A field the composite does not have is an error naming it.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "status.other"}, composite: composite},
			want:   want{err: "module.from: cannot read status.other from the composite resource"},
		},
		"WrongShape": {
			reason: "A value that does not decode into the source type is an error.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.path"}, composite: composite},
			want:   want{err: "module.from: spec.path of the composite resource is not a {ref, credentials} object"},
		},
		"WrongShapeHTTP": {
			reason: "The type decides what the field must hold.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.module"}, composite: composite},
			want:   want{err: `module.from: spec.module of the composite resource is not a {url, digest} object: json: unknown field "ref"`},
		},
		"UnknownField": {
			reason: "A typo in the object is refused rather than ignored.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.typo"}, composite: composite},
			want:   want{err: `is not a {ref, credentials} object: json: unknown field "reference"`},
		},
		"PathNotString": {
			reason: "A Path source read from the composite must be a string.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, From: "spec.number"}, composite: composite},
			want:   want{err: "module.from: spec.number of the composite resource is not a string"},
		},
		"NoComposite": {
			reason: "Without an observed composite nothing can be read.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "spec.module"}, composite: nil},
			want:   want{err: "module.from spec.module: no observed composite resource to read it from"},
		},
		"Invalid": {
			reason: "Validation runs first.",
			args:   args{src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "metadata.name"}, composite: composite},
			want:   want{err: "must be a field under spec or status"},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := FromComposite(tc.args.src, tc.args.policy, tc.args.composite)
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nFromComposite(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nFromComposite(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.src, got); diff != "" {
				t.Errorf("\n%s\nFromComposite(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

func gzipped(b []byte) []byte {
	var buf bytes.Buffer
	zw := gzip.NewWriter(&buf)
	_, _ = zw.Write(b)
	_ = zw.Close()
	return buf.Bytes()
}

func TestResolvePath(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "fn.wasm"), module, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(dir, "sub"), 0o750); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "sub", "big.wasm"), bytes.Repeat([]byte("x"), 64), 0o600); err != nil {
		t.Fatal(err)
	}
	type want struct {
		digest string
		err    string
	}
	cases := map[string]struct {
		reason string
		opts   Options
		src    v1beta1.ModuleSource
		want   want
	}{
		"Served":    {reason: "A file under the module directory resolves to its digest.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"}, want: want{digest: moduleDigest}},
		"Nested":    {reason: "Subdirectories are fine.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "sub/../fn.wasm"}, want: want{digest: moduleDigest}},
		"NoDir":     {reason: "Without --module-dir path sources are refused.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"}, want: want{err: "started without --module-dir"}},
		"Absolute":  {reason: "Absolute paths are refused.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: filepath.Join(dir, "fn.wasm")}, want: want{err: "must be relative"}},
		"Escape":    {reason: "Paths escaping the directory are refused.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "../fn.wasm"}, want: want{err: "escapes the module directory"}},
		"Missing":   {reason: "A missing file is an error.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "nope.wasm"}, want: want{err: "cannot stat module file"}},
		"Directory": {reason: "A directory is not a module.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "sub"}, want: want{err: "is a directory"}},
		"TooLarge":  {reason: "The size limit is checked before hashing.", opts: Options{Dir: dir, MaxSize: 32}, src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "sub/big.wasm"}, want: want{err: "the limit is 32"}},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			r, err := NewResolver(tc.opts)
			if err != nil {
				t.Fatalf("NewResolver(): %v", err)
			}
			ref, err := r.Resolve(context.Background(), tc.src, nil)
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nResolve(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nResolve(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.digest, ref.Digest); diff != "" {
				t.Errorf("\n%s\nResolve() digest: -want, +got:\n%s", tc.reason, diff)
			}
			got, err := ref.Fetch(context.Background())
			if err != nil {
				t.Fatalf("\n%s\nFetch(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(module, got); diff != "" {
				t.Errorf("\n%s\nFetch(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

func TestResolvePathChange(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "fn.wasm")
	if err := os.WriteFile(path, module, 0o600); err != nil {
		t.Fatal(err)
	}
	r, err := NewResolver(Options{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	first, err := r.Resolve(context.Background(), v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	// A different size guarantees the stamp changes even within mtime granularity.
	if err := os.WriteFile(path, append(module, '!'), 0o600); err != nil {
		t.Fatal(err)
	}
	second, err := r.Resolve(context.Background(), v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if first.Digest == second.Digest {
		t.Errorf("a rewritten module file kept digest %s", first.Digest)
	}
}

func TestResolveHTTP(t *testing.T) {
	var hits atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		hits.Add(1)
		switch req.URL.Path {
		case "/fn.wasm":
			_, _ = w.Write(module)
		case "/other.wasm":
			_, _ = w.Write([]byte("something else"))
		default:
			http.NotFound(w, req)
		}
	}))
	defer srv.Close()

	type want struct {
		err  string
		hits int32
	}
	cases := map[string]struct {
		reason string
		opts   Options
		src    v1beta1.ModuleSource
		fetch  int
		want   want
	}{
		"Download": {
			reason: "The module is downloaded and verified against the digest.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}},
			fetch:  1,
			want:   want{hits: 1},
		},
		"NotFound": {
			reason: "A non-200 status is an error.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/missing.wasm", Digest: moduleDigest}},
			fetch:  1,
			want:   want{err: "404 Not Found", hits: 1},
		},
		"DigestMismatch": {
			reason: "Content that does not match the digest is rejected.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/other.wasm", Digest: moduleDigest}},
			fetch:  1,
			want:   want{err: "module content is sha256:", hits: 1},
		},
		"TooLarge": {
			reason: "Downloads stop at the size limit.",
			opts:   Options{MaxSize: 4},
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}},
			fetch:  1,
			want:   want{err: "exceeds the size limit of 4 bytes", hits: 1},
		},
		"BlobStore": {
			reason: "With a blob store the second fetch does not touch the network.",
			opts:   Options{Blobs: cache.New(afero.NewMemMapFs(), true)},
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}},
			fetch:  2,
			want:   want{hits: 1},
		},
		"NoBlobStore": {
			reason: "Without a blob store every fetch downloads.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}},
			fetch:  2,
			want:   want{hits: 2},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			hits.Store(0)
			r, err := NewResolver(tc.opts)
			if err != nil {
				t.Fatalf("NewResolver(): %v", err)
			}
			ref, err := r.Resolve(context.Background(), tc.src, nil)
			if err != nil {
				t.Fatalf("\n%s\nResolve(): unexpected error %v", tc.reason, err)
			}
			var got []byte
			for range tc.fetch {
				got, err = ref.Fetch(context.Background())
			}
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nFetch(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
			} else {
				if err != nil {
					t.Fatalf("\n%s\nFetch(): unexpected error %v", tc.reason, err)
				}
				if diff := cmp.Diff(module, got); diff != "" {
					t.Errorf("\n%s\nFetch(): -want, +got:\n%s", tc.reason, diff)
				}
			}
			if diff := cmp.Diff(tc.want.hits, hits.Load()); diff != "" {
				t.Errorf("\n%s\nserver hits: -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

// artifact pushes an image with the given layers to the test registry as
// <repo>:v1 and returns its digest reference.
func artifact(t *testing.T, reg string, repo string, layers ...v1.Layer) string {
	t.Helper()
	img := empty.Image
	img = mutate.ConfigMediaType(img, "application/vnd.wasm.config.v0+json")
	img, err := mutate.AppendLayers(img, layers...)
	if err != nil {
		t.Fatalf("cannot build artifact: %v", err)
	}
	ref, err := name.ParseReference(reg + "/" + repo + ":v1")
	if err != nil {
		t.Fatal(err)
	}
	if err := remote.Write(ref, img); err != nil {
		t.Fatalf("cannot push artifact: %v", err)
	}
	d, err := img.Digest()
	if err != nil {
		t.Fatal(err)
	}
	return ref.Context().Digest(d.String()).String()
}

func tarLayer(t *testing.T, gz bool) v1.Layer {
	return paddedTarLayer(t, gz, []byte("hi"), "fn.wasm")
}

// paddedTarLayer stores the module under name behind a README of the given
// content.
func paddedTarLayer(t *testing.T, gz bool, readme []byte, name string) v1.Layer {
	t.Helper()
	var buf bytes.Buffer
	tw := tar.NewWriter(&buf)
	if err := tw.WriteHeader(&tar.Header{Name: "README", Typeflag: tar.TypeReg, Mode: 0o600, Size: int64(len(readme))}); err != nil {
		t.Fatal(err)
	}
	_, _ = tw.Write(readme)
	if err := tw.WriteHeader(&tar.Header{Name: name, Typeflag: tar.TypeReg, Mode: 0o600, Size: int64(len(module))}); err != nil {
		t.Fatal(err)
	}
	_, _ = tw.Write(module)
	_ = tw.Close()
	if gz {
		return static.NewLayer(gzipped(buf.Bytes()), types.DockerLayer)
	}
	return static.NewLayer(buf.Bytes(), types.OCIUncompressedLayer)
}

func TestResolveOCI(t *testing.T) {
	var manifests, blobs atomic.Int32
	var corrupt atomic.Bool
	handler := registry.New()
	reg := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		switch {
		case strings.Contains(req.URL.Path, "/manifests/"):
			manifests.Add(1)
		case strings.Contains(req.URL.Path, "/blobs/"):
			blobs.Add(1)
			if corrupt.Load() && strings.HasSuffix(req.URL.Path, "/blobs/"+moduleDigest) {
				_, _ = w.Write([]byte("not the module"))
				return
			}
		}
		handler.ServeHTTP(w, req)
	}))
	defer reg.Close()
	host := strings.TrimPrefix(reg.URL, "http://")

	wasm := static.NewLayer(module, "application/wasm")
	spin := static.NewLayer(module, "application/vnd.wasm.content.layer.v1+wasm")
	other := static.NewLayer([]byte("not the module"), "application/octet-stream")

	wasmRef := artifact(t, host, "wasm", wasm)
	spinRef := artifact(t, host, "spin", other, spin)
	singleRef := artifact(t, host, "single", static.NewLayer(module, "application/octet-stream"))
	tarRef := artifact(t, host, "tar", tarLayer(t, false))
	tgzRef := artifact(t, host, "tgz", tarLayer(t, true))
	bombRef := artifact(t, host, "bomb", paddedTarLayer(t, true, make([]byte, 64<<10), "fn.wasm"))
	dotSlashRef := artifact(t, host, "dotslash", paddedTarLayer(t, false, []byte("hi"), "./fn.wasm"))
	otherNameRef := artifact(t, host, "othername", paddedTarLayer(t, false, []byte("hi"), "greeter.wasm"))
	nestedRef := artifact(t, host, "nested", paddedTarLayer(t, false, []byte("hi"), "app/fn.wasm"))
	ambiguousRef := artifact(t, host, "ambiguous", other, static.NewLayer(module, "application/octet-stream"))
	missingRef := host + "/wasm@" + otherDigest
	manifest := wasmRef[strings.Index(wasmRef, "@")+1:]
	taggedRef := host + "/wasm:v1@" + manifest
	staleTagRef := host + "/wasm:moved@" + manifest

	type want struct {
		err       string
		manifests int32
		blobs     int32
		stored    int
	}
	cases := map[string]struct {
		reason  string
		opts    Options
		src     v1beta1.OCISource
		fetch   int
		corrupt bool
		want    want
	}{
		"WasmLayer":         {reason: "A raw wasm layer resolves without any registry access and fetches with one manifest read and one blob download.", src: v1beta1.OCISource{Ref: wasmRef}, want: want{manifests: 1, blobs: 1}},
		"TagAndDigest":      {reason: "repository:tag@digest is accepted; the manifest is fetched by digest.", src: v1beta1.OCISource{Ref: taggedRef}, want: want{manifests: 1, blobs: 1}},
		"StaleTagAndDigest": {reason: "The tag is context only: a tag that does not exist (or was moved) changes nothing, the digest is what is fetched.", src: v1beta1.OCISource{Ref: staleTagRef}, want: want{manifests: 1, blobs: 1}},
		"PreferWasmLayer":   {reason: "The wasm-typed layer wins over other layers.", src: v1beta1.OCISource{Ref: spinRef}, want: want{manifests: 1, blobs: 1}},
		"SingleLayer":       {reason: "A single layer of any type is the module.", src: v1beta1.OCISource{Ref: singleRef}, want: want{manifests: 1, blobs: 1}},
		"TarLayer":          {reason: "A tar layer yields /fn.wasm.", src: v1beta1.OCISource{Ref: tarRef}, want: want{manifests: 1, blobs: 1}},
		"GzipTarLayer":      {reason: "A gzipped tar layer (FROM scratch image) works too.", src: v1beta1.OCISource{Ref: tgzRef}, want: want{manifests: 1, blobs: 1}},
		"TarDotSlash":       {reason: "Builders name the root entry fn.wasm, ./fn.wasm or /fn.wasm; all are /fn.wasm.", src: v1beta1.OCISource{Ref: dotSlashRef}, want: want{manifests: 1, blobs: 1}},
		"TarOtherName":      {reason: "The module must be /fn.wasm exactly: another .wasm file is not guessed at.", src: v1beta1.OCISource{Ref: otherNameRef}, want: want{err: "module layer is a tar archive without /fn.wasm: a FROM scratch image must COPY the module to /fn.wasm", manifests: 1, blobs: 1}},
		"TarNested":         {reason: "Nor is a fn.wasm anywhere but the root.", src: v1beta1.OCISource{Ref: nestedRef}, want: want{err: "module layer is a tar archive without /fn.wasm", manifests: 1, blobs: 1}},
		"TarBomb":           {reason: "An archive that expands past eight times the size limit before its .wasm entry is refused, not decompressed to the end.", opts: Options{MaxSize: 4 << 10}, src: v1beta1.OCISource{Ref: bombRef}, want: want{err: "exceeds the size limit before /fn.wasm", manifests: 1, blobs: 1}},
		"Ambiguous":         {reason: "Several layers with no wasm-typed one cannot be resolved.", src: v1beta1.OCISource{Ref: ambiguousRef}, want: want{err: "has 2 layers and none is a wasm layer", manifests: 1}},
		"Missing":           {reason: "An unknown manifest is an error at fetch time.", src: v1beta1.OCISource{Ref: missingRef}, want: want{err: "cannot fetch manifest", manifests: 1}},
		"CorruptLayer":      {reason: "A layer whose bytes do not match the digest the manifest states is refused and not stored.", opts: Options{Blobs: cache.New(afero.NewMemMapFs(), true)}, src: v1beta1.OCISource{Ref: wasmRef}, corrupt: true, want: want{err: "module layer", manifests: 1, blobs: 1}},
		"BlobStore":         {reason: "With a blob store the second fetch reads the manifest but downloads nothing: the layer is stored under its digest.", opts: Options{Blobs: cache.New(afero.NewMemMapFs(), true)}, src: v1beta1.OCISource{Ref: wasmRef}, fetch: 2, want: want{manifests: 2, blobs: 1, stored: 1}},
		"TarBlobStore":      {reason: "A tar layer is stored as fetched and extracted on every read, so it is verifiable on disk.", opts: Options{Blobs: cache.New(afero.NewMemMapFs(), true)}, src: v1beta1.OCISource{Ref: tarRef}, fetch: 2, want: want{manifests: 2, blobs: 1, stored: 1}},
		"NoBlobStore":       {reason: "Without a blob store every fetch downloads.", src: v1beta1.OCISource{Ref: wasmRef}, fetch: 2, want: want{manifests: 2, blobs: 2}},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			manifests.Store(0)
			blobs.Store(0)
			corrupt.Store(tc.corrupt)
			r, err := NewResolver(tc.opts)
			if err != nil {
				t.Fatalf("NewResolver(): %v", err)
			}
			ref, err := r.Resolve(context.Background(), v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &tc.src}, nil)
			if err != nil {
				t.Fatalf("\n%s\nResolve(): unexpected error %v", tc.reason, err)
			}
			if got := manifests.Load() + blobs.Load(); got != 0 {
				t.Errorf("\n%s\nResolve() touched the registry %d times", tc.reason, got)
			}
			if diff := cmp.Diff(tc.src.Ref[strings.Index(tc.src.Ref, "@")+1:], ref.Digest); diff != "" {
				t.Errorf("\n%s\nResolve() digest: -want, +got:\n%s", tc.reason, diff)
			}
			var got []byte
			for range max(tc.fetch, 1) {
				got, err = ref.Fetch(context.Background())
			}
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nFetch(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
			} else {
				if err != nil {
					t.Fatalf("\n%s\nFetch(): unexpected error %v", tc.reason, err)
				}
				if diff := cmp.Diff(module, got); diff != "" {
					t.Errorf("\n%s\nFetch(): -want, +got:\n%s", tc.reason, diff)
				}
			}
			if diff := cmp.Diff(tc.want.manifests, manifests.Load()); diff != "" {
				t.Errorf("\n%s\nmanifest reads: -want, +got:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.blobs, blobs.Load()); diff != "" {
				t.Errorf("\n%s\nblob downloads: -want, +got:\n%s", tc.reason, diff)
			}
			if tc.opts.Blobs != nil {
				if diff := cmp.Diff(tc.want.stored, tc.opts.Blobs.Len()); diff != "" {
					t.Errorf("\n%s\nblobs stored: -want, +got:\n%s", tc.reason, diff)
				}
			}
		})
	}
}

func TestResolveOCIAuth(t *testing.T) {
	handler := registry.New()
	reg := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		if req.URL.Path != "/v2/" {
			user, pass, ok := req.BasicAuth()
			if !ok || user != "robot" || pass != "s3cret" {
				w.Header().Set("WWW-Authenticate", `Basic realm="test"`)
				http.Error(w, "unauthorized", http.StatusUnauthorized)
				return
			}
		}
		handler.ServeHTTP(w, req)
	}))
	defer reg.Close()
	host := strings.TrimPrefix(reg.URL, "http://")

	// Push with credentials.
	img, err := mutate.AppendLayers(empty.Image, static.NewLayer(module, "application/wasm"))
	if err != nil {
		t.Fatal(err)
	}
	tag, err := name.ParseReference(host + "/private:v1")
	if err != nil {
		t.Fatal(err)
	}
	if err := remote.Write(tag, img, remote.WithAuth(&authn.Basic{Username: "robot", Password: "s3cret"})); err != nil {
		t.Fatalf("cannot push: %v", err)
	}
	d, err := img.Digest()
	if err != nil {
		t.Fatal(err)
	}
	ref := tag.Context().Digest(d.String()).String()

	dockerConfig := mustJSON(t, map[string]any{"auths": map[string]any{
		host: map[string]string{"auth": base64.StdEncoding.EncodeToString([]byte("robot:s3cret"))},
	}})
	wrongConfig := mustJSON(t, map[string]any{"auths": map[string]any{
		"other.example.com": map[string]string{"username": "robot", "password": "s3cret"},
	}})

	cases := map[string]struct {
		reason string
		data   map[string][]byte
		want   string
	}{
		"Basic":         {reason: "username/password keys authenticate the pull.", data: map[string][]byte{"username": []byte("robot"), "password": []byte("s3cret")}},
		"DockerConfig":  {reason: "A .dockerconfigjson entry for the registry authenticates the pull.", data: map[string][]byte{".dockerconfigjson": dockerConfig}},
		"WrongRegistry": {reason: "A .dockerconfigjson without the registry is an error.", data: map[string][]byte{".dockerconfigjson": wrongConfig}, want: "no auth entry for registry"},
		"Empty":         {reason: "A credential without usable keys is an error.", data: map[string][]byte{"token": []byte("x")}, want: "neither a .dockerconfigjson key nor username and password keys"},
		"Anonymous":     {reason: "Without credentials the registry refuses the pull.", data: nil, want: "cannot fetch manifest"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var auth authn.Authenticator
			if tc.data != nil {
				a, err := AuthFor(ref, tc.data)
				if tc.want != "" && err != nil {
					if !strings.Contains(err.Error(), tc.want) {
						t.Fatalf("\n%s\nAuthFor(): want error containing %q, got %v", tc.reason, tc.want, err)
					}
					return
				}
				if err != nil {
					t.Fatalf("\n%s\nAuthFor(): unexpected error %v", tc.reason, err)
				}
				auth = a
			}
			r, err := NewResolver(Options{Keychain: authn.NewMultiKeychain()})
			if err != nil {
				t.Fatal(err)
			}
			got, err := r.Resolve(context.Background(), v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: ref}}, auth)
			if err != nil {
				t.Fatalf("\n%s\nResolve(): unexpected error %v", tc.reason, err)
			}
			b, err := got.Fetch(context.Background())
			if tc.want != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want) {
					t.Fatalf("\n%s\nFetch(): want error containing %q, got %v", tc.reason, tc.want, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nFetch(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(module, b); diff != "" {
				t.Errorf("\n%s\nFetch(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

func mustJSON(t *testing.T, v any) []byte {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func TestFetchMetrics(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { _, _ = w.Write(module) }))
	defer srv.Close()
	blobHits, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheBlob, "event": metrics.EventHit})
	blobMisses, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheBlob, "event": metrics.EventMiss})
	fetches, _ := metrics.Sample("function_wasm_module_fetch_duration_seconds", map[string]string{"source": "http"})

	r, err := NewResolver(Options{Blobs: cache.New(afero.NewMemMapFs(), true)})
	if err != nil {
		t.Fatal(err)
	}
	ref, err := r.Resolve(context.Background(), v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}}, nil)
	if err != nil {
		t.Fatal(err)
	}
	for range 2 {
		if _, err := ref.Fetch(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	if got, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheBlob, "event": metrics.EventMiss}); got != blobMisses+1 {
		t.Errorf("blob misses: want %v, got %v", blobMisses+1, got)
	}
	if got, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheBlob, "event": metrics.EventHit}); got != blobHits+1 {
		t.Errorf("blob hits: want %v, got %v", blobHits+1, got)
	}
	if got, _ := metrics.Sample("function_wasm_module_fetch_duration_seconds", map[string]string{"source": "http"}); got != fetches+2 {
		t.Errorf("fetch_duration_seconds{source=http} count: want %v, got %v", fetches+2, got)
	}
}

// TestValidateFrom pins the check a Composition gets without a composite
// resource: shape, policy shape, and the fence a from source of type OCI or
// HTTP requires.
func TestValidateFrom(t *testing.T) {
	fenced := mustCompositionPolicy(t, `permit (principal, action == Action::"pullModule", resource in Repository::"example.com");`)
	cases := map[string]struct {
		reason string
		src    v1beta1.ModuleSource
		policy *authz.CompositionPolicy
		want   string
	}{
		"Static":           {reason: "A static source needs only its shape.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"}},
		"StaticBadShape":   {reason: "Shape errors come first.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI}, want: "module.type OCI needs exactly one of module.oci and module.from"},
		"OCIFromFenced":    {reason: "An OCI from source with a compositionPolicy is admitted without reading the XR; the pullModule verdict waits for the value.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "status.module"}, policy: fenced},
		"OCIFromUnfenced":  {reason: "Without a compositionPolicy an OCI from source is refused, the XR unread.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, From: "status.module"}, want: "module.from: status.module of the composite resource names a OCI source, but the Input has no compositionPolicy"},
		"HTTPFromUnfenced": {reason: "The same for HTTP.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, From: "spec.url"}, want: "module.from: spec.url of the composite resource names a HTTP source, but the Input has no compositionPolicy"},
		"PathFrom":         {reason: "A Path from source has no host to fence.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, From: "spec.path"}},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			err := ValidateFrom(tc.src, tc.policy)
			if tc.want == "" {
				if err != nil {
					t.Fatalf("\n%s\nValidateFrom(): unexpected error %v", tc.reason, err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("\n%s\nValidateFrom(): want error containing %q, got %v", tc.reason, tc.want, err)
			}
		})
	}
}

// TestRefManifest pins how a module's manifest reaches the runtime: the
// artifact's manifest layer, verified and bounded, or nothing at all — an
// artifact without one, a path or http source.
func TestRefManifest(t *testing.T) {
	reg := httptest.NewServer(registry.New())
	defer reg.Close()
	host := strings.TrimPrefix(reg.URL, "http://")
	wasm := static.NewLayer(module, "application/wasm")
	declared := []byte(`{"abi":1,"name":"greeter"}`)
	manifestLayer := static.NewLayer(declared, manifest.LayerMediaType)
	withRef := artifact(t, host, "with", wasm, manifestLayer)
	withoutRef := artifact(t, host, "without", wasm)
	// The manifest layer beside a module layer of no wasm media type still
	// leaves one candidate for the module.
	plainRef := artifact(t, host, "plain", static.NewLayer(module, "application/octet-stream"), manifestLayer)
	hugeRef := artifact(t, host, "huge", wasm, static.NewLayer(bytes.Repeat([]byte("x"), manifest.MaxSize+1), manifest.LayerMediaType))

	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "fn.wasm"), module, 0o600); err != nil {
		t.Fatal(err)
	}
	r, err := NewResolver(Options{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	cases := map[string]struct {
		reason string
		src    v1beta1.ModuleSource
		want   string
		found  bool
		err    string
	}{
		"Layer":    {reason: "The manifest layer is the manifest.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: withRef}}, want: string(declared), found: true},
		"NoLayer":  {reason: "An artifact without one has nothing to declare.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: withoutRef}}},
		"Untyped":  {reason: "The manifest layer does not count as the module's only layer.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: plainRef}}, want: string(declared), found: true},
		"TooLarge": {reason: "The layer is bounded to manifest.MaxSize.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: hugeRef}}, err: "exceeds the size limit"},
		"Path":     {reason: "A path source carries no manifest.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm"}},
		"HTTP":     {reason: "Neither does an http source.", src: v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: "https://example.com/fn.wasm", Digest: moduleDigest}}},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			ref, err := r.Resolve(context.Background(), tc.src, nil)
			if err != nil {
				t.Fatalf("\n%s\nResolve(): %v", tc.reason, err)
			}
			if tc.src.Type == v1beta1.ModuleTypeOCI {
				if b, err := ref.Fetch(context.Background()); err != nil || !bytes.Equal(b, module) {
					t.Fatalf("\n%s\nFetch(): %q %v", tc.reason, b, err)
				}
			}
			got, found, err := ref.Manifest(context.Background())
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\nManifest(): want error containing %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nManifest(): %v", tc.reason, err)
			}
			if found != tc.found || string(got) != tc.want {
				t.Errorf("\n%s\nManifest() = %q, %v; want %q, %v", tc.reason, got, found, tc.want, tc.found)
			}
		})
	}
}

// TestRefManifestByReference pins the manifest a manifest-less source carries
// by reference: a wasmfn.yaml fetched beside an http module (verified against
// its own digest) or read beside a path module, normalized to the JSON an OCI
// manifest layer already carries.
func TestRefManifestByReference(t *testing.T) {
	manifestYAML := []byte("abi: 1\nname: greeter\n")
	manifestJSONBytes := `{"abi":1,"name":"greeter"}`
	manifestYAMLDigest := digestOf(manifestYAML)
	badYAML := []byte("[") // an unclosed flow sequence: valid to fetch, not valid YAML
	badYAMLDigest := digestOf(badYAML)

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		switch req.URL.Path {
		case "/fn.wasm":
			_, _ = w.Write(module)
		case "/fn-manifest.yaml":
			_, _ = w.Write(manifestYAML)
		case "/bad.yaml":
			_, _ = w.Write(badYAML)
		default:
			http.NotFound(w, req)
		}
	}))
	defer srv.Close()

	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "fn.wasm"), module, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "fn-manifest.yaml"), manifestYAML, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "bad.yaml"), badYAML, 0o600); err != nil {
		t.Fatal(err)
	}

	httpSrc := func(manifestURL, manifestDigest string) v1beta1.ModuleSource {
		return v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest, ManifestURL: manifestURL, ManifestDigest: manifestDigest}}
	}
	cases := map[string]struct {
		reason string
		src    v1beta1.ModuleSource
		want   string
		found  bool
		err    string
	}{
		"HTTP": {
			reason: "The manifest is fetched beside the module, verified and converted to JSON.",
			src:    httpSrc(srv.URL+"/fn-manifest.yaml", manifestYAMLDigest),
			want:   manifestJSONBytes, found: true,
		},
		"HTTPDigestMismatch": {
			reason: "A manifest whose bytes do not match the digest is refused.",
			src:    httpSrc(srv.URL+"/fn-manifest.yaml", otherDigest),
			err:    "manifest content is sha256:",
		},
		"HTTPNotFound": {
			reason: "A non-200 status fetching the manifest is an error.",
			src:    httpSrc(srv.URL+"/missing.yaml", manifestYAMLDigest),
			err:    "cannot download manifest: 404",
		},
		"HTTPBadYAML": {
			reason: "A manifest that verifies but is not YAML is refused before the runtime parses it.",
			src:    httpSrc(srv.URL+"/bad.yaml", badYAMLDigest),
			err:    "manifest is not valid YAML",
		},
		"HTTPNone": {
			reason: "Without a manifestURL an http source still carries no manifest.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypeHTTP, HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}},
		},
		"Path": {
			reason: "A path source reads its manifest file under the module directory and converts it to JSON.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm", ManifestPath: "fn-manifest.yaml"},
			want:   manifestJSONBytes, found: true,
		},
		"PathBadYAML": {
			reason: "A path manifest that is not YAML is refused too.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm", ManifestPath: "bad.yaml"},
			err:    "manifest is not valid YAML",
		},
		"PathMissing": {
			reason: "A named manifest file that is not there is an error when it is read.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm", ManifestPath: "nope.yaml"},
			err:    "cannot read manifest file",
		},
		"PathEscape": {
			reason: "A manifest path escaping the module directory is refused at resolve time.",
			src:    v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: "fn.wasm", ManifestPath: "../secret.yaml"},
			err:    `module.manifestPath "../secret.yaml" escapes the module directory`,
		},
	}
	r, err := NewResolver(Options{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			ref, err := r.Resolve(context.Background(), tc.src, nil)
			if err != nil {
				if tc.err != "" && strings.Contains(err.Error(), tc.err) {
					return
				}
				t.Fatalf("\n%s\nResolve(): unexpected error %v", tc.reason, err)
			}
			got, found, err := ref.Manifest(context.Background())
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\nManifest(): want error containing %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nManifest(): %v", tc.reason, err)
			}
			if found != tc.found || string(got) != tc.want {
				t.Errorf("\n%s\nManifest() = %q, %v; want %q, %v", tc.reason, got, found, tc.want, tc.found)
			}
		})
	}
}
