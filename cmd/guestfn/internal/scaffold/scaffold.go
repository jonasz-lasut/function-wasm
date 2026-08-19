// Package scaffold renders a new guest project in one of three flavours: Go
// with function-sdk-go (its ABI glue vendored in internal/wasmfn), TinyGo over
// generated protobuf messages, or Rust with prost. Each template set is the
// matching example guest of this repository (examples/hello-go, hello-tinygo,
// hello-rust) with the module path and name parameterised; tests keep them
// identical.
package scaffold

import (
	"bytes"
	"embed"
	"fmt"
	"io/fs"
	"os"
	"path"
	"path/filepath"
	"slices"
	"strings"
	"text/template"
)

//go:embed all:templates
var templates embed.FS

// Languages a scaffold can be rendered in.
const (
	LangGo     = "go"
	LangTinyGo = "tinygo"
	LangRust   = "rust"
)

// Langs lists the supported languages, default first.
var Langs = []string{LangGo, LangTinyGo, LangRust}

// Options parameterise a scaffold.
type Options struct {
	// Lang selects the template set; empty means LangGo.
	Lang string
	// Module is the Go module path of the guest, e.g. github.com/me/my-fn.
	// Required for Go and TinyGo; Rust projects have no module path.
	Module string
	// Name is the guest's short name: the crate name for Rust, and what docs
	// and the example Composition call the guest. Empty derives it from the
	// last element of Module.
	Name string
	// GoVersion is the go directive of the generated go.mod, e.g. 1.26.6.
	GoVersion string
	// SDKVersion is the function-sdk-go version to require.
	SDKVersion string
	// Requires controls whether go.mod carries the require block at all; when
	// false the caller is expected to run go get, which is what guestfn init
	// does unless asked to stay offline.
	Requires bool
}

// Render returns the files of the scaffold keyed by their path relative to
// the project root.
func Render(o Options) (map[string][]byte, error) {
	if o.Lang == "" {
		o.Lang = LangGo
	}
	if !slices.Contains(Langs, o.Lang) {
		return nil, fmt.Errorf("unsupported language %q; one of %s", o.Lang, strings.Join(Langs, ", "))
	}
	if o.Module == "" && o.Lang != LangRust {
		return nil, fmt.Errorf("a module path is required")
	}
	if o.Name == "" {
		if o.Module == "" {
			return nil, fmt.Errorf("a name is required")
		}
		o.Name = path.Base(o.Module)
	}
	root := "templates/" + o.Lang
	files := map[string][]byte{}
	err := fs.WalkDir(templates, root, func(p string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return err
		}
		content, err := templates.ReadFile(p)
		if err != nil {
			return err
		}
		rel := strings.TrimPrefix(p, root+"/")
		if strings.HasSuffix(rel, ".tmpl") {
			rendered, err := render(rel, content, o)
			if err != nil {
				return err
			}
			content = rendered
			rel = strings.TrimSuffix(rel, ".tmpl")
		}
		files[rel] = content
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("cannot render templates: %w", err)
	}
	return files, nil
}

// render executes a template with [[ ]] delimiters, so Go source in the
// templates keeps its braces.
func render(name string, content []byte, o Options) ([]byte, error) {
	t, err := template.New(name).Delims("[[", "]]").Option("missingkey=error").Parse(string(content))
	if err != nil {
		return nil, fmt.Errorf("cannot parse template %s: %w", name, err)
	}
	var buf bytes.Buffer
	if err := t.Execute(&buf, o); err != nil {
		return nil, fmt.Errorf("cannot render template %s: %w", name, err)
	}
	return buf.Bytes(), nil
}

// Write writes rendered files under dir, creating it. It refuses to overwrite
// existing files so a typo cannot clobber a project.
func Write(dir string, files map[string][]byte) error {
	for rel := range files {
		if _, err := os.Stat(filepath.Join(dir, rel)); err == nil {
			return fmt.Errorf("%s already exists", filepath.Join(dir, rel))
		}
	}
	for rel, content := range files {
		full := filepath.Join(dir, rel)
		if err := os.MkdirAll(filepath.Dir(full), 0o750); err != nil {
			return fmt.Errorf("cannot create %s: %w", filepath.Dir(full), err)
		}
		if err := os.WriteFile(full, content, 0o644); err != nil { //nolint:gosec // Project sources the user asked for, readable like any checkout.
			return fmt.Errorf("cannot write %s: %w", full, err)
		}
	}
	return nil
}
