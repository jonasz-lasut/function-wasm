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
	"github.com/jonasz-lasut/function-wasm/internal/cache"
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
		"OCI":             {reason: "A digest-pinned reference is valid.", src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		"HTTP":            {reason: "A URL with a digest is valid.", src: v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: "https://x/fn.wasm", Digest: moduleDigest}}},
		"Path":            {reason: "Path alone is valid; it carries no digest.", src: v1beta1.ModuleSource{Path: "fn.wasm"}},
		"OCIFrom":         {reason: "A field under status is a valid dynamic source.", src: v1beta1.ModuleSource{OCIFrom: "status.module"}},
		"HTTPFromSpec":    {reason: "A field under spec is a valid dynamic source.", src: v1beta1.ModuleSource{HTTPFrom: "spec.module.http"}},
		"PathFromNested":  {reason: "Nested field paths are accepted.", src: v1beta1.ModuleSource{PathFrom: "spec.modules[0].path"}},
		"None":            {reason: "No source is invalid.", want: "exactly one of"},
		"Two":             {reason: "Two sources are invalid.", src: v1beta1.ModuleSource{Path: "a", OCIFrom: "status.x"}, want: "exactly one of"},
		"OCITagAndDigest": {reason: "A tag next to the manifest digest is fine: the digest pins the manifest, the tag is human-readable context.", src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: "example.com/repo:v1@" + otherDigest}}},
		"OCITag":          {reason: "A tag reference is refused: tags can be moved.", src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: "example.com/repo:v1"}}, want: "must be a reference pinned to its manifest digest (repository@sha256:...); tags are not supported"},
		"OCINoRef":        {reason: "An OCI source needs a ref.", src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{}}, want: "module.oci.ref is required"},
		"HTTPNoDigest":    {reason: "The module digest is mandatory for HTTP.", src: v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: "https://x"}}, want: "module.http.digest is required"},
		"HTTPNoURL":       {reason: "An HTTP source needs a URL.", src: v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{Digest: moduleDigest}}, want: "module.http.url is required"},
		"FromMetadata":    {reason: "Only spec and status of the composite may name a module.", src: v1beta1.ModuleSource{OCIFrom: "metadata.annotations.module"}, want: `module.ociFrom "metadata.annotations.module" must be a field under spec or status`},
		"FromBare":        {reason: "spec alone is not a field.", src: v1beta1.ModuleSource{PathFrom: "spec"}, want: "must be a field under spec or status"},
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

func TestFromComposite(t *testing.T) {
	composite := map[string]any{
		"apiVersion": "example.org/v1",
		"kind":       "XR",
		"spec": map[string]any{
			"module":  map[string]any{"ref": manifestRef, "credentials": "registry"},
			"url":     map[string]any{"url": "https://example.com/fn.wasm", "digest": moduleDigest},
			"nopin":   map[string]any{"url": "https://example.com/fn.wasm"},
			"path":    "fn.wasm",
			"typo":    map[string]any{"reference": manifestRef},
			"number":  7,
			"modules": []any{map[string]any{"ref": manifestRef}},
		},
		"status": map[string]any{
			"module": map[string]any{"ref": manifestRef},
		},
	}
	type want struct {
		src v1beta1.ModuleSource
		err string
	}
	cases := map[string]struct {
		reason    string
		src       v1beta1.ModuleSource
		composite map[string]any
		want      want
	}{
		"Static": {
			reason:    "A concrete source passes through untouched.",
			src:       v1beta1.ModuleSource{Path: "x.wasm"},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{Path: "x.wasm"}},
		},
		"OCIFromSpec": {
			reason:    "An OCI source object under spec becomes the oci source, credentials included.",
			src:       v1beta1.ModuleSource{OCIFrom: "spec.module"},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: manifestRef, Credentials: "registry"}}},
		},
		"OCIFromStatus": {
			reason:    "status works the same way.",
			src:       v1beta1.ModuleSource{OCIFrom: "status.module"},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		},
		"OCIFromArray": {
			reason:    "Field paths may index into arrays.",
			src:       v1beta1.ModuleSource{OCIFrom: "spec.modules[0]"},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: manifestRef}}},
		},
		"HTTPFrom": {
			reason:    "An HTTP source object carries its own digest.",
			src:       v1beta1.ModuleSource{HTTPFrom: "spec.url"},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: "https://example.com/fn.wasm", Digest: moduleDigest}}},
		},
		"HTTPFromNoDigest": {
			reason:    "A dynamic http source without a digest is refused like a static one.",
			src:       v1beta1.ModuleSource{HTTPFrom: "spec.nopin"},
			composite: composite,
			want:      want{err: "module.httpFrom: spec.nopin of the composite resource: module.http.digest is required"},
		},
		"PathFrom": {
			reason:    "A string under spec becomes the path.",
			src:       v1beta1.ModuleSource{PathFrom: "spec.path"},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{Path: "fn.wasm"}},
		},
		"Missing": {
			reason:    "A field the composite does not have is an error naming it.",
			src:       v1beta1.ModuleSource{OCIFrom: "status.other"},
			composite: composite,
			want:      want{err: "module.ociFrom: cannot read status.other from the composite resource"},
		},
		"WrongShape": {
			reason:    "A value that does not decode into the source type is an error.",
			src:       v1beta1.ModuleSource{OCIFrom: "spec.path"},
			composite: composite,
			want:      want{err: "module.ociFrom: spec.path of the composite resource is not a {ref, digest, credentials} object"},
		},
		"UnknownField": {
			reason:    "A typo in the object is refused rather than ignored.",
			src:       v1beta1.ModuleSource{OCIFrom: "spec.typo"},
			composite: composite,
			want:      want{err: `is not a {ref, digest, credentials} object: json: unknown field "reference"`},
		},
		"PathNotString": {
			reason:    "pathFrom must point at a string.",
			src:       v1beta1.ModuleSource{PathFrom: "spec.number"},
			composite: composite,
			want:      want{err: "module.pathFrom: spec.number of the composite resource is not a string"},
		},
		"NoComposite": {
			reason:    "Without an observed composite nothing can be read.",
			src:       v1beta1.ModuleSource{OCIFrom: "spec.module"},
			composite: nil,
			want:      want{err: "module.ociFrom spec.module: no observed composite resource to read it from"},
		},
		"Invalid": {
			reason:    "Validation runs first.",
			src:       v1beta1.ModuleSource{OCIFrom: "metadata.name"},
			composite: composite,
			want:      want{err: "must be a field under spec or status"},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			got, err := FromComposite(tc.src, tc.composite)
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
		"Served":    {reason: "A file under the module directory resolves to its digest.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Path: "fn.wasm"}, want: want{digest: moduleDigest}},
		"Nested":    {reason: "Subdirectories are fine.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Path: "sub/../fn.wasm"}, want: want{digest: moduleDigest}},
		"NoDir":     {reason: "Without --module-dir path sources are refused.", src: v1beta1.ModuleSource{Path: "fn.wasm"}, want: want{err: "started without --module-dir"}},
		"Absolute":  {reason: "Absolute paths are refused.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Path: filepath.Join(dir, "fn.wasm")}, want: want{err: "must be relative"}},
		"Escape":    {reason: "Paths escaping the directory are refused.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Path: "../fn.wasm"}, want: want{err: "escapes the module directory"}},
		"Missing":   {reason: "A missing file is an error.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Path: "nope.wasm"}, want: want{err: "cannot stat module file"}},
		"Directory": {reason: "A directory is not a module.", opts: Options{Dir: dir}, src: v1beta1.ModuleSource{Path: "sub"}, want: want{err: "is a directory"}},
		"TooLarge":  {reason: "The size limit is checked before hashing.", opts: Options{Dir: dir, MaxSize: 32}, src: v1beta1.ModuleSource{Path: "sub/big.wasm"}, want: want{err: "the limit is 32"}},
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
	first, err := r.Resolve(context.Background(), v1beta1.ModuleSource{Path: "fn.wasm"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	// A different size guarantees the stamp changes even within mtime granularity.
	if err := os.WriteFile(path, append(module, '!'), 0o600); err != nil {
		t.Fatal(err)
	}
	second, err := r.Resolve(context.Background(), v1beta1.ModuleSource{Path: "fn.wasm"}, nil)
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
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}},
			fetch:  1,
			want:   want{hits: 1},
		},
		"NotFound": {
			reason: "A non-200 status is an error.",
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/missing.wasm", Digest: moduleDigest}},
			fetch:  1,
			want:   want{err: "404 Not Found", hits: 1},
		},
		"DigestMismatch": {
			reason: "Content that does not match the digest is rejected.",
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/other.wasm", Digest: moduleDigest}},
			fetch:  1,
			want:   want{err: "module content is sha256:", hits: 1},
		},
		"TooLarge": {
			reason: "Downloads stop at the size limit.",
			opts:   Options{MaxSize: 4},
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}},
			fetch:  1,
			want:   want{err: "exceeds the size limit of 4 bytes", hits: 1},
		},
		"BlobStore": {
			reason: "With a blob store the second fetch does not touch the network.",
			opts:   Options{Blobs: cache.New(afero.NewMemMapFs(), true)},
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}},
			fetch:  2,
			want:   want{hits: 1},
		},
		"NoBlobStore": {
			reason: "Without a blob store every fetch downloads.",
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}},
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
	t.Helper()
	var buf bytes.Buffer
	tw := tar.NewWriter(&buf)
	if err := tw.WriteHeader(&tar.Header{Name: "README", Typeflag: tar.TypeReg, Mode: 0o600, Size: 2}); err != nil {
		t.Fatal(err)
	}
	_, _ = tw.Write([]byte("hi"))
	if err := tw.WriteHeader(&tar.Header{Name: "fn.wasm", Typeflag: tar.TypeReg, Mode: 0o600, Size: int64(len(module))}); err != nil {
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
		"TarLayer":          {reason: "A tar layer yields its .wasm file.", src: v1beta1.OCISource{Ref: tarRef}, want: want{manifests: 1, blobs: 1}},
		"GzipTarLayer":      {reason: "A gzipped tar layer (FROM scratch image) works too.", src: v1beta1.OCISource{Ref: tgzRef}, want: want{manifests: 1, blobs: 1}},
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
			ref, err := r.Resolve(context.Background(), v1beta1.ModuleSource{OCI: &tc.src}, nil)
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
			got, err := r.Resolve(context.Background(), v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: ref}}, auth)
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
	ref, err := r.Resolve(context.Background(), v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm", Digest: moduleDigest}}, nil)
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
