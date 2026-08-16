package module

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

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

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

var (
	module       = []byte("\x00asm\x01\x00\x00\x00 pretend module")
	moduleDigest = digestOf(module)
	otherDigest  = "sha256:" + strings.Repeat("0", 64)
)

func TestValidate(t *testing.T) {
	cases := map[string]struct {
		reason string
		src    v1beta1.ModuleSource
		want   string
	}{
		"OCI":            {reason: "One source is valid.", src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: "r/x"}}},
		"Path":           {reason: "Path alone is valid.", src: v1beta1.ModuleSource{Path: "fn.wasm"}},
		"OCIFrom":        {reason: "A field under status is a valid dynamic source.", src: v1beta1.ModuleSource{OCIFrom: "status.module"}},
		"HTTPFromSpec":   {reason: "A field under spec is a valid dynamic source.", src: v1beta1.ModuleSource{HTTPFrom: "spec.module.http", Digest: moduleDigest}},
		"PathFromNested": {reason: "Nested field paths are accepted.", src: v1beta1.ModuleSource{PathFrom: "spec.modules[0].path"}},
		"None":           {reason: "No source is invalid.", want: "exactly one of"},
		"Two":            {reason: "Two sources are invalid.", src: v1beta1.ModuleSource{Path: "a", OCIFrom: "status.x"}, want: "exactly one of"},
		"HTTPNoDigest":   {reason: "HTTP without a digest is invalid.", src: v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: "https://x"}}, want: "digest is required for an http source"},
		"BadDigest":      {reason: "A malformed digest is invalid.", src: v1beta1.ModuleSource{Path: "a", Digest: "md5:abc"}, want: "is not sha256:<64 hex characters>"},
		"OCINoRef":       {reason: "An OCI source needs a ref.", src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{}}, want: "module.oci.ref is required"},
		"HTTPNoURL":      {reason: "An HTTP source needs a URL.", src: v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{}, Digest: moduleDigest}, want: "module.http.url is required"},
		"FromMetadata":   {reason: "Only spec and status of the composite may name a module.", src: v1beta1.ModuleSource{OCIFrom: "metadata.annotations.module"}, want: `module.ociFrom "metadata.annotations.module" must be a field under spec or status`},
		"FromBare":       {reason: "spec alone is not a field.", src: v1beta1.ModuleSource{PathFrom: "spec"}, want: "must be a field under spec or status"},
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

func gzipped(b []byte) []byte {
	var buf bytes.Buffer
	zw := gzip.NewWriter(&buf)
	_, _ = zw.Write(b)
	_ = zw.Close()
	return buf.Bytes()
}

func TestFromComposite(t *testing.T) {
	composite := map[string]any{
		"apiVersion": "example.org/v1",
		"kind":       "XR",
		"spec": map[string]any{
			"module":  map[string]any{"ref": "ghcr.io/example/fn@" + moduleDigest, "credentials": "registry"},
			"url":     map[string]any{"url": "https://example.com/fn.wasm"},
			"path":    "fn.wasm",
			"typo":    map[string]any{"reference": "ghcr.io/example/fn"},
			"number":  7,
			"modules": []any{map[string]any{"ref": "ghcr.io/example/first"}},
		},
		"status": map[string]any{
			"module": map[string]any{"ref": "ghcr.io/example/status@" + moduleDigest},
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
			src:       v1beta1.ModuleSource{Path: "x.wasm", Digest: moduleDigest},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{Path: "x.wasm", Digest: moduleDigest}},
		},
		"OCIFromSpec": {
			reason:    "An OCI source object under spec becomes the oci source, credentials included.",
			src:       v1beta1.ModuleSource{OCIFrom: "spec.module"},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: "ghcr.io/example/fn@" + moduleDigest, Credentials: "registry"}}},
		},
		"OCIFromStatus": {
			reason:    "status works the same way.",
			src:       v1beta1.ModuleSource{OCIFrom: "status.module"},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: "ghcr.io/example/status@" + moduleDigest}}},
		},
		"OCIFromArray": {
			reason:    "Field paths may index into arrays.",
			src:       v1beta1.ModuleSource{OCIFrom: "spec.modules[0]"},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: "ghcr.io/example/first"}}},
		},
		"HTTPFrom": {
			reason:    "An HTTP source object is combined with the static digest.",
			src:       v1beta1.ModuleSource{HTTPFrom: "spec.url", Digest: moduleDigest},
			composite: composite,
			want:      want{src: v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: "https://example.com/fn.wasm"}, Digest: moduleDigest}},
		},
		"HTTPFromNoDigest": {
			reason:    "A dynamic http source still needs the static digest.",
			src:       v1beta1.ModuleSource{HTTPFrom: "spec.url"},
			composite: composite,
			want:      want{err: "module.httpFrom: spec.url of the composite resource: module.digest is required for an http source"},
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
			want:      want{err: "module.ociFrom: spec.path of the composite resource is not a {ref, credentials} object"},
		},
		"UnknownField": {
			reason:    "A typo in the object is refused rather than ignored.",
			src:       v1beta1.ModuleSource{OCIFrom: "spec.typo"},
			composite: composite,
			want:      want{err: `is not a {ref, credentials} object: json: unknown field "reference"`},
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
		"Served": {
			reason: "A file under the module directory resolves to its digest.",
			opts:   Options{Dir: dir},
			src:    v1beta1.ModuleSource{Path: "fn.wasm"},
			want:   want{digest: moduleDigest},
		},
		"Nested": {
			reason: "Subdirectories are fine.",
			opts:   Options{Dir: dir},
			src:    v1beta1.ModuleSource{Path: "sub/../fn.wasm"},
			want:   want{digest: moduleDigest},
		},
		"NoDir": {
			reason: "Without --module-dir path sources are refused.",
			src:    v1beta1.ModuleSource{Path: "fn.wasm"},
			want:   want{err: "started without --module-dir"},
		},
		"Absolute": {
			reason: "Absolute paths are refused.",
			opts:   Options{Dir: dir},
			src:    v1beta1.ModuleSource{Path: filepath.Join(dir, "fn.wasm")},
			want:   want{err: "must be relative"},
		},
		"Escape": {
			reason: "Paths escaping the directory are refused.",
			opts:   Options{Dir: dir},
			src:    v1beta1.ModuleSource{Path: "../fn.wasm"},
			want:   want{err: "escapes the module directory"},
		},
		"Missing": {
			reason: "A missing file is an error.",
			opts:   Options{Dir: dir},
			src:    v1beta1.ModuleSource{Path: "nope.wasm"},
			want:   want{err: "cannot stat module file"},
		},
		"Directory": {
			reason: "A directory is not a module.",
			opts:   Options{Dir: dir},
			src:    v1beta1.ModuleSource{Path: "sub"},
			want:   want{err: "is a directory"},
		},
		"TooLarge": {
			reason: "The size limit is checked before hashing.",
			opts:   Options{Dir: dir, MaxSize: 32},
			src:    v1beta1.ModuleSource{Path: "sub/big.wasm"},
			want:   want{err: "the limit is 32"},
		},
		"PinMismatch": {
			reason: "A wrong digest pin is rejected.",
			opts:   Options{Dir: dir},
			src:    v1beta1.ModuleSource{Path: "fn.wasm", Digest: otherDigest},
			want:   want{err: "does not match the source's"},
		},
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
			reason: "The module is downloaded and verified against the pinned digest.",
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm"}, Digest: moduleDigest},
			fetch:  1,
			want:   want{hits: 1},
		},
		"NotFound": {
			reason: "A non-200 status is an error.",
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/missing.wasm"}, Digest: moduleDigest},
			fetch:  1,
			want:   want{err: "404 Not Found", hits: 1},
		},
		"DigestMismatch": {
			reason: "Content that does not match the pin is rejected.",
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/other.wasm"}, Digest: moduleDigest},
			fetch:  1,
			want:   want{err: "module content is sha256:", hits: 1},
		},
		"TooLarge": {
			reason: "Downloads stop at the size limit.",
			opts:   Options{MaxSize: 4},
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm"}, Digest: moduleDigest},
			fetch:  1,
			want:   want{err: "exceeds the size limit of 4 bytes", hits: 1},
		},
		"BlobCache": {
			reason: "With a blob directory the second fetch does not touch the network.",
			opts:   Options{BlobDir: t.TempDir()},
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm"}, Digest: moduleDigest},
			fetch:  2,
			want:   want{hits: 1},
		},
		"NoBlobCache": {
			reason: "Without a blob directory every fetch downloads.",
			src:    v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm"}, Digest: moduleDigest},
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
// <repo>:v1 and returns the repository and the manifest digest.
func artifact(t *testing.T, reg string, repo string, layers ...v1.Layer) (name.Repository, string) {
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
	return ref.Context(), d.String()
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
	var heads atomic.Int32
	handler := registry.New()
	reg := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		if req.Method == http.MethodHead && strings.Contains(req.URL.Path, "/manifests/") {
			heads.Add(1)
		}
		handler.ServeHTTP(w, req)
	}))
	defer reg.Close()
	host := strings.TrimPrefix(reg.URL, "http://")

	wasm := static.NewLayer(module, "application/wasm")
	spin := static.NewLayer(module, "application/vnd.wasm.content.layer.v1+wasm")
	other := static.NewLayer([]byte("not the module"), "application/octet-stream")

	wasmRepo, wasmManifest := artifact(t, host, "wasm", wasm)
	spinRepo, _ := artifact(t, host, "spin", other, spin)
	singleRepo, _ := artifact(t, host, "single", static.NewLayer(module, "application/octet-stream"))
	tarRepo, _ := artifact(t, host, "tar", tarLayer(t, false))
	tgzRepo, _ := artifact(t, host, "tgz", tarLayer(t, true))
	ambiguousRepo, _ := artifact(t, host, "ambiguous", other, static.NewLayer(module, "application/octet-stream"))

	wasmDigest := wasmRepo.Digest(wasmManifest).String()
	now := time.Now()

	type want struct {
		digest string
		err    string
		heads  int32
	}
	cases := map[string]struct {
		reason string
		ref    string
		pin    string
		calls  int
		tick   time.Duration
		want   want
	}{
		"WasmLayerByTag": {
			reason: "A tag resolves via one HEAD to the wasm layer's digest.",
			ref:    wasmRepo.String() + ":v1",
			calls:  1,
			want:   want{digest: moduleDigest, heads: 1},
		},
		"TagCached": {
			reason: "Within the TTL a tag is not resolved again.",
			ref:    wasmRepo.String() + ":v1",
			calls:  3,
			want:   want{digest: moduleDigest, heads: 1},
		},
		"TagExpires": {
			reason: "After the TTL the tag is resolved again.",
			ref:    wasmRepo.String() + ":v1",
			calls:  2,
			tick:   DefaultTagTTL + time.Second,
			want:   want{digest: moduleDigest, heads: 2},
		},
		"ByDigest": {
			reason: "A digest reference needs no HEAD.",
			ref:    wasmDigest,
			calls:  1,
			want:   want{digest: moduleDigest},
		},
		"PreferWasmLayer": {
			reason: "The wasm-typed layer wins over other layers.",
			ref:    spinRepo.String() + ":v1",
			calls:  1,
			want:   want{digest: moduleDigest, heads: 1},
		},
		"SingleLayer": {
			reason: "A single layer of any type is the module.",
			ref:    singleRepo.String() + ":v1",
			calls:  1,
			want:   want{digest: moduleDigest, heads: 1},
		},
		"TarLayer": {
			reason: "A tar layer yields its .wasm file; the digest is the layer's.",
			ref:    tarRepo.String() + ":v1",
			calls:  1,
			want:   want{digest: mustDigest(t, tarLayer(t, false)).String(), heads: 1},
		},
		"GzipTarLayer": {
			reason: "A gzipped tar layer (FROM scratch image) works too.",
			ref:    tgzRepo.String() + ":v1",
			calls:  1,
			want:   want{digest: mustDigest(t, tarLayer(t, true)).String(), heads: 1},
		},
		"Ambiguous": {
			reason: "Several layers with no wasm-typed one cannot be resolved.",
			ref:    ambiguousRepo.String() + ":v1",
			calls:  1,
			want:   want{err: "has 2 layers and none is a wasm layer", heads: 1},
		},
		"Missing": {
			reason: "An unknown tag is an error.",
			ref:    wasmRepo.String() + ":nope",
			calls:  1,
			want:   want{err: "cannot resolve", heads: 1},
		},
		"PinMismatch": {
			reason: "A digest pin that does not match the layer is rejected.",
			ref:    wasmRepo.String() + ":v1",
			pin:    otherDigest,
			calls:  1,
			want:   want{err: "does not match the source's", heads: 1},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			heads.Store(0)
			clock := now
			r, err := NewResolver(Options{Now: func() time.Time { return clock }})
			if err != nil {
				t.Fatalf("NewResolver(): %v", err)
			}
			var ref *Ref
			for i := range tc.calls {
				if i > 0 {
					clock = clock.Add(tc.tick)
				}
				ref, err = r.Resolve(context.Background(), v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: tc.ref}, Digest: tc.pin}, nil)
			}
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nResolve(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
			} else {
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
			}
			if diff := cmp.Diff(tc.want.heads, heads.Load()); diff != "" {
				t.Errorf("\n%s\nmanifest HEADs: -want, +got:\n%s", tc.reason, diff)
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

func mustDigest(t *testing.T, l v1.Layer) v1.Hash {
	t.Helper()
	d, err := l.Digest()
	if err != nil {
		t.Fatal(err)
	}
	return d
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
	ref := host + "/private:v1"

	// Push with credentials.
	img, err := mutate.AppendLayers(empty.Image, static.NewLayer(module, "application/wasm"))
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := name.ParseReference(ref)
	if err != nil {
		t.Fatal(err)
	}
	if err := remote.Write(parsed, img, remote.WithAuth(&authn.Basic{Username: "robot", Password: "s3cret"})); err != nil {
		t.Fatalf("cannot push: %v", err)
	}

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
		"Anonymous":     {reason: "Without credentials the registry refuses the pull.", data: nil, want: "cannot resolve"},
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
			if tc.want != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want) {
					t.Fatalf("\n%s\nResolve(): want error containing %q, got %v", tc.reason, tc.want, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nResolve(): unexpected error %v", tc.reason, err)
			}
			b, err := got.Fetch(context.Background())
			if err != nil {
				t.Fatalf("\n%s\nFetch(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(module, b); diff != "" {
				t.Errorf("\n%s\nFetch(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

func TestFetchMetrics(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { _, _ = w.Write(module) }))
	defer srv.Close()
	blobHits, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheBlob, "event": metrics.EventHit})
	blobMisses, _ := metrics.Sample("function_wasm_module_cache_events_total", map[string]string{"cache": metrics.CacheBlob, "event": metrics.EventMiss})
	fetches, _ := metrics.Sample("function_wasm_module_fetch_duration_seconds", map[string]string{"source": "http"})

	r, err := NewResolver(Options{BlobDir: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	ref, err := r.Resolve(context.Background(), v1beta1.ModuleSource{HTTP: &v1beta1.HTTPSource{URL: srv.URL + "/fn.wasm"}, Digest: moduleDigest}, nil)
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

func TestBlobStoreCorruption(t *testing.T) {
	dir := t.TempDir()
	s, err := newBlobStore(dir)
	if err != nil {
		t.Fatal(err)
	}
	s.put(moduleDigest, module)
	if _, ok := s.get(moduleDigest); !ok {
		t.Fatal("stored blob not found")
	}
	if err := os.WriteFile(s.path(moduleDigest), []byte("corrupt"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, ok := s.get(moduleDigest); ok {
		t.Error("corrupt blob was served")
	}
	if _, err := os.Stat(s.path(moduleDigest)); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("corrupt blob was not removed: %v", err)
	}
}
