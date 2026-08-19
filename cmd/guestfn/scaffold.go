package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"path/filepath"
	"sort"
	"strings"

	"github.com/google/go-containerregistry/pkg/name"
	"sigs.k8s.io/yaml"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/sandbox"
)

// ScaffoldCmd prints Composition fragments for a module.
type ScaffoldCmd struct {
	Composition ScaffoldCompositionCmd `cmd:"" help:"Print a Composition step for a module — its module source, the sandbox its manifest requires, a config skeleton from the manifest's schema — or a whole Composition with --full."`
}

// ScaffoldCompositionCmd prints a Composition step from a module and its
// manifest.
type ScaffoldCompositionCmd struct {
	From         string `help:"The module: a file (a Path source, served from its directory) or an OCI reference (pinned to its manifest digest in the output, its manifest read from the artifact's manifest layer)." default:"fn.wasm"`
	Manifest     string `help:"The manifest to scaffold from; by default the wasmfn.yaml next to a module file (when there is one), or the artifact's manifest layer for a reference." placeholder:"wasmfn.yaml"`
	Name         string `help:"The step's name (and the Composition's, with --full); defaults to the manifest's name, else the module file's base name."`
	FunctionName string `help:"The functionRef.name of the step." default:"function-wasm"`
	Full         bool   `help:"Print a whole Composition, like a scaffold's example/composition.yaml, instead of one pipeline step."`
}

// Run prints the step (or Composition) as YAML.
func (c *ScaffoldCompositionCmd) Run(ctx context.Context, stdout io.Writer) error {
	src, m, err := c.source(ctx)
	if err != nil {
		return err
	}
	name := c.Name
	if name == "" {
		switch {
		case m != nil && m.Name != "":
			name = m.Name
		case src.Path != "":
			name = strings.TrimSuffix(filepath.Base(src.Path), filepath.Ext(src.Path))
		default:
			name = "module"
		}
	}
	step, err := compositionStep(name, c.FunctionName, src, m)
	if err != nil {
		return err
	}
	if !c.Full {
		_, _ = fmt.Fprint(stdout, step)
		return nil
	}
	_, _ = fmt.Fprintf(stdout, `apiVersion: apiextensions.crossplane.io/v1
kind: Composition
metadata:
  name: %s
spec:
  compositeTypeRef:
    apiVersion: example.crossplane.io/v1
    kind: XR
  mode: Pipeline
  pipeline:
%s`, name, indent(step, "  "))
	return nil
}

// source resolves --from to a module source and its manifest: --manifest,
// else the wasmfn.yaml beside a module file, else an artifact's manifest
// layer; nil when there is none.
func (c *ScaffoldCompositionCmd) source(ctx context.Context) (v1beta1.ModuleSource, *manifest.Manifest, error) {
	var m *manifest.Manifest
	if c.Manifest != "" {
		loaded, err := manifest.Load(c.Manifest)
		if err != nil {
			return v1beta1.ModuleSource{}, nil, err
		}
		m = loaded
	}
	if fileExists(c.From) {
		if m == nil {
			if candidate := filepath.Join(filepath.Dir(c.From), manifest.FileName); fileExists(candidate) {
				loaded, err := manifest.Load(candidate)
				if err != nil {
					return v1beta1.ModuleSource{}, nil, err
				}
				m = loaded
			}
		}
		return v1beta1.ModuleSource{Type: v1beta1.ModuleTypePath, Path: filepath.Base(c.From)}, m, nil
	}
	ref, err := name.ParseReference(c.From)
	if err != nil {
		return v1beta1.ModuleSource{}, nil, fmt.Errorf("%s is neither a file nor an OCI reference: %w", c.From, err)
	}
	opts := remoteOpts(ctx)
	desc, om, err := module.ParseRemoteManifest(ref, "name", opts...)
	if err != nil {
		return v1beta1.ModuleSource{}, nil, err
	}
	pinned := pinnedRef(ref, desc.Digest)
	if m == nil {
		if ml, ok := module.ManifestLayer(om); ok {
			if m, err = fetchManifest(ref, ml, opts); err != nil {
				return v1beta1.ModuleSource{}, nil, fmt.Errorf("%s: %w", ref, err)
			}
		}
	}
	return v1beta1.ModuleSource{Type: v1beta1.ModuleTypeOCI, OCI: &v1beta1.OCISource{Ref: pinned}}, m, nil
}

// compositionStep renders one pipeline step: the module, commented limits and
// a config skeleton from the schema. The module's sandbox needs are its
// manifest's requires - granted by the operator's policy (and narrowed by a
// compositionPolicy), never copied into the Input.
func compositionStep(name, functionName string, src v1beta1.ModuleSource, m *manifest.Manifest) (string, error) {
	var b strings.Builder
	fmt.Fprintf(&b, "- step: %s\n  functionRef:\n    name: %s\n  input:\n    apiVersion: %s\n    kind: %s\n", name, functionName, inputAPIVersion, inputKind)
	moduleYAML, err := yaml.Marshal(struct {
		Module v1beta1.ModuleSource `json:"module"`
	}{src})
	if err != nil {
		return "", err
	}
	b.WriteString(indent(string(moduleYAML), "    "))
	b.WriteString("    # limits: {timeout: 5s, memory: 128Mi}\n")
	if m != nil {
		config, err := configSkeleton(m)
		if err != nil {
			return "", err
		}
		if config != nil {
			configYAML, err := yaml.Marshal(struct {
				Config map[string]any `json:"config"`
			}{config})
			if err != nil {
				return "", err
			}
			b.WriteString(indent(string(configYAML), "    "))
		}
	}
	if skeleton := compositionPolicySkeleton(src, m); skeleton != "" {
		b.WriteString(indent(skeleton, "    "))
	}
	return b.String(), nil
}

// compositionPolicySkeleton renders a commented compositionPolicy the author
// can start from: the Cedar permits this module's manifest requirements would
// need (grantEgress per egress host, usePrivateTmp, setEnv and spendCredential
// per env binding) and, for an OCI source, a pullModule permit for its
// repository - the one a module.from source needs. The whole block is
// commented: the manifest is the request and the two Cedar layers decide, so a
// skeleton never copies in a grant. Empty when nothing is derivable (a static
// source with no requirements). The comments inside the block use Cedar's `//`
// so the block is valid once uncommented.
func compositionPolicySkeleton(src v1beta1.ModuleSource, m *manifest.Manifest) string {
	var body []string
	if m != nil && m.Requires != nil {
		r := m.Requires
		if r.Egress != nil {
			for _, h := range egressHosts(r.Egress.HTTP) {
				body = append(body,
					"// egress "+h+" (grantEgress is also the host allowlist):",
					`permit (principal, action == Action::"grantEgress", resource in HostPattern::"`+h+`");`)
			}
		}
		if r.Filesystem != nil && r.Filesystem.PrivateTmp {
			body = append(body,
				"// the private /tmp the module requires:",
				`permit (principal, action == Action::"usePrivateTmp", resource);`)
		}
		if len(r.Env) > 0 {
			body = append(body,
				"// the env the module binds from step credentials:",
				`permit (principal, action == Action::"setEnv", resource);`)
			for _, name := range credentialNames(r.Env) {
				body = append(body, `permit (principal, action == Action::"spendCredential", resource == Credential::"`+name+`");`)
			}
		}
	}
	if repo := ociRepository(src); repo != "" {
		body = append(body,
			"// for a module.from source, permit pulling this repository (a static",
			"// source needs no pullModule):",
			`permit (principal, action == Action::"pullModule", resource in Repository::"`+repo+`");`)
	}
	if len(body) == 0 {
		return ""
	}
	var b strings.Builder
	b.WriteString("# compositionPolicy is the composition author's Cedar layer - optional and\n")
	b.WriteString("# narrowing-only. A static source needs none (sandbox actions are scoped\n")
	b.WriteString("# default-permit; writing a permit for one opts into narrowing it). These\n")
	b.WriteString("# permits, from this module's manifest, are a starting point, never a grant:\n")
	b.WriteString("# compositionPolicy: |\n")
	for _, line := range body {
		b.WriteString("#   " + line + "\n")
	}
	return b.String()
}

// egressHosts lists the distinct hosts (or host patterns) of a module's egress
// rules, first-seen order, so one grantEgress permit is emitted per host.
func egressHosts(rules []egress.HTTPRule) []string {
	var out []string
	seen := map[string]bool{}
	for _, r := range rules {
		h := r.Host
		if h == "" {
			h = r.HostPattern
		}
		if h == "" || seen[h] {
			continue
		}
		seen[h] = true
		out = append(out, h)
	}
	return out
}

// credentialNames lists the distinct credential names a module's env bindings
// spend, first-seen order.
func credentialNames(bindings []sandbox.EnvBinding) []string {
	var out []string
	seen := map[string]bool{}
	for _, b := range bindings {
		name := b.FromCredential.Name
		if name == "" || seen[name] {
			continue
		}
		seen[name] = true
		out = append(out, name)
	}
	return out
}

// ociRepository is the "registry/repository" location of an OCI source (what a
// pullModule permit matches), empty for a path source. The ref is pinned, so
// name.NewDigest parses it the way internal/module does.
func ociRepository(src v1beta1.ModuleSource) string {
	if src.OCI == nil {
		return ""
	}
	d, err := name.NewDigest(src.OCI.Ref)
	if err != nil {
		return ""
	}
	return d.Context().RegistryStr() + "/" + d.Context().RepositoryStr()
}

// configSkeleton derives a config block from the manifest's schema: every
// top-level property, its default where the schema has one, otherwise a
// placeholder of its type. Nil without a schema or without properties.
func configSkeleton(m *manifest.Manifest) (map[string]any, error) {
	if m.Config == nil || len(m.Config.Schema) == 0 {
		return nil, nil
	}
	var schema struct {
		Properties map[string]struct {
			Type    any `json:"type"`
			Default any `json:"default"`
		} `json:"properties"`
	}
	if err := json.Unmarshal(m.Config.Schema, &schema); err != nil {
		return nil, fmt.Errorf("cannot read the manifest's config schema: %w", err)
	}
	if len(schema.Properties) == 0 {
		return nil, nil
	}
	out := make(map[string]any, len(schema.Properties))
	keys := make([]string, 0, len(schema.Properties))
	for k := range schema.Properties {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		p := schema.Properties[k]
		if p.Default != nil {
			out[k] = p.Default
			continue
		}
		out[k] = placeholder(p.Type)
	}
	return out, nil
}

// placeholder is the empty value of a JSON Schema type ("string", or a list
// whose first entry decides).
func placeholder(t any) any {
	if list, ok := t.([]any); ok && len(list) > 0 {
		t = list[0]
	}
	switch t {
	case "number", "integer":
		return 0
	case "boolean":
		return false
	case "object":
		return map[string]any{}
	case "array":
		return []any{}
	default:
		return ""
	}
}

// indent prefixes every non-empty line.
func indent(s, prefix string) string {
	lines := strings.Split(strings.TrimRight(s, "\n"), "\n")
	for i, l := range lines {
		if l != "" {
			lines[i] = prefix + l
		}
	}
	return strings.Join(lines, "\n") + "\n"
}
