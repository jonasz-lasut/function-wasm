//go:build ignore

// vendorproto keeps the copies of crossplane's run_function.proto in this
// repository in step with the function-sdk-go version the root go.mod
// requires. It is run by go generate (see generate.go) and writes:
//
//   - templates/{tinygo,rust}/proto/run_function.proto and the same file in
//     examples/hello-tinygo and examples/hello-rust — the proto with a header
//     naming its origin and version;
//   - templates/tinygo/internal/fnv1/*.pb.go.tmpl — a copy of the TinyGo
//     example's generated code, which is produced from that proto by
//     `go generate ./...` in examples/hello-tinygo (needs protoc).
//
// After a function-sdk-go bump: go generate ./... (re-vendors the proto),
// go generate in examples/hello-tinygo (regenerates the codecs), go generate
// ./... again (copies them into the template and refreshes the goldens).
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

const sdkModule = "github.com/crossplane/function-sdk-go"

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "vendorproto:", err)
		os.Exit(1)
	}
}

func run() error {
	out, err := exec.Command("go", "list", "-m", "-json", sdkModule).Output()
	if err != nil {
		return fmt.Errorf("cannot locate %s in the module cache: %w", sdkModule, err)
	}
	var mod struct {
		Dir     string
		Version string
	}
	if err := json.Unmarshal(out, &mod); err != nil {
		return err
	}
	if mod.Dir == "" {
		return fmt.Errorf("%s has no local directory; run go mod download", sdkModule)
	}
	proto, err := os.ReadFile(filepath.Join(mod.Dir, "proto", "v1", "run_function.proto"))
	if err != nil {
		return err
	}

	examples := filepath.Join("..", "..", "..", "..", "examples")
	targets := map[string]string{
		filepath.Join("templates", "tinygo", "proto", "run_function.proto"):    tinygoNote,
		filepath.Join(examples, "hello-tinygo", "proto", "run_function.proto"): tinygoNote,
		filepath.Join("templates", "rust", "proto", "run_function.proto"):      rustNote,
		filepath.Join(examples, "hello-rust", "proto", "run_function.proto"):   rustNote,
	}
	for path, note := range targets {
		header := fmt.Sprintf("// Vendored from %s %s (proto/v1/run_function.proto), Apache-2.0,\n// by go generate in the function-wasm repository. %s\n\n", sdkModule, mod.Version, note)
		if err := os.WriteFile(path, append([]byte(header), proto...), 0o644); err != nil {
			return err
		}
	}

	generated, err := filepath.Glob(filepath.Join(examples, "hello-tinygo", "internal", "fnv1", "*.pb.go"))
	if err != nil {
		return err
	}
	if len(generated) == 0 {
		return fmt.Errorf("examples/hello-tinygo/internal/fnv1 holds no generated code; run go generate there first")
	}
	for _, src := range generated {
		b, err := os.ReadFile(src)
		if err != nil {
			return err
		}
		dst := filepath.Join("templates", "tinygo", "internal", "fnv1", filepath.Base(src)+".tmpl")
		if strings.Contains(string(b), "[[") {
			return fmt.Errorf("%s contains the template delimiter [[", src)
		}
		if err := os.WriteFile(dst, b, 0o644); err != nil {
			return err
		}
	}
	return nil
}

const (
	tinygoNote = "Regenerate the Go code with `go generate ./...` (see generate.go)."
	rustNote   = "build.rs compiles it with prost-build at cargo build time (needs protoc)."
)
