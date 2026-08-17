package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-containerregistry/pkg/name"
	"github.com/google/go-containerregistry/pkg/registry"
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/remote"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/module"
)

func TestInitOffline(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "greeter")
	var out bytes.Buffer
	cmd := &InitCmd{Dir: dir, Module: "github.com/me/greeter", SDKVersion: "v0.7.1", WasmfnVersion: "v0.3.0", Offline: true}
	if err := cmd.Run(context.Background(), &out); err != nil {
		t.Fatalf("init: %v", err)
	}
	for _, f := range []string{"go.mod", "main.go", "fn.go", "fn_test.go", "README.md", ".gitignore", "example/composition.yaml", "example/xr.yaml", "example/functions.yaml"} {
		if _, err := os.Stat(filepath.Join(dir, f)); err != nil {
			t.Errorf("missing %s: %v", f, err)
		}
	}
	gomod, err := os.ReadFile(filepath.Join(dir, "go.mod"))
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"module github.com/me/greeter", "go " + goVersion(), "github.com/crossplane/function-sdk-go v0.7.1", "github.com/jonasz-lasut/function-wasm/pkg/wasmfn v0.3.0"} {
		if !strings.Contains(string(gomod), want) {
			t.Errorf("go.mod lacks %q:\n%s", want, gomod)
		}
	}
	if !strings.Contains(out.String(), "Created "+dir) {
		t.Errorf("unexpected output:\n%s", out.String())
	}

	// A second init must not clobber the project.
	if err := cmd.Run(context.Background(), &out); err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Errorf("re-running init over an existing project: want 'already exists' error, got %v", err)
	}
}

// TestParseAndRun drives a command through kong itself, so the parser's
// bindings and defaults are exercised, not only the Run methods.
func TestParseAndRun(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "parsed")
	var out bytes.Buffer
	ctx, err := parser(&out).Parse([]string{"init", dir, "--offline", "--wasmfn-version", "v0.3.0"})
	if err != nil {
		t.Fatalf("Parse(): %v", err)
	}
	if err := ctx.Run(); err != nil {
		t.Fatalf("Run(): %v", err)
	}
	gomod, err := os.ReadFile(filepath.Join(dir, "go.mod"))
	if err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"module parsed", "github.com/crossplane/function-sdk-go " + fallbackSDKVersion, "github.com/jonasz-lasut/function-wasm/pkg/wasmfn v0.3.0"} {
		if !strings.Contains(string(gomod), want) {
			t.Errorf("go.mod lacks %q:\n%s", want, gomod)
		}
	}
	if !strings.Contains(out.String(), "Created "+dir+" (module parsed)") {
		t.Errorf("unexpected output:\n%s", out.String())
	}
}

func TestInitOfflineNeedsVersion(t *testing.T) {
	cmd := &InitCmd{Dir: t.TempDir(), WasmfnVersion: "latest", SDKVersion: "v0.7.1", Offline: true}
	if err := cmd.Run(context.Background(), &bytes.Buffer{}); err == nil || !strings.Contains(err.Error(), "--offline needs --wasmfn-version") {
		t.Errorf("want an error about --wasmfn-version, got %v", err)
	}
}

// TestInitBuild scaffolds a project against the SDK checkout in this
// repository and compiles it to wasm with the Go toolchain: the scaffold must
// build, not just render.
func TestInitBuild(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping scaffold build in -short mode")
	}
	dir := filepath.Join(t.TempDir(), "greeter")
	sdk, err := filepath.Abs(filepath.Join("..", "..", "pkg", "wasmfn"))
	if err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	if err := (&InitCmd{Dir: dir, SDKVersion: "v0.7.1", WasmfnVersion: "latest", WasmfnDir: sdk}).Run(context.Background(), &out); err != nil {
		t.Fatalf("init: %v\n%s", err, out.String())
	}
	if err := (&BuildCmd{Dir: dir, Output: "fn.wasm"}).Run(context.Background(), &out); err != nil {
		t.Fatalf("build: %v\n%s", err, out.String())
	}
	wasm, err := os.ReadFile(filepath.Join(dir, "fn.wasm"))
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.HasPrefix(wasm, []byte("\x00asm")) {
		t.Errorf("fn.wasm does not start with the wasm magic: %q", wasm[:4])
	}
	if !strings.Contains(out.String(), "Built ") || !strings.Contains(out.String(), "ABI v1, imports wasmfn.http wasmfn.log") {
		t.Errorf("build output should carry the ABI verdict and the host imports:\n%s", out.String())
	}
}

func TestDetectLang(t *testing.T) {
	cases := map[string]struct {
		reason string
		files  map[string]string
		want   string
		err    string
	}{
		"Cargo":  {reason: "A Cargo.toml is a Rust guest.", files: map[string]string{"Cargo.toml": "[package]"}, want: "rust"},
		"Go":     {reason: "A go.mod requiring wasmfn is a Go guest.", files: map[string]string{"go.mod": "module x\nrequire github.com/jonasz-lasut/function-wasm/pkg/wasmfn v0.1.0\n"}, want: "go"},
		"TinyGo": {reason: "A go.mod requiring vtprotobuf but not wasmfn is the TinyGo flavour.", files: map[string]string{"go.mod": "module x\nrequire github.com/planetscale/vtprotobuf v0.6.0\n"}, want: "tinygo"},
		"Both":   {reason: "wasmfn wins over vtprotobuf: the SDK guest is built with Go.", files: map[string]string{"go.mod": "module x\nrequire (\n\tgithub.com/planetscale/vtprotobuf v0.6.0\n\tgithub.com/jonasz-lasut/function-wasm/pkg/wasmfn v0.1.0\n)\n"}, want: "go"},
		"None":   {reason: "Neither file is an error pointing at --lang.", files: nil, err: "cannot tell the project's language"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			dir := t.TempDir()
			for f, content := range tc.files {
				if err := os.WriteFile(filepath.Join(dir, f), []byte(content), 0o600); err != nil {
					t.Fatal(err)
				}
			}
			got, err := detectLang(dir)
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\ndetectLang(): want error containing %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\ndetectLang(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want, got); diff != "" {
				t.Errorf("\n%s\ndetectLang(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

// TestInitBuildTinyGo scaffolds the TinyGo flavour and compiles it with
// tinygo; the pre-generated codecs must match the pinned module versions.
func TestInitBuildTinyGo(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping scaffold build in -short mode")
	}
	if _, err := exec.LookPath("tinygo"); err != nil {
		t.Skip("tinygo not on PATH")
	}
	dir := filepath.Join(t.TempDir(), "tiny")
	var out bytes.Buffer
	ctx, err := parser(&out).Parse([]string{"init", dir, "--lang", "tinygo", "--module", "example.com/tiny"})
	if err != nil {
		t.Fatal(err)
	}
	if err := ctx.Run(); err != nil {
		t.Fatalf("init: %v\n%s", err, out.String())
	}
	if err := (&BuildCmd{Dir: dir, Output: "fn.wasm", Lang: "auto"}).Run(context.Background(), &out); err != nil {
		t.Fatalf("build: %v\n%s", err, out.String())
	}
	assertWasm(t, filepath.Join(dir, "fn.wasm"))
	if err := run(context.Background(), dir, &out, "go", "test", "./..."); err != nil {
		t.Fatalf("go test in the scaffold: %v\n%s", err, out.String())
	}
}

// TestInitBuildRust scaffolds the Rust flavour and compiles it with cargo.
func TestInitBuildRust(t *testing.T) {
	if testing.Short() {
		t.Skip("skipping scaffold build in -short mode")
	}
	if _, err := exec.LookPath("cargo"); err != nil {
		t.Skip("cargo not on PATH")
	}
	if _, err := exec.LookPath("protoc"); err != nil {
		t.Skip("protoc not on PATH (prost-build needs it)")
	}
	dir := filepath.Join(t.TempDir(), "rusty")
	var out bytes.Buffer
	ctx, err := parser(&out).Parse([]string{"init", dir, "--lang", "rust"})
	if err != nil {
		t.Fatal(err)
	}
	if err := ctx.Run(); err != nil {
		t.Fatalf("init: %v\n%s", err, out.String())
	}
	if !strings.Contains(out.String(), "(crate rusty)") {
		t.Errorf("unexpected init output:\n%s", out.String())
	}
	if err := (&BuildCmd{Dir: dir, Output: "fn.wasm", Lang: "auto"}).Run(context.Background(), &out); err != nil {
		if strings.Contains(err.Error(), "wasm32-wasip1") || strings.Contains(out.String(), "wasm32-wasip1") {
			t.Skip("rustup target wasm32-wasip1 not installed")
		}
		t.Fatalf("build: %v\n%s", err, out.String())
	}
	assertWasm(t, filepath.Join(dir, "fn.wasm"))
	if err := run(context.Background(), dir, &out, "cargo", "test"); err != nil {
		t.Fatalf("cargo test in the scaffold: %v\n%s", err, out.String())
	}
}

func assertWasm(t *testing.T, path string) {
	t.Helper()
	wasm, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.HasPrefix(wasm, []byte("\x00asm")) {
		t.Errorf("%s does not start with the wasm magic: %q", path, wasm[:4])
	}
}

// Two hand-assembled modules, so these tests need no wasm toolchain:
// abiModule imports wasmfn.log and exports memory, wasmfn_alloc and
// wasmfn_run; nonABIModule lacks wasmfn_run.
var (
	abiModule = []byte{
		0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x12, 0x03, 0x60, 0x03, 0x7f, 0x7f, 0x7f,
		0x00, 0x60, 0x01, 0x7f, 0x01, 0x7f, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7e, 0x02, 0x0e, 0x01, 0x06,
		0x77, 0x61, 0x73, 0x6d, 0x66, 0x6e, 0x03, 0x6c, 0x6f, 0x67, 0x00, 0x00, 0x03, 0x03, 0x02, 0x01,
		0x02, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x26, 0x03, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79,
		0x02, 0x00, 0x0c, 0x77, 0x61, 0x73, 0x6d, 0x66, 0x6e, 0x5f, 0x61, 0x6c, 0x6c, 0x6f, 0x63, 0x00,
		0x01, 0x0a, 0x77, 0x61, 0x73, 0x6d, 0x66, 0x6e, 0x5f, 0x72, 0x75, 0x6e, 0x00, 0x02, 0x0a, 0x0c,
		0x02, 0x05, 0x00, 0x41, 0x80, 0x08, 0x0b, 0x04, 0x00, 0x42, 0x00, 0x0b, 0x00, 0x0e, 0x09, 0x70,
		0x72, 0x6f, 0x64, 0x75, 0x63, 0x65, 0x72, 0x73, 0x74, 0x65, 0x73, 0x74,
	}
	nonABIModule = []byte{
		0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f,
		0x03, 0x02, 0x01, 0x00, 0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x19, 0x02, 0x06, 0x6d, 0x65, 0x6d,
		0x6f, 0x72, 0x79, 0x02, 0x00, 0x0c, 0x77, 0x61, 0x73, 0x6d, 0x66, 0x6e, 0x5f, 0x61, 0x6c, 0x6c,
		0x6f, 0x63, 0x00, 0x00, 0x0a, 0x07, 0x01, 0x05, 0x00, 0x41, 0x80, 0x08, 0x0b,
	}
)

func TestPush(t *testing.T) {
	srv := httptest.NewServer(registry.New())
	defer srv.Close()
	host := strings.TrimPrefix(srv.URL, "http://")

	wasm := abiModule
	file := filepath.Join(t.TempDir(), "fn.wasm")
	if err := os.WriteFile(file, wasm, 0o600); err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	if err := (&PushCmd{Ref: host + "/greeter:v1", File: file}).Run(context.Background(), &out); err != nil {
		t.Fatalf("push: %v", err)
	}
	// The output is the module block a Composition needs, verbatim: the
	// discriminated source with the pushed tag kept for readability, pinned
	// to the manifest digest.
	var refLine string
	for _, line := range strings.Split(out.String(), "\n") {
		if strings.HasPrefix(strings.TrimSpace(line), "ref: ") {
			refLine = strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(line), "ref: "))
		}
	}
	if !strings.HasPrefix(refLine, host+"/greeter:v1@sha256:") {
		t.Fatalf("push output should show the tag pinned to the manifest digest:\n%s", out.String())
	}
	if want := "module:\n  type: OCI\n  oci:\n    ref: " + refLine + "\n"; !strings.HasSuffix(out.String(), want) {
		t.Errorf("push output should end with the module block:\nwant suffix:\n%s\ngot:\n%s", want, out.String())
	}
	wantDigest := refLine[strings.Index(refLine, "@")+1:]

	// The artifact has the CNCF layout...
	ref, err := name.ParseReference(refLine)
	if err != nil {
		t.Fatal(err)
	}
	desc, err := remote.Get(ref)
	if err != nil {
		t.Fatalf("cannot fetch pushed manifest: %v", err)
	}
	m, err := v1.ParseManifest(bytes.NewReader(desc.Manifest))
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(wasmConfigMediaType, m.Config.MediaType); diff != "" {
		t.Errorf("config media type: -want, +got:\n%s", diff)
	}
	if len(m.Layers) != 1 || m.Layers[0].MediaType != wasmLayerMediaType {
		t.Errorf("want one %s layer, got %+v", wasmLayerMediaType, m.Layers)
	}
	// ...with the config the specification lists: layerDigests naming the
	// layer.
	configLayer, err := remote.Layer(ref.Context().Digest(m.Config.Digest.String()))
	if err != nil {
		t.Fatalf("cannot fetch config: %v", err)
	}
	rc, err := configLayer.Compressed()
	if err != nil {
		t.Fatal(err)
	}
	config, err := io.ReadAll(rc)
	if err != nil {
		t.Fatal(err)
	}
	var cfg map[string]any
	if err := json.Unmarshal(config, &cfg); err != nil {
		t.Fatalf("config is not JSON: %v\n%s", err, config)
	}
	wantConfig := map[string]any{"created": "1970-01-01T00:00:00Z", "architecture": "wasm", "os": "wasip1", "layerDigests": []any{m.Layers[0].Digest.String()}}
	if diff := cmp.Diff(wantConfig, cfg); diff != "" {
		t.Errorf("config: -want, +got:\n%s", diff)
	}

	// ...and function-wasm resolves it back to the same bytes.
	r, err := module.NewResolver(module.Options{})
	if err != nil {
		t.Fatal(err)
	}
	got, err := r.Resolve(context.Background(), v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: refLine}}, nil)
	if err != nil {
		t.Fatalf("resolve pushed artifact: %v", err)
	}
	if diff := cmp.Diff(wantDigest, got.Digest); diff != "" {
		t.Errorf("digest: -want, +got:\n%s", diff)
	}
	b, err := got.Fetch(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(wasm, b); diff != "" {
		t.Errorf("module bytes: -want, +got:\n%s", diff)
	}
}

// TestPushRefusesNonABI pins that a module the runtime would refuse at load
// is not published, with the runtime's words.
func TestPushRefusesNonABI(t *testing.T) {
	srv := httptest.NewServer(registry.New())
	defer srv.Close()
	host := strings.TrimPrefix(srv.URL, "http://")
	cases := map[string]struct {
		reason string
		wasm   []byte
		want   string
	}{
		"MissingExport": {reason: "The ABI check runs before anything is pushed.", wasm: nonABIModule, want: `would be refused by the runtime and is not pushed: module does not export "wasmfn_run"`},
		"NotWasm":       {reason: "So does wasmtime's decoder.", wasm: []byte("not wasm"), want: "would be refused by the runtime and is not pushed: cannot compile module"},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			file := filepath.Join(t.TempDir(), "fn.wasm")
			if err := os.WriteFile(file, tc.wasm, 0o600); err != nil {
				t.Fatal(err)
			}
			err := (&PushCmd{Ref: host + "/refused:v1", File: file}).Run(context.Background(), &bytes.Buffer{})
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("\n%s\npush: want error containing %q, got %v", tc.reason, tc.want, err)
			}
			if _, err := remote.Get(mustRef(t, host+"/refused:v1")); err == nil {
				t.Errorf("\n%s\nthe refused module was pushed anyway", tc.reason)
			}
		})
	}
}

func mustRef(t *testing.T, s string) name.Reference {
	t.Helper()
	ref, err := name.ParseReference(s)
	if err != nil {
		t.Fatal(err)
	}
	return ref
}

// TestInspect pins guestfn inspect over a file, over a reference described
// from its manifest alone, and over a reference pulled and read.
func TestInspect(t *testing.T) {
	srv := httptest.NewServer(registry.New())
	defer srv.Close()
	host := strings.TrimPrefix(srv.URL, "http://")
	dir := t.TempDir()
	file := filepath.Join(dir, "fn.wasm")
	if err := os.WriteFile(file, abiModule, 0o600); err != nil {
		t.Fatal(err)
	}
	bad := filepath.Join(dir, "bad.wasm")
	if err := os.WriteFile(bad, nonABIModule, 0o600); err != nil {
		t.Fatal(err)
	}
	var pushOut bytes.Buffer
	if err := (&PushCmd{Ref: host + "/greeter:v1", File: file}).Run(context.Background(), &pushOut); err != nil {
		t.Fatalf("push: %v", err)
	}
	img, err := artifact(abiModule, creationTime())
	if err != nil {
		t.Fatal(err)
	}
	manifestDigest, err := img.Digest()
	if err != nil {
		t.Fatal(err)
	}
	manifestSize, err := img.Size()
	if err != nil {
		t.Fatal(err)
	}
	configDigest, err := img.ConfigName()
	if err != nil {
		t.Fatal(err)
	}
	config, err := img.RawConfigFile()
	if err != nil {
		t.Fatal(err)
	}
	layerDigest := digestOf(abiModule)
	pinned := host + "/greeter:v1@" + manifestDigest.String()

	moduleText := func(indent string) string {
		return indent + "exports: memory (memory), wasmfn_alloc (i32) -> (i32), wasmfn_run (i32, i32) -> (i64)\n" +
			indent + "imports: wasmfn.log (i32, i32, i32) -> ()\n" +
			indent + "memory: 1 pages (64.0 KB) initial, no maximum\n"
	}
	referenceText := func(target string) string {
		return target + ": manifest " + manifestDigest.String() + " (application/vnd.oci.image.manifest.v1+json, " + humanBytes(manifestSize) + ")\n" +
			"  config: application/vnd.wasm.config.v0+json " + configDigest.String() + " (" + humanBytes(int64(len(config))) + ")\n" +
			"  layer: application/wasm " + layerDigest + " (124 B)\n" +
			"  module layer: application/wasm " + layerDigest + " (124 B)\n"
	}
	cases := map[string]struct {
		reason string
		args   []string
		want   string
		err    string
	}{
		"File": {
			reason: "A file is compiled and described: size, verdict, exports, imports, memory.",
			args:   []string{file},
			want:   file + ": 124 B, ABI v1\n" + moduleText("  "),
		},
		"FileNotABI": {
			reason: "A module the runtime would refuse says so in the runtime's words; inspect still describes it.",
			args:   []string{bad},
			want: bad + `: 61 B, not ABI v1: module does not export "wasmfn_run"` + "\n" +
				"  exports: memory (memory), wasmfn_alloc (i32) -> (i32)\n  imports: none\n  memory: 1 pages (64.0 KB) initial, no maximum\n",
		},
		"NotWasm": {
			reason: "A file that is not a module is wasmtime's error.",
			args:   []string{filepath.Join("testdata", "..", "main.go")},
			err:    "cannot compile module",
		},
		"Reference": {
			reason: "A pinned reference is described from its manifest: digest, media types, layers, the layer the runtime would take.",
			args:   []string{pinned},
			want:   referenceText(pinned),
		},
		"Tag": {
			reason: "A tag is fine for inspection — it resolves to the manifest digest shown.",
			args:   []string{host + "/greeter:v1"},
			want:   referenceText(host + "/greeter:v1"),
		},
		"Pull": {
			reason: "--pull reads the module layer as it would a file.",
			args:   []string{pinned, "--pull"},
			want:   referenceText(pinned) + "  module: 124 B, ABI v1\n" + moduleText("  "),
		},
		"Neither": {
			reason: "Something that is neither a file nor a reference says so.",
			args:   []string{"not a ref!"},
			err:    "is neither a file nor an OCI reference",
		},
		"Unknown": {
			reason: "A reference the registry does not have is a fetch error.",
			args:   []string{host + "/nope:v1"},
			err:    "cannot fetch manifest",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var out bytes.Buffer
			ctx, err := parser(&out).Parse(append([]string{"inspect"}, tc.args...))
			if err != nil {
				t.Fatalf("\n%s\nParse(): %v", tc.reason, err)
			}
			err = ctx.Run()
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("\n%s\ninspect: want error containing %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\ninspect: %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want, out.String()); diff != "" {
				t.Errorf("\n%s\ninspect: -want, +got:\n%s", tc.reason, diff)
			}
		})
	}

	// JSON carries the same, structured.
	var out bytes.Buffer
	if err := (&InspectCmd{Target: pinned, Pull: true, Output: "json", MaxSize: 128}).Run(context.Background(), &out); err != nil {
		t.Fatal(err)
	}
	var got inspection
	if err := json.Unmarshal(out.Bytes(), &got); err != nil {
		t.Fatalf("inspect --output json is not JSON: %v\n%s", err, out.String())
	}
	want := inspection{
		Target: pinned,
		Reference: &referenceInfo{
			Digest: manifestDigest.String(), MediaType: "application/vnd.oci.image.manifest.v1+json", Size: manifestSize,
			Config:      descriptorInfo{MediaType: "application/vnd.wasm.config.v0+json", Digest: configDigest.String(), Size: int64(len(config))},
			Layers:      []descriptorInfo{{MediaType: "application/wasm", Digest: layerDigest, Size: 124}},
			ModuleLayer: &descriptorInfo{MediaType: "application/wasm", Digest: layerDigest, Size: 124},
		},
		Module: &moduleInfo{
			Size: 124, ABI: "v1",
			Exports:  []externInfo{{Name: "memory", Kind: "memory"}, {Name: "wasmfn_alloc", Kind: "func", Type: "(i32) -> (i32)"}, {Name: "wasmfn_run", Kind: "func", Type: "(i32, i32) -> (i64)"}},
			Imports:  []externInfo{{Module: "wasmfn", Name: "log", Kind: "func", Type: "(i32, i32, i32) -> ()"}},
			Memories: []memoryInfo{{MinPages: 1}},
		},
	}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Errorf("inspect --output json: -want, +got:\n%s", diff)
	}
}

func digestOf(b []byte) string {
	sum := sha256.Sum256(b)
	return "sha256:" + hex.EncodeToString(sum[:])
}

func TestArtifactDeterministic(t *testing.T) {
	created := time.Date(2026, 8, 16, 0, 0, 0, 0, time.UTC)
	a, err := artifact([]byte("x"), created)
	if err != nil {
		t.Fatal(err)
	}
	b, err := artifact([]byte("x"), created)
	if err != nil {
		t.Fatal(err)
	}
	da, _ := a.Digest()
	db, _ := b.Digest()
	if da != db {
		t.Errorf("the same module and timestamp produced different digests: %s vs %s", da, db)
	}
}
