package main

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
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
	if !strings.Contains(out.String(), "Built ") {
		t.Errorf("unexpected build output:\n%s", out.String())
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

func TestPush(t *testing.T) {
	srv := httptest.NewServer(registry.New())
	defer srv.Close()
	host := strings.TrimPrefix(srv.URL, "http://")

	wasm := []byte("\x00asm\x01\x00\x00\x00 module bytes")
	file := filepath.Join(t.TempDir(), "fn.wasm")
	if err := os.WriteFile(file, wasm, 0o600); err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	if err := (&PushCmd{Ref: host + "/greeter:v1", File: file}).Run(context.Background(), &out); err != nil {
		t.Fatalf("push: %v", err)
	}
	lines := strings.Split(strings.TrimSpace(out.String()), "\n")
	if len(lines) != 2 || !strings.HasPrefix(lines[1], host+"/greeter@sha256:") {
		t.Fatalf("push output should end with the digest reference:\n%s", out.String())
	}

	// The artifact has the CNCF layout...
	ref, err := name.ParseReference(lines[1])
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

	// ...and function-wasm resolves it back to the same bytes.
	r, err := module.NewResolver(module.Options{})
	if err != nil {
		t.Fatal(err)
	}
	got, err := r.Resolve(context.Background(), v1beta1.ModuleSource{OCI: &v1beta1.OCISource{Ref: lines[1]}}, nil)
	if err != nil {
		t.Fatalf("resolve pushed artifact: %v", err)
	}
	sum := sha256.Sum256(wasm)
	if diff := cmp.Diff("sha256:"+hex.EncodeToString(sum[:]), got.Digest); diff != "" {
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
