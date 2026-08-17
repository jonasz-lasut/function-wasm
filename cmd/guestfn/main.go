// Package main implements guestfn, the CLI that scaffolds, builds, inspects
// and publishes guest modules for function-wasm. It checks modules with the
// runtime's own engine (wasmtime), so its verdicts are the runtime's; that
// makes it a CGo binary like the runtime.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strconv"
	"strings"
	"time"

	"github.com/alecthomas/kong"
	"github.com/google/go-containerregistry/pkg/authn"
	"github.com/google/go-containerregistry/pkg/name"
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/partial"
	"github.com/google/go-containerregistry/pkg/v1/remote"
	"github.com/google/go-containerregistry/pkg/v1/static"
	"github.com/google/go-containerregistry/pkg/v1/types"
	"golang.org/x/mod/module"
	"golang.org/x/mod/semver"
	k8sruntime "k8s.io/apimachinery/pkg/runtime"
	"sigs.k8s.io/yaml"

	"github.com/jonasz-lasut/function-wasm/cmd/guestfn/internal/scaffold"
	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
)

const (
	sdkModule    = "github.com/crossplane/function-sdk-go"
	wasmfnModule = "github.com/jonasz-lasut/function-wasm/pkg/wasmfn"

	// The Input's identity in a Composition step.
	inputAPIVersion = "wasm.fn.crossplane.io/v1beta1"
	inputKind       = "Input"

	// The CNCF wasm OCI artifact layout: a wasm config and one raw wasm layer.
	wasmConfigMediaType types.MediaType = "application/vnd.wasm.config.v0+json"
	wasmLayerMediaType  types.MediaType = "application/wasm"

	// fallbackSDKVersion is used when the binary carries no build info (go
	// test), so a scaffold is never left without a version.
	fallbackSDKVersion = "v0.7.1"
)

// CLI is the guestfn command line.
type CLI struct {
	Init     InitCmd     `cmd:"" help:"Scaffold a new guest project."`
	Build    BuildCmd    `cmd:"" help:"Compile a guest project to a wasip1 module, check its ABI and its manifest (wasmfn.yaml)."`
	Push     PushCmd     `cmd:"" help:"Push a module to an OCI registry as a wasm artifact; a module the runtime would refuse is not pushed."`
	Inspect  InspectCmd  `cmd:"" help:"Show what a module (or an artifact in a registry) is made of, as the runtime sees it: size, ABI verdict, exports, imports, memories, manifest."`
	Manifest ManifestCmd `cmd:"" help:"Validate a module manifest (wasmfn.yaml) or print the one an artifact carries."`
	Scaffold ScaffoldCmd `cmd:"" help:"Print a Composition step (or a Composition) for a module, from its manifest."`

	Version kong.VersionFlag `help:"Print the version and exit."`
}

// InitCmd scaffolds a guest project.
type InitCmd struct {
	Dir string `arg:"" help:"Directory to create the project in."`

	Lang          string `help:"Language of the project: go (function-sdk-go + wasmfn), tinygo (raw protobuf messages, ~1 MB modules) or rust (prost)." enum:"go,tinygo,rust" default:"go"`
	Module        string `help:"Go module path of the project (go, tinygo). Defaults to the directory's base name."`
	Name          string `help:"Short name used in docs and the example Composition, and the crate name for rust. Defaults to the module's last element or the directory's base name."`
	SDKVersion    string `help:"function-sdk-go version to require (go)." default:"${sdk_version}"`
	WasmfnVersion string `help:"wasmfn guest SDK version to require (go)." default:"${wasmfn_version}"`
	WasmfnDir     string `help:"Use a local checkout of the wasmfn SDK through a replace directive (go; for developing the SDK)." type:"existingdir"`
	Offline       bool   `help:"Do not run go get / go mod tidy; go writes go.mod from the given versions."`
}

// Run scaffolds the project and resolves its dependencies.
func (c *InitCmd) Run(ctx context.Context, stdout io.Writer) error {
	if c.Lang == "" {
		c.Lang = scaffold.LangGo
	}
	base := filepath.Base(filepath.Clean(c.Dir))
	module, name := c.Module, c.Name
	if c.Lang == scaffold.LangRust {
		module = ""
		if name == "" {
			name = base
		}
	} else if module == "" {
		module = base
	}
	if c.Lang == scaffold.LangGo && c.Offline && c.WasmfnVersion == "latest" && c.WasmfnDir == "" {
		return fmt.Errorf("--offline needs --wasmfn-version (or --wasmfn-dir): the SDK version cannot be resolved without go get")
	}
	wasmfnDir := c.WasmfnDir
	if wasmfnDir != "" {
		abs, err := filepath.Abs(wasmfnDir)
		if err != nil {
			return err
		}
		wasmfnDir = abs
	}
	files, err := scaffold.Render(scaffold.Options{
		Lang:          c.Lang,
		Module:        module,
		Name:          name,
		GoVersion:     goVersion(),
		SDKVersion:    c.SDKVersion,
		WasmfnVersion: c.WasmfnVersion,
		Requires:      c.Offline,
		WasmfnDir:     wasmfnDir,
	})
	if err != nil {
		return err
	}
	if err := scaffold.Write(c.Dir, files); err != nil {
		return err
	}
	if c.Lang == scaffold.LangRust {
		_, _ = fmt.Fprintf(stdout, "Created %s (crate %s)\n", c.Dir, name)
	} else {
		_, _ = fmt.Fprintf(stdout, "Created %s (module %s)\n", c.Dir, module)
	}

	if !c.Offline && c.Lang != scaffold.LangRust {
		if c.Lang == scaffold.LangGo {
			gets := []string{sdkModule + "@" + c.SDKVersion}
			if wasmfnDir == "" {
				gets = append(gets, wasmfnModule+"@"+c.WasmfnVersion)
			}
			if err := run(ctx, c.Dir, stdout, "go", append([]string{"get"}, gets...)...); err != nil {
				return err
			}
		}
		if err := run(ctx, c.Dir, stdout, "go", "mod", "tidy"); err != nil {
			return err
		}
	}
	test := "go test ./...        # edit fn.go, keep the tests passing"
	if c.Lang == scaffold.LangRust {
		test = "cargo test           # edit src/lib.rs, keep the tests passing"
	}
	_, _ = fmt.Fprintf(stdout, "\nNext:\n  cd %s\n  %s\n  guestfn build        # fn.wasm\n  guestfn push <ref>   # publish, then reference the digest from a Composition\n", c.Dir, test)
	return nil
}

// BuildCmd compiles a guest.
type BuildCmd struct {
	Dir     string `help:"Project directory." default:"." type:"existingdir"`
	Output  string `short:"o" help:"Output file, relative to the project directory unless absolute." default:"fn.wasm"`
	Lang    string `help:"Toolchain to use. auto picks rust for a Cargo.toml, tinygo for a go.mod that requires vtprotobuf but not wasmfn, go otherwise." enum:"auto,go,tinygo,rust" default:"auto"`
	WasmOpt bool   `help:"Run wasm-opt -Oz on the result (binaryen must be on PATH)."`
}

// Run compiles the project as a wasip1 reactor with the toolchain of its
// language: Go and TinyGo build -buildmode=c-shared, which exports _initialize
// and lets //go:wasmexport functions be called by the host; Rust builds a
// cdylib for wasm32-wasip1. The result is checked as the runtime would at
// load, and the project's wasmfn.yaml, if any, is validated as the manifest
// guestfn push will publish beside it.
func (c *BuildCmd) Run(ctx context.Context, stdout io.Writer) error {
	out := c.Output
	if !filepath.IsAbs(out) {
		out = filepath.Join(c.Dir, out)
	}
	lang := c.Lang
	if lang == "" || lang == "auto" {
		detected, err := detectLang(c.Dir)
		if err != nil {
			return err
		}
		lang = detected
	}
	if err := buildGuest(ctx, lang, c.Dir, out, stdout); err != nil {
		return err
	}
	if c.WasmOpt {
		if err := run(ctx, c.Dir, stdout, "wasm-opt", "-Oz", "-o", out, out); err != nil {
			return err
		}
	}
	wasm, err := os.ReadFile(out) //nolint:gosec // The file the build just wrote.
	if err != nil {
		return err
	}
	// The manifest is checked with the build so a broken wasmfn.yaml fails
	// here rather than at push time.
	var m *manifest.Manifest
	if path := filepath.Join(c.Dir, manifest.FileName); fileExists(path) {
		if m, err = manifest.Load(path); err != nil {
			return err
		}
	}
	// The runtime's own load-time check, so a module it would refuse is
	// refused here, with the same words, before it is pushed anywhere.
	shape, err := checkModule(wasm)
	if err != nil {
		return fmt.Errorf("built %s, but the runtime would refuse it: %w", out, err)
	}
	line := fmt.Sprintf("Built %s (%s, ABI v1%s", out, humanBytes(int64(len(wasm))), importsSuffix(shape))
	if m != nil {
		line += "; manifest: " + m.Summary()
	}
	_, _ = fmt.Fprintln(stdout, line+")")
	if m != nil {
		warnExampleConfig(stdout, c.Dir, m)
	}
	return nil
}

// warnExampleConfig holds the scaffold's example Composition config against
// the manifest's schema, when both exist: a mismatch is a warning, not a
// failed build - the example is documentation, the schema is the contract.
func warnExampleConfig(stdout io.Writer, dir string, m *manifest.Manifest) {
	path := filepath.Join(dir, "example", "composition.yaml")
	if !fileExists(path) || m.Config == nil || len(m.Config.Schema) == 0 {
		return
	}
	config, ok, err := exampleConfig(path)
	if err != nil {
		_, _ = fmt.Fprintf(stdout, "warning: cannot read %s: %v\n", path, err)
		return
	}
	if !ok {
		return
	}
	if err := m.ValidateConfig(config); err != nil {
		_, _ = fmt.Fprintf(stdout, "warning: %s: %v\n", path, err)
	}
}

// exampleConfig returns the config block of the first function-wasm step of
// a Composition file, as the runtime receives it; ok is false without such a
// step.
func exampleConfig(path string) (config *k8sruntime.RawExtension, ok bool, err error) {
	raw, err := os.ReadFile(path) //nolint:gosec // The project's own example.
	if err != nil {
		return nil, false, err
	}
	var doc struct {
		Spec struct {
			Pipeline []struct {
				Input map[string]any `json:"input"`
			} `json:"pipeline"`
		} `json:"spec"`
	}
	if err := yaml.Unmarshal(raw, &doc); err != nil {
		return nil, false, err
	}
	for _, step := range doc.Spec.Pipeline {
		if step.Input["apiVersion"] != inputAPIVersion || step.Input["kind"] != inputKind {
			continue
		}
		cfg, ok := step.Input["config"]
		if !ok {
			return nil, true, nil
		}
		js, err := json.Marshal(cfg)
		if err != nil {
			return nil, false, err
		}
		return &k8sruntime.RawExtension{Raw: js}, true, nil
	}
	return nil, false, nil
}

func fileExists(path string) bool {
	st, err := os.Stat(path)
	return err == nil && !st.IsDir()
}

// checkModule compiles wasm with the runtime's own engine — wasmtime is the
// only reader of a module, and a compile is what its verdict costs — and
// returns the shape, or the refusal the runtime would give at load.
func checkModule(wasm []byte) (*engine.Shape, error) {
	shape, err := inspectModule(wasm)
	if err != nil {
		return nil, err
	}
	if shape.ABIError != nil {
		return nil, shape.ABIError
	}
	return shape, nil
}

// inspectModule compiles wasm with a throwaway engine and reports its shape,
// ABI verdict included.
func inspectModule(wasm []byte) (*engine.Shape, error) {
	eng, err := engine.New(engine.Config{})
	if err != nil {
		return nil, err
	}
	defer eng.Close()
	return eng.Inspect(wasm)
}

// importsSuffix names the host imports a module uses, for the build line.
func importsSuffix(shape *engine.Shape) string {
	imports := shape.HostImports()
	if len(imports) == 0 {
		return ""
	}
	return ", imports " + strings.Join(imports, " ")
}

// detectLang tells the language of a project from its files: a Cargo.toml is
// Rust; a go.mod that requires vtprotobuf but not the wasmfn SDK is the TinyGo
// flavour (that is what its generated codecs are for); any other go.mod is Go.
func detectLang(dir string) (string, error) {
	if _, err := os.Stat(filepath.Join(dir, "Cargo.toml")); err == nil {
		return scaffold.LangRust, nil
	}
	gomod, err := os.ReadFile(filepath.Join(dir, "go.mod")) //nolint:gosec // The user's own project directory.
	if err != nil {
		return "", fmt.Errorf("cannot tell the project's language: neither Cargo.toml nor go.mod in %s (use --lang)", dir)
	}
	if strings.Contains(string(gomod), "github.com/planetscale/vtprotobuf") && !strings.Contains(string(gomod), wasmfnModule) {
		return scaffold.LangTinyGo, nil
	}
	return scaffold.LangGo, nil
}

// buildGuest runs the language's compiler and leaves the module at out.
func buildGuest(ctx context.Context, lang, dir, out string, stdout io.Writer) error {
	switch lang {
	case scaffold.LangGo:
		cmd := exec.CommandContext(ctx, "go", "build", "-buildmode=c-shared", "-trimpath", "-ldflags=-s -w", "-o", out, ".") //nolint:gosec // The output path is the user's own flag.
		cmd.Dir = dir
		cmd.Env = append(os.Environ(), "GOOS=wasip1", "GOARCH=wasm")
		cmd.Stdout, cmd.Stderr = stdout, os.Stderr
		if err := cmd.Run(); err != nil {
			return fmt.Errorf("go build failed: %w", err)
		}
	case scaffold.LangTinyGo:
		if _, err := exec.LookPath("tinygo"); err != nil {
			return errors.New("tinygo not found on PATH: install it from https://tinygo.org/getting-started/install/")
		}
		if err := run(ctx, dir, stdout, "tinygo", "build", "-target=wasip1", "-buildmode=c-shared", "-no-debug", "-o", out, "."); err != nil {
			return err
		}
	case scaffold.LangRust:
		if _, err := exec.LookPath("cargo"); err != nil {
			return errors.New("cargo not found on PATH: install Rust from https://rustup.rs and run rustup target add wasm32-wasip1")
		}
		if err := run(ctx, dir, stdout, "cargo", "build", "--release", "--target", "wasm32-wasip1"); err != nil {
			return err
		}
		matches, err := filepath.Glob(filepath.Join(dir, "target", "wasm32-wasip1", "release", "*.wasm"))
		if err != nil {
			return err
		}
		if len(matches) != 1 {
			return fmt.Errorf("expected one .wasm under target/wasm32-wasip1/release, found %d", len(matches))
		}
		wasm, err := os.ReadFile(matches[0])
		if err != nil {
			return err
		}
		if err := os.WriteFile(out, wasm, 0o600); err != nil { //nolint:gosec // out is the user's own --output flag.
			return err
		}
	default:
		return fmt.Errorf("unsupported language %q", lang)
	}
	return nil
}

// PushCmd publishes a module.
type PushCmd struct {
	Ref           string `arg:"" help:"Reference to push to, e.g. ghcr.io/me/my-fn:v0.1.0."`
	File          string `short:"f" help:"Module to push." default:"fn.wasm" type:"existingfile"`
	Manifest      string `help:"The module manifest to publish beside the module as the artifact's second layer; by default the wasmfn.yaml next to the module file, when there is one." placeholder:"wasmfn.yaml"`
	ModuleVersion string `help:"Module version to record in the published manifest, overriding the file's version."`
	Revision      string `help:"Source revision to record as org.opencontainers.image.revision on the artifact."`
}

// OCI annotation keys mirrored from the manifest onto the artifact.
const (
	annotationTitle       = "org.opencontainers.image.title"
	annotationVersion     = "org.opencontainers.image.version"
	annotationSource      = "org.opencontainers.image.source"
	annotationDescription = "org.opencontainers.image.description"
	annotationRevision    = "org.opencontainers.image.revision"
)

// Run pushes the module as an OCI artifact — the module layer and, when the
// project has a wasmfn.yaml, the manifest layer beside it — and prints what
// a Composition needs: the reference pinned to the OCI manifest digest, with
// the tag it was pushed to kept for readability, and the sandbox block the
// module's manifest requires, if any.
func (c *PushCmd) Run(ctx context.Context, stdout io.Writer) error {
	wasm, err := os.ReadFile(c.File)
	if err != nil {
		return err
	}
	// A module the runtime would refuse at load is not published: pushing it
	// only moves the refusal into an XR condition. Modules for other hosts
	// are oras push's business.
	if _, err := checkModule(wasm); err != nil {
		return fmt.Errorf("%s would be refused by the runtime and is not pushed: %w (guestfn push publishes function-wasm modules; use oras push for other artifacts)", c.File, err)
	}
	m, err := c.manifest()
	if err != nil {
		return err
	}
	ref, err := name.ParseReference(c.Ref)
	if err != nil {
		return fmt.Errorf("cannot parse reference: %w", err)
	}
	var manifestJSON []byte
	if m != nil {
		if manifestJSON, err = m.JSON(); err != nil {
			return err
		}
	}
	// A fixed creation time makes the artifact — and so the manifest digest
	// a Composition pins and the caches key on — a function of the module
	// bytes, the manifest and the annotations alone: pushing the same
	// fn.wasm twice yields the same reference. SOURCE_DATE_EPOCH overrides
	// it, as for any reproducible build.
	img, err := artifact(wasm, creationTime(), manifestJSON, manifestAnnotations(m, c.Revision))
	if err != nil {
		return err
	}
	if err := remote.Write(ref, img, remote.WithAuthFromKeychain(authn.DefaultKeychain), remote.WithContext(ctx)); err != nil {
		return fmt.Errorf("cannot push %s: %w", ref, err)
	}
	digest, err := img.Digest()
	if err != nil {
		return err
	}
	pinned := ref.String()
	if _, ok := ref.(name.Digest); !ok {
		pinned += "@" + digest.String()
	}
	_, _ = fmt.Fprintf(stdout, "Pushed %s\n\nmodule:\n  type: OCI\n  oci:\n    ref: %s\n", pinned, pinned)
	if m != nil {
		block, err := sandboxBlock(m)
		if err != nil {
			return err
		}
		_, _ = fmt.Fprint(stdout, block)
	}
	return nil
}

// manifest loads the manifest to publish: --manifest, or the wasmfn.yaml
// next to the module file when there is one; nil without either. The
// version override applies before it is published.
func (c *PushCmd) manifest() (*manifest.Manifest, error) {
	path := c.Manifest
	if path == "" {
		candidate := filepath.Join(filepath.Dir(c.File), manifest.FileName)
		if !fileExists(candidate) {
			if c.ModuleVersion != "" {
				return nil, fmt.Errorf("--module-version needs a manifest: no %s next to %s and no --manifest", manifest.FileName, c.File)
			}
			return nil, nil //nolint:nilnil // No manifest is the common case, not an error.
		}
		path = candidate
	}
	m, err := manifest.Load(path)
	if err != nil {
		return nil, err
	}
	if c.ModuleVersion != "" {
		m.Version = c.ModuleVersion
	}
	return m, nil
}

// manifestAnnotations maps the manifest onto the standard OCI image
// annotations of the artifact; revision comes from the flag. Nil when there
// is nothing to record. The manifest itself is a layer, not an annotation.
func manifestAnnotations(m *manifest.Manifest, revision string) map[string]string {
	annotations := map[string]string{}
	if m != nil {
		for key, value := range map[string]string{
			annotationTitle: m.Name, annotationVersion: m.Version, annotationSource: m.Source, annotationDescription: m.Description,
		} {
			if value != "" {
				annotations[key] = value
			}
		}
	}
	if revision != "" {
		annotations[annotationRevision] = revision
	}
	if len(annotations) == 0 {
		return nil
	}
	return annotations
}

// sandboxBlock renders the sandbox a Composition must grant for the
// manifest's requirements, as the YAML fragment that follows the module
// block; empty when the module requires nothing.
func sandboxBlock(m *manifest.Manifest) (string, error) {
	sb := m.Sandbox()
	if sb == nil {
		return "", nil
	}
	out, err := yaml.Marshal(struct {
		Sandbox *v1beta1.Sandbox `json:"sandbox"`
	}{sb})
	if err != nil {
		return "", err
	}
	return string(out), nil
}

// creationTime is SOURCE_DATE_EPOCH when set and valid, the Unix epoch
// otherwise.
func creationTime() time.Time {
	if v := os.Getenv("SOURCE_DATE_EPOCH"); v != "" {
		if secs, err := strconv.ParseInt(v, 10, 64); err == nil {
			return time.Unix(secs, 0).UTC()
		}
	}
	return time.Unix(0, 0).UTC()
}

// wasmConfig is the artifact's config as the CNCF wasm OCI artifact
// specification lists it: created, architecture, os and the digests of the
// layers.
type wasmConfig struct {
	Created      string   `json:"created"`
	Architecture string   `json:"architecture"`
	OS           string   `json:"os"`
	LayerDigests []string `json:"layerDigests"`
}

// artifact wraps a module in the CNCF wasm OCI artifact layout: the
// application/wasm layer, then — when manifestJSON is set — the module
// manifest as a second layer of manifest.LayerMediaType, a wasm config naming
// every layer in layerDigests, and the OCI annotations. The OCI manifest is
// a function of the module bytes, the manifest, created and annotations
// alone.
func artifact(wasm []byte, created time.Time, manifestJSON []byte, annotations map[string]string) (v1.Image, error) {
	layers := []v1.Layer{static.NewLayer(wasm, wasmLayerMediaType)}
	if len(manifestJSON) > 0 {
		layers = append(layers, static.NewLayer(manifestJSON, types.MediaType(manifest.LayerMediaType)))
	}
	descriptors := make([]v1.Descriptor, 0, len(layers))
	digests := make([]string, 0, len(layers))
	for _, l := range layers {
		digest, err := l.Digest()
		if err != nil {
			return nil, err
		}
		size, err := l.Size()
		if err != nil {
			return nil, err
		}
		mediaType, err := l.MediaType()
		if err != nil {
			return nil, err
		}
		descriptors = append(descriptors, v1.Descriptor{MediaType: mediaType, Size: size, Digest: digest})
		digests = append(digests, digest.String())
	}
	config, err := json.Marshal(wasmConfig{
		Created:      created.UTC().Format(time.RFC3339),
		Architecture: "wasm",
		OS:           "wasip1",
		LayerDigests: digests,
	})
	if err != nil {
		return nil, err
	}
	configDigest, configSize, err := v1.SHA256(bytes.NewReader(config))
	if err != nil {
		return nil, err
	}
	rawManifest, err := json.Marshal(v1.Manifest{
		SchemaVersion: 2,
		MediaType:     types.OCIManifestSchema1,
		Config:        v1.Descriptor{MediaType: wasmConfigMediaType, Size: configSize, Digest: configDigest},
		Layers:        descriptors,
		Annotations:   annotations,
	})
	if err != nil {
		return nil, err
	}
	return partial.CompressedToImage(&wasmArtifact{config: config, manifest: rawManifest, layers: layers})
}

// wasmArtifact is the minimal image core the artifact needs: raw config and
// manifest bytes written as they are, and the layers.
type wasmArtifact struct {
	config   []byte
	manifest []byte
	layers   []v1.Layer
}

func (a *wasmArtifact) RawConfigFile() ([]byte, error)      { return a.config, nil }
func (a *wasmArtifact) MediaType() (types.MediaType, error) { return types.OCIManifestSchema1, nil }
func (a *wasmArtifact) RawManifest() ([]byte, error)        { return a.manifest, nil }

func (a *wasmArtifact) LayerByDigest(h v1.Hash) (partial.CompressedLayer, error) {
	for _, l := range a.layers {
		d, err := l.Digest()
		if err != nil {
			return nil, err
		}
		if d == h {
			return l, nil
		}
	}
	return nil, fmt.Errorf("no layer %s in the artifact", h)
}

func run(ctx context.Context, dir string, stdout io.Writer, name string, args ...string) error {
	cmd := exec.CommandContext(ctx, name, args...) //nolint:gosec // guestfn runs the go toolchain and wasm-opt on the user's behalf.
	cmd.Dir = dir
	cmd.Stdout, cmd.Stderr = stdout, os.Stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("%s %s failed: %w", name, strings.Join(args, " "), err)
	}
	return nil
}

// goVersion is the go directive for scaffolds: the toolchain building
// guestfn, e.g. 1.26.6.
func goVersion() string {
	v := strings.TrimPrefix(runtime.Version(), "go")
	if i := strings.IndexAny(v, " -"); i >= 0 {
		v = v[:i]
	}
	return v
}

// versions reads the CLI's own version and the function-sdk-go version it was
// built with, which become the defaults a scaffold requires. wasmfn is tagged
// in lockstep with guestfn, so a released guestfn pins the matching SDK; a
// development build asks go get for the latest.
func versions() (cli, sdk, wasmfn string) {
	cli, sdk, wasmfn = "(devel)", fallbackSDKVersion, "latest"
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		return cli, sdk, wasmfn
	}
	if bi.Main.Version != "" {
		cli = bi.Main.Version
	}
	if semver.IsValid(cli) && !module.IsPseudoVersion(cli) {
		wasmfn = cli
	}
	for _, d := range bi.Deps {
		if d.Path == sdkModule && d.Version != "" {
			sdk = d.Version
		}
	}
	return cli, sdk, wasmfn
}

// parser builds the kong parser with the bindings the commands need; main
// and the tests share it so a binding mistake surfaces in tests.
func parser(stdout io.Writer) *kong.Kong {
	cli, sdk, wasmfn := versions()
	return kong.Must(&CLI{},
		kong.Name("guestfn"),
		kong.Description("Scaffold, build and publish WebAssembly guest functions for function-wasm."),
		kong.Vars{"version": cli, "sdk_version": sdk, "wasmfn_version": wasmfn},
		kong.BindTo(stdout, (*io.Writer)(nil)),
		kong.BindTo(context.Background(), (*context.Context)(nil)),
	)
}

func main() {
	p := parser(os.Stdout)
	ctx, err := p.Parse(os.Args[1:])
	p.FatalIfErrorf(err)
	p.FatalIfErrorf(ctx.Run())
}
