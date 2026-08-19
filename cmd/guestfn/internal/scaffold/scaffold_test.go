package scaffold

import (
	"flag"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

var update = flag.Bool("update", false, "regenerate the golden scaffolds under testdata/<lang>")

// golden are the options the golden scaffolds are rendered with, per language.
func golden(lang string) Options {
	return Options{
		Lang:       lang,
		Module:     "github.com/example/my-fn",
		Name:       "my-fn",
		GoVersion:  "1.26.6",
		SDKVersion: "v0.7.1",
		Requires:   true,
	}
}

func TestRender(t *testing.T) {
	for _, lang := range Langs {
		t.Run(lang, func(t *testing.T) {
			dir := filepath.Join("testdata", lang)
			files, err := Render(golden(lang))
			if err != nil {
				t.Fatalf("Render(): %v", err)
			}
			if *update {
				if err := os.RemoveAll(dir); err != nil {
					t.Fatal(err)
				}
				if err := Write(dir, files); err != nil {
					t.Fatal(err)
				}
			}
			want := readTree(t, dir)
			if diff := cmp.Diff(names(want), names(files)); diff != "" {
				t.Fatalf("scaffold files: -want, +got:\n%s\n(run go generate ./... to refresh the golden)", diff)
			}
			for name := range want {
				if diff := cmp.Diff(string(want[name]), string(files[name])); diff != "" {
					t.Errorf("%s: -want, +got:\n%s\n(run go generate ./... to refresh the golden)", name, diff)
				}
			}
		})
	}
}

// examples maps each language to the example guest that is its scaffold
// rendered for itself.
var examples = map[string]Options{
	LangGo:     {Lang: LangGo, Module: "github.com/jonasz-lasut/function-wasm/examples/hello-go", GoVersion: "1.26.6"},
	LangTinyGo: {Lang: LangTinyGo, Module: "github.com/jonasz-lasut/function-wasm/examples/hello-tinygo", GoVersion: "1.26.6"},
	LangRust:   {Lang: LangRust, Name: "hello-rust"},
	LangZig:    {Lang: LangZig, Name: "hello-zig"},
}

// TestRenderMatchesExample keeps each scaffold and its example the same
// project. Only go.mod may differ (the Go examples replace the SDK with the
// checkout and carry tidy's indirect requirements); the examples' Makefile,
// Cargo.lock and generated build artefacts are extra.
func TestRenderMatchesExample(t *testing.T) {
	for lang, o := range examples {
		t.Run(lang, func(t *testing.T) {
			files, err := Render(o)
			if err != nil {
				t.Fatalf("Render(): %v", err)
			}
			example := filepath.Join("..", "..", "..", "..", "examples", exampleDir(lang))
			for name, rendered := range files {
				if name == "go.mod" {
					continue
				}
				want, err := os.ReadFile(filepath.Join(example, name))
				if err != nil {
					t.Errorf("examples/%s/%s: %v (the scaffold renders it; copy it there)", exampleDir(lang), name, err)
					continue
				}
				if diff := cmp.Diff(string(want), string(rendered)); diff != "" {
					t.Errorf("examples/%s/%s differs from the scaffold template: -example, +scaffold:\n%s", exampleDir(lang), name, diff)
				}
			}
		})
	}
}

func exampleDir(lang string) string {
	return "hello-" + lang
}

func TestRenderOptions(t *testing.T) {
	cases := map[string]struct {
		reason string
		opts   Options
		file   string
		want   []string
		err    string
	}{
		"NoModule": {
			reason: "A module path is mandatory for Go.",
			err:    "a module path is required",
		},
		"NoModuleTinyGo": {
			reason: "A module path is mandatory for TinyGo too.",
			opts:   Options{Lang: LangTinyGo, Name: "x"},
			err:    "a module path is required",
		},
		"RustNeedsName": {
			reason: "Rust has no module path, so a name is mandatory.",
			opts:   Options{Lang: LangRust},
			err:    "a name is required",
		},
		"UnknownLang": {
			reason: "Unknown languages are refused.",
			opts:   Options{Lang: "cobol", Name: "x"},
			err:    `unsupported language "cobol"; one of go, tinygo, rust, zig`,
		},
		"NameFromModule": {
			reason: "The name defaults to the module's last element.",
			opts:   Options{Module: "github.com/me/greeter"},
			file:   "README.md",
			want:   []string{"# greeter"},
		},
		"ExplicitName": {
			reason: "An explicit name wins.",
			opts:   Options{Module: "github.com/me/greeter", Name: "hi"},
			file:   "example/composition.yaml",
			want:   []string{"name: hi", "step: hi"},
		},
		"OfflineRequires": {
			reason: "Without go get the require block carries the SDK version; go mod tidy pulls the rest, including the deps of the vendored internal/wasmfn glue.",
			opts:   Options{Module: "github.com/me/greeter", GoVersion: "1.26.6", SDKVersion: "v0.7.1", Requires: true},
			file:   "go.mod",
			want:   []string{"go 1.26.6", "require github.com/crossplane/function-sdk-go v0.7.1"},
		},
		"VendoredGlue": {
			reason: "The Go scaffold owns its ABI glue under internal/wasmfn and imports it by the project's module path.",
			opts:   Options{Module: "github.com/me/greeter", GoVersion: "1.26.6"},
			file:   "main.go",
			want:   []string{`import "github.com/me/greeter/internal/wasmfn"`},
		},
		"TinyGoModule": {
			reason: "The TinyGo scaffold pins the codec versions its generated code was made with and points generate at the module.",
			opts:   Options{Lang: LangTinyGo, Module: "github.com/me/greeter", GoVersion: "1.26.6"},
			file:   "go.mod",
			want:   []string{"module github.com/me/greeter", "github.com/planetscale/vtprotobuf v0.6.0", "tool ("},
		},
		"TinyGoGenerate": {
			reason: "generate.go maps the proto onto the guest's own module path.",
			opts:   Options{Lang: LangTinyGo, Module: "github.com/me/greeter", GoVersion: "1.26.6"},
			file:   "generate.go",
			want:   []string{"Mrun_function.proto=github.com/me/greeter/internal/fnv1;fnv1"},
		},
		"RustCrate": {
			reason: "The crate is named after the guest.",
			opts:   Options{Lang: LangRust, Name: "greeter"},
			file:   "Cargo.toml",
			want:   []string{`name = "greeter"`, `crate-type = ["cdylib", "rlib"]`},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			files, err := Render(tc.opts)
			if tc.err != "" {
				if err == nil || err.Error() != tc.err {
					t.Fatalf("\n%s\nRender(): want error %q, got %v", tc.reason, tc.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nRender(): unexpected error %v", tc.reason, err)
			}
			got := string(files[tc.file])
			for _, w := range tc.want {
				if !strings.Contains(got, w) {
					t.Errorf("\n%s\n%s does not contain %q:\n%s", tc.reason, tc.file, w, got)
				}
			}
		})
	}
}

func TestWriteRefusesOverwrite(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "fn.go"), []byte("mine"), 0o600); err != nil {
		t.Fatal(err)
	}
	err := Write(dir, map[string][]byte{"fn.go": []byte("theirs"), "main.go": []byte("x")})
	if err == nil {
		t.Fatal("Write() over an existing file succeeded")
	}
	if b, _ := os.ReadFile(filepath.Join(dir, "fn.go")); string(b) != "mine" {
		t.Errorf("Write() clobbered fn.go: %q", b)
	}
	if _, err := os.Stat(filepath.Join(dir, "main.go")); err == nil {
		t.Error("Write() wrote main.go although it refused the project")
	}
}

func readTree(t *testing.T, dir string) map[string][]byte {
	t.Helper()
	files := map[string][]byte{}
	err := filepath.WalkDir(dir, func(p string, d os.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return err
		}
		rel, err := filepath.Rel(dir, p)
		if err != nil {
			return err
		}
		files[filepath.ToSlash(rel)], err = os.ReadFile(p)
		return err
	})
	if err != nil {
		t.Fatalf("cannot read %s: %v", dir, err)
	}
	return files
}

func names(m map[string][]byte) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}
