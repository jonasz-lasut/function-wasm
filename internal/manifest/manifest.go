// Package manifest is the module manifest of docs/one-pager-module-manifest.md:
// what a module declares about itself - the sandbox grants it cannot run
// without, the JSON Schema of the config it reads, its ABI and the oldest
// runtime it works on - carried as a layer of the module's OCI artifact
// (JSON, at most MaxSize; media type LayerMediaType) beside the module
// layer, so it is covered by the manifest digest a Composition pins and by
// a cosign signature. guestfn writes it from wasmfn.yaml (Load) and pushes
// it; the runtime and guestfn read it back (Parse) and hold it against the
// grant a Composition made (Check). A manifest is a requirement, never a
// grant: it can make a run fail earlier and say why, it cannot make a run
// possible. Path and HTTP sources carry no manifest.
package manifest

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"runtime/debug"
	"strings"

	"github.com/santhosh-tekuri/jsonschema/v6"
	"golang.org/x/mod/semver"
	"golang.org/x/text/language"
	"golang.org/x/text/message"
	"k8s.io/apimachinery/pkg/runtime"
	"sigs.k8s.io/yaml"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

const (
	// LayerMediaType is the media type of the artifact layer that carries the
	// manifest as JSON, beside the application/wasm layer.
	LayerMediaType = "application/vnd.wasmfn.manifest.v1+json"
	// FileName is the source file in a guest project guestfn build reads.
	FileName = "wasmfn.yaml"
	// MaxSize bounds the layer and the file: a manifest is a few lines of
	// requirements and a schema, never a document.
	MaxSize = 64 << 10
	// ABIVersion is the ABI this runtime implements (docs/abi.md).
	ABIVersion = 1

	// schemaURL is the resource name the inline config schema is compiled
	// under; a $ref that leaves it has nowhere to go.
	schemaURL = "wasmfn:///config.schema.json"
)

// Manifest is what a module declares about itself.
type Manifest struct {
	// ABI is the ABI version the module implements; required, must be
	// ABIVersion.
	ABI int `json:"abi"`
	// Name of the module (org.opencontainers.image.title in the artifact).
	Name string `json:"name,omitempty"`
	// Version of the module (org.opencontainers.image.version).
	Version string `json:"version,omitempty"`
	// Source is where the module's code lives
	// (org.opencontainers.image.source).
	Source string `json:"source,omitempty"`
	// Description of the module (org.opencontainers.image.description).
	Description string `json:"description,omitempty"`
	// Requires are the grants the module cannot run without.
	Requires *Requires `json:"requires,omitempty"`
	// Config describes the Input's config block.
	Config *Config `json:"config,omitempty"`
	// MinRuntime is the oldest function-wasm runtime that serves this
	// module, as a semantic version ("v0.2.0" or "0.2.0").
	MinRuntime string `json:"minRuntime,omitempty"`

	// schema is Config.Schema compiled once, by Validate.
	schema *jsonschema.Schema
}

// Requires are the sandbox grants a module needs: each is optional, each
// must be covered by the Composition's grant for the module to run.
type Requires struct {
	// Egress the module needs, in the Input's own shape.
	Egress *Egress `json:"egress,omitempty"`
	// Filesystem: {privateTmp: true} when the module writes to /tmp.
	Filesystem *v1beta1.SandboxFilesystem `json:"filesystem,omitempty"`
	// Environment variables are deliberately not a requirement: they are
	// values the Composition sets (and, one day, the request delivers), not
	// a capability the module needs granted.
}

// Egress is the HTTP egress a module needs: the same rule type the Input's
// sandbox.egress.http carries, so a requirement compares like with like and
// copies verbatim into a Composition.
type Egress struct {
	HTTP []v1beta1.SandboxHTTPRule `json:"http,omitempty"`
}

// Config describes the module's config block.
type Config struct {
	// Schema is a JSON Schema (draft 2020-12) the Input's config must
	// satisfy, inline: no $ref to a URL is followed.
	Schema json.RawMessage `json:"schema,omitempty"`
}

// Grants are what one run was granted - the Composition's sandbox already
// admitted by the operator's ceiling - held against Requires by Check.
type Grants struct {
	PrivateTmp bool
	HTTP       []v1beta1.SandboxHTTPRule
}

// Load reads a wasmfn.yaml: YAML, decoded strictly (a field the runtime does
// not know is a typo here, not forward compatibility), then validated.
func Load(path string) (*Manifest, error) {
	raw, err := os.ReadFile(path) //nolint:gosec // The user's own project file.
	if err != nil {
		return nil, fmt.Errorf("cannot read manifest: %w", err)
	}
	if len(raw) > MaxSize {
		return nil, fmt.Errorf("manifest %s is %d bytes, the limit is %d", path, len(raw), MaxSize)
	}
	m := &Manifest{}
	if err := yaml.UnmarshalStrict(raw, m); err != nil {
		return nil, fmt.Errorf("cannot parse manifest %s: %w", path, err)
	}
	if err := m.Validate(); err != nil {
		return nil, fmt.Errorf("manifest %s: %w", path, err)
	}
	return m, nil
}

// Parse decodes the layer's payload: JSON, at most
// MaxSize. Unknown top-level fields are ignored, so a module built by a newer
// guestfn still loads; an unknown field anywhere under requires is refused,
// so a requirement this runtime cannot honour fails closed rather than being
// silently dropped. The result is validated.
func Parse(raw []byte) (*Manifest, error) {
	if len(raw) > MaxSize {
		return nil, fmt.Errorf("manifest is %d bytes, the limit is %d", len(raw), MaxSize)
	}
	m := &Manifest{}
	if err := json.Unmarshal(raw, m); err != nil {
		return nil, fmt.Errorf("cannot parse manifest: %w", err)
	}
	// The strict pass over requires alone: decode the raw object again,
	// keeping only that field, with unknown fields refused.
	var top struct {
		Requires json.RawMessage `json:"requires"`
	}
	if err := json.Unmarshal(raw, &top); err != nil {
		return nil, fmt.Errorf("cannot parse manifest: %w", err)
	}
	if len(top.Requires) > 0 && string(top.Requires) != "null" {
		dec := json.NewDecoder(bytes.NewReader(top.Requires))
		dec.DisallowUnknownFields()
		if err := dec.Decode(&Requires{}); err != nil {
			return nil, fmt.Errorf("cannot parse manifest requires: %w", err)
		}
	}
	if err := m.Validate(); err != nil {
		return nil, fmt.Errorf("invalid manifest: %w", err)
	}
	return m, nil
}

// Validate checks the manifest's shape and compiles its schema: abi is
// ABIVersion, every required egress rule passes the checks a Composition's
// rule passes, minRuntime is a semantic version, config.schema is a JSON
// Schema that refers to nothing outside itself.
func (m *Manifest) Validate() error {
	if m.ABI != ABIVersion {
		return fmt.Errorf("abi must be %d (this runtime implements ABI v%d), got %d", ABIVersion, ABIVersion, m.ABI)
	}
	if m.Requires != nil {
		if m.Requires.Egress != nil {
			if err := sandbox.ValidateRules("requires.egress.http", m.Requires.Egress.HTTP); err != nil {
				return err
			}
		}
	}
	if m.MinRuntime != "" && !semver.IsValid(canonical(m.MinRuntime)) {
		return fmt.Errorf("minRuntime %q is not a semantic version (e.g. v0.2.0)", m.MinRuntime)
	}
	if m.hasConfigSchema() {
		if err := m.ensureSchema(); err != nil {
			return err
		}
	}
	return nil
}

// hasConfigSchema reports whether the manifest carries an inline config
// schema: a Config block whose Schema is non-empty and not the JSON null.
func (m *Manifest) hasConfigSchema() bool {
	return m.Config != nil && len(m.Config.Schema) > 0 && string(m.Config.Schema) != "null"
}

// ensureSchema compiles the inline config schema into m.schema. Callers gate
// it on hasConfigSchema, so m.Config.Schema is present here.
func (m *Manifest) ensureSchema() error {
	schema, err := compile(m.Config.Schema)
	if err != nil {
		return err
	}
	m.schema = schema
	return nil
}

// JSON renders the manifest as the compact JSON the layer carries.
func (m *Manifest) JSON() ([]byte, error) {
	b, err := json.Marshal(m)
	if err != nil {
		return nil, fmt.Errorf("cannot encode manifest: %w", err)
	}
	return b, nil
}

// Summary is one line for a CLI: name and version, then what the module
// requires and whether it ships a config schema; empty parts are left out.
func (m *Manifest) Summary() string {
	var head []string
	if m.Name != "" {
		head = append(head, m.Name)
	}
	if m.Version != "" {
		head = append(head, m.Version)
	}
	var parts []string
	if h := strings.Join(head, " "); h != "" {
		parts = append(parts, h)
	}
	if r := m.Requires; r != nil {
		if r.Egress != nil && len(r.Egress.HTTP) > 0 {
			hosts := make([]string, 0, len(r.Egress.HTTP))
			for _, rule := range r.Egress.HTTP {
				if rule.Host != "" {
					hosts = append(hosts, rule.Host)
				} else {
					hosts = append(hosts, rule.HostPattern)
				}
			}
			parts = append(parts, "requires egress "+strings.Join(hosts, " "))
		}
		if r.Filesystem != nil && r.Filesystem.PrivateTmp {
			parts = append(parts, "private /tmp")
		}
	}
	out := strings.Join(parts, ", ")
	if m.hasConfigSchema() {
		if out != "" {
			out += "; "
		}
		out += "config schema"
	}
	if m.MinRuntime != "" {
		if out != "" {
			out += "; "
		}
		out += "runtime " + canonical(m.MinRuntime) + " or newer"
	}
	return out
}

// Sandbox is the Composition sandbox block that satisfies Requires: the
// egress rules and filesystem copied. Nil when the module requires nothing.
func (m *Manifest) Sandbox() *v1beta1.Sandbox {
	r := m.Requires
	if r == nil {
		return nil
	}
	s := &v1beta1.Sandbox{}
	if r.Egress != nil && len(r.Egress.HTTP) > 0 {
		s.Egress = &v1beta1.SandboxEgress{HTTP: append([]v1beta1.SandboxHTTPRule(nil), r.Egress.HTTP...)}
	}
	if r.Filesystem != nil && r.Filesystem.PrivateTmp {
		s.Filesystem = &v1beta1.SandboxFilesystem{PrivateTmp: true}
	}
	if s.Egress == nil && s.Filesystem == nil {
		return nil
	}
	return s
}

// Check holds the manifest against what one run was granted - narrowing
// only: every requirement must be covered by g, the runtime must be at least
// minRuntime, and config must satisfy the schema. The first miss is the
// error, worded for a fatal result the caller prefixes with the module's
// name. runtimeVersion is the runtime's own (RuntimeVersion); empty or a
// development build passes every minRuntime.
func (m *Manifest) Check(g Grants, config *runtime.RawExtension, runtimeVersion string) error {
	if m.ABI != ABIVersion {
		return fmt.Errorf("requires ABI v%d, this runtime implements ABI v%d", m.ABI, ABIVersion)
	}
	if r := m.Requires; r != nil {
		if r.Egress != nil {
			for _, required := range r.Egress.HTTP {
				if !covered(required, g.HTTP) {
					return fmt.Errorf("requires sandbox.egress.http %s, which the Composition does not grant", describeRule(required))
				}
			}
		}
		if r.Filesystem != nil && r.Filesystem.PrivateTmp && !g.PrivateTmp {
			return errors.New("requires sandbox.filesystem.privateTmp, which the Composition does not grant")
		}
	}
	if m.MinRuntime != "" && runtimeVersion != "" && runtimeVersion != "(devel)" {
		if have := canonical(runtimeVersion); semver.IsValid(have) && semver.Compare(have, canonical(m.MinRuntime)) < 0 {
			return fmt.Errorf("requires runtime %s or newer, this is %s", canonical(m.MinRuntime), have)
		}
	}
	return m.ValidateConfig(config)
}

// ValidateConfig holds config against config.schema alone; nil without a
// schema, and an absent config validates as an empty object.
func (m *Manifest) ValidateConfig(config *runtime.RawExtension) error {
	if m.schema == nil {
		if !m.hasConfigSchema() {
			return nil
		}
		// Validate was not run: compile now, once.
		if err := m.ensureSchema(); err != nil {
			return err
		}
	}
	raw := []byte("{}")
	if config != nil && len(config.Raw) > 0 {
		raw = config.Raw
	}
	instance, err := jsonschema.UnmarshalJSON(bytes.NewReader(raw))
	if err != nil {
		return fmt.Errorf("config is not JSON: %w", err)
	}
	if err := m.schema.Validate(instance); err != nil {
		var ve *jsonschema.ValidationError
		if errors.As(err, &ve) {
			return fmt.Errorf("config does not match the module's schema: %s", firstFailure(ve))
		}
		return fmt.Errorf("config does not match the module's schema: %w", err)
	}
	return nil
}

// RuntimeVersion is this binary's version from its build information - the
// tag it was built at, "(devel)" or a pseudo-version for a development
// build; empty when there is no build information at all.
func RuntimeVersion() string {
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		return ""
	}
	return bi.Main.Version
}

// covered reports whether one required rule is admitted by a granted rule:
// the same host, or a granted pattern covering the required host or
// pattern; the required methods among the granted; the granted path prefix
// a prefix of the required one (an empty granted prefix admits every path,
// an empty required prefix needs an empty granted one).
func covered(required v1beta1.SandboxHTTPRule, granted []v1beta1.SandboxHTTPRule) bool {
	for _, g := range granted {
		if !hostCovered(required, g) {
			continue
		}
		if !methodsCovered(required.Methods, g.Methods) {
			continue
		}
		if g.PathPrefix != "" && !strings.HasPrefix(required.PathPrefix, g.PathPrefix) {
			continue
		}
		return true
	}
	return false
}

func hostCovered(required, granted v1beta1.SandboxHTTPRule) bool {
	switch {
	case required.Host != "":
		if granted.Host != "" {
			return strings.EqualFold(strings.TrimSuffix(granted.Host, "."), strings.TrimSuffix(required.Host, "."))
		}
		return egress.PatternCovers(granted.HostPattern, required.Host)
	default:
		// A granted exact host never covers a pattern.
		return granted.HostPattern != "" && egress.PatternUnder(required.HostPattern, granted.HostPattern)
	}
}

func methodsCovered(required, granted []string) bool {
	have := make(map[string]bool, len(granted))
	for _, m := range granted {
		have[strings.ToUpper(m)] = true
	}
	for _, m := range required {
		if !have[strings.ToUpper(m)] {
			return false
		}
	}
	return true
}

// describeRule renders a rule for a refusal: "host api.example.com methods
// [GET] pathPrefix /v1/".
func describeRule(r v1beta1.SandboxHTTPRule) string {
	var b strings.Builder
	if r.Host != "" {
		b.WriteString("host " + r.Host)
	} else {
		b.WriteString("hostPattern " + r.HostPattern)
	}
	b.WriteString(" methods [" + strings.Join(r.Methods, " ") + "]")
	if r.PathPrefix != "" {
		b.WriteString(" pathPrefix " + r.PathPrefix)
	}
	return b.String()
}

// canonical gives a version the leading v semver wants.
func canonical(v string) string {
	if v == "" || strings.HasPrefix(v, "v") {
		return v
	}
	return "v" + v
}

// compile turns an inline JSON Schema into a validator. The compiler's
// loader refuses every URL, so a $ref that leaves the document is an error
// here rather than a fetch at run time; formats are asserted (a manifest that
// says format: uri means it).
func compile(raw json.RawMessage) (*jsonschema.Schema, error) {
	doc, err := jsonschema.UnmarshalJSON(bytes.NewReader(raw))
	if err != nil {
		return nil, fmt.Errorf("config.schema is not JSON: %w", err)
	}
	c := jsonschema.NewCompiler()
	c.DefaultDraft(jsonschema.Draft2020)
	c.AssertFormat()
	c.UseLoader(refuseLoader{})
	if err := c.AddResource(schemaURL, doc); err != nil {
		return nil, fmt.Errorf("config.schema: %w", err)
	}
	schema, err := c.Compile(schemaURL)
	if err != nil {
		var load *jsonschema.LoadURLError
		if errors.As(err, &load) {
			return nil, fmt.Errorf("config.schema: $ref to %s is not allowed: schemas are inline", load.URL)
		}
		return nil, fmt.Errorf("config.schema: %w", err)
	}
	return schema, nil
}

// refuseLoader is the compiler's view of the world outside the inline
// document: nothing.
type refuseLoader struct{}

func (refuseLoader) Load(url string) (any, error) {
	return nil, fmt.Errorf("$ref to %s is not allowed: schemas are inline", url)
}

// englishPrinter renders the library's messages in English, whatever the
// process locale: a refusal is compared in tests and read in an XR condition.
var englishPrinter = message.NewPrinter(language.English)

// firstFailure renders the first leaf of a validation error as
// "<instance JSON pointer>: <message>", the root as "/".
func firstFailure(ve *jsonschema.ValidationError) string {
	leaf := ve
	for len(leaf.Causes) > 0 {
		leaf = leaf.Causes[0]
	}
	return jsonPointer(leaf.InstanceLocation) + ": " + leaf.ErrorKind.LocalizedString(englishPrinter)
}

func jsonPointer(segments []string) string {
	if len(segments) == 0 {
		return "/"
	}
	var b strings.Builder
	for _, s := range segments {
		b.WriteByte('/')
		b.WriteString(strings.ReplaceAll(strings.ReplaceAll(s, "~", "~0"), "/", "~1"))
	}
	return b.String()
}
