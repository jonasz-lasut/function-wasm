package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"sort"
	"strings"

	"github.com/crossplane/function-sdk-go"
	"github.com/crossplane/function-sdk-go/errors"
	"github.com/crossplane/function-sdk-go/logging"
	"k8s.io/apimachinery/pkg/api/resource"
	utilyaml "k8s.io/apimachinery/pkg/util/yaml"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/admission"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
	"github.com/jonasz-lasut/function-wasm/internal/module"
)

// The Input's identity in a Composition step, and the Composition's own.
const (
	inputAPIVersion       = "wasm.fn.crossplane.io/v1beta1"
	inputKind             = "Input"
	compositionAPIVersion = "apiextensions.crossplane.io/v1"
	compositionKind       = "Composition"
)

// ValidateCmd runs the runtime's admission over Compositions, offline: for
// every pipeline step whose input is a function-wasm Input it runs exactly
// what RunFunction runs before it resolves anything — sandbox shape, grants
// within the ceiling flags, egress within the policy, limits, module and
// policy shape — and, with --xr, materialises module.from against a
// composite resource; with --resolve it goes on to resolve, verify and fetch
// each module and read its ABI. It prints one line per step in the runtime's
// own words and exits 0 when every step is admitted, 1 when at least one is
// refused, 2 when the tool itself failed.
type ValidateCmd struct {
	CeilingFlags `embed:""`

	Files        []string `arg:"" name:"file" help:"Composition or Input files, YAML or JSON, multi-document; - reads stdin. Every pipeline step whose input is a wasm.fn.crossplane.io/v1beta1 Input is checked, and every bare Input document."`
	FunctionName string   `help:"Only check pipeline steps whose functionRef.name is this; by default every step carrying a function-wasm Input, whatever the function's name."`
	XR           string   `help:"A composite resource (YAML or JSON) to materialise module.from sources against, as the observed XR would; without it a from source is checked for the policy fence the runtime requires and reported as chosen by the composite resource." type:"existingfile"`
	Resolve      bool     `help:"Also resolve, verify (with --cosign-key) and fetch every module — OCI pulls use the local Docker config, never a step credential — and compile it with wasmtime for the runtime's own verdict: size, ABI, host imports. A compile is seconds and about a gigabyte for a large Go module."`
	Output       string   `help:"text: one line per step, warnings indented below it; json: one JSON object per step, one per line." enum:"text,json" default:"text"`

	// stderr receives tool failures and notes; os.Stderr when nil (tests set
	// it).
	stderr io.Writer
}

// exitError carries validate's exit code out of kong: 1 when a step is
// refused, 2 when the tool itself failed (the message is printed already).
type exitError struct {
	code int
}

func (e exitError) Error() string { return fmt.Sprintf("exit status %d", e.code) }

// stepResult is the verdict on one step — what one text line and one JSON
// object carry.
type stepResult struct {
	File string `json:"file"`
	// Composition is the Composition's name; empty for a bare Input document.
	Composition string `json:"composition,omitempty"`
	// Document is the index of the document in the file (from 0).
	Document int `json:"document"`
	// Index is the step's index in the pipeline; -1 for a bare Input.
	Index int `json:"index"`
	// Step is the step's name; for a bare Input its metadata.name, if any.
	Step string `json:"step,omitempty"`
	// Function is functionRef.name; empty for a bare Input.
	Function string `json:"function,omitempty"`
	// Status is ok or refused.
	Status string `json:"status"`
	// Message is the refusal, in the runtime's words.
	Message string `json:"message,omitempty"`
	// Module describes the source as admitted (oci …, http …, path …, or
	// chosen by the composite resource from …).
	Module string `json:"module,omitempty"`
	// Details are the grants and limits admitted (limits, egress, private
	// /tmp, env), for the text line.
	Details []string `json:"details,omitempty"`
	// Warnings are accepted-but-unwise findings; they never change the
	// status or the exit code.
	Warnings []string `json:"warnings,omitempty"`
	// Resolved is what --resolve found.
	Resolved *resolvedModule `json:"resolved,omitempty"`
}

// resolvedModule is what --resolve reads from a module.
type resolvedModule struct {
	Digest  string   `json:"digest"`
	Size    int      `json:"size"`
	ABI     string   `json:"abi"`
	Imports []string `json:"imports,omitempty"`
	// Manifest is the module's manifest, when it carries one.
	Manifest *manifest.Manifest `json:"manifest,omitempty"`
}

// Run validates the files and prints the verdicts to stdout; tool failures go
// to stderr with exit code 2.
func (c *ValidateCmd) Run(cli *CLI, stdout io.Writer) error {
	if c.stderr == nil {
		c.stderr = os.Stderr
	}
	log := logging.NewNopLogger()
	if cli.Debug {
		l, err := function.NewLogger(true)
		if err != nil {
			return err
		}
		log = l
	}
	ceilings, err := c.ceilings(log)
	if err != nil {
		return c.fail(err)
	}
	var resolver *module.Resolver
	var eng *engine.Engine
	if c.Resolve {
		if resolver, err = c.resolver(nil); err != nil {
			return c.fail(err)
		}
		// wasmtime reads a module only by compiling it: --resolve pays the
		// compile the runtime would, for the verdict the runtime would reach.
		if eng, err = engine.New(ceilings.Engine); err != nil {
			return c.fail(err)
		}
		defer eng.Close()
	}
	var xr map[string]any
	if c.XR != "" {
		docs, err := readDocuments(c.XR)
		if err != nil {
			return c.fail(err)
		}
		if len(docs) != 1 {
			return c.fail(errors.Errorf("--xr %s must hold exactly one document, found %d", c.XR, len(docs)))
		}
		xr = docs[0]
	}

	v := &validator{ceilings: ceilings, resolver: resolver, engine: eng, xr: xr, cosignKey: c.CosignKey != "", functionName: c.FunctionName}
	refused := false
	for _, file := range c.Files {
		docs, err := readDocuments(file)
		if err != nil {
			return c.fail(err)
		}
		steps := findSteps(file, docs, c.FunctionName)
		if len(steps) == 0 {
			_, _ = fmt.Fprintf(c.stderr, "%s: no function-wasm Input found\n", file)
		}
		for _, s := range steps {
			result := v.validate(context.Background(), s)
			if result.Status != "ok" {
				refused = true
			}
			if err := c.print(stdout, result); err != nil {
				return err
			}
		}
	}
	if refused {
		return exitError{code: 1}
	}
	return nil
}

func (c *ValidateCmd) fail(err error) error {
	_, _ = fmt.Fprintf(c.stderr, "function validate: %v\n", err)
	return exitError{code: 2}
}

func (c *ValidateCmd) print(w io.Writer, r stepResult) error {
	if c.Output == "json" {
		enc := json.NewEncoder(w)
		enc.SetEscapeHTML(false)
		return enc.Encode(r)
	}
	where := r.File + ": "
	if r.Index >= 0 {
		where += fmt.Sprintf("Composition/%s pipeline[%d] %s", r.Composition, r.Index, r.Step)
	} else {
		where += fmt.Sprintf("Input[%d]", r.Document)
		if r.Step != "" {
			where += " " + r.Step
		}
	}
	if r.Status != "ok" {
		_, _ = fmt.Fprintf(w, "%s: refused: %s\n", where, r.Message)
	} else {
		parts := append([]string{r.Module}, r.Details...)
		_, _ = fmt.Fprintf(w, "%s: OK (%s)\n", where, strings.Join(parts, ", "))
	}
	if r.Resolved != nil {
		line := fmt.Sprintf("  module: %s, %s, ABI %s", r.Resolved.Digest, humanBytes(r.Resolved.Size), r.Resolved.ABI)
		if len(r.Resolved.Imports) > 0 {
			line += ", imports " + strings.Join(r.Resolved.Imports, " ")
		}
		if r.Resolved.Manifest != nil {
			line += "; manifest: " + r.Resolved.Manifest.Summary()
		}
		_, _ = fmt.Fprintln(w, line)
	}
	for _, warning := range r.Warnings {
		_, _ = fmt.Fprintf(w, "  warning: %s\n", warning)
	}
	return nil
}

// step is one Input found in a file, with where it came from.
type step struct {
	file        string
	composition string
	document    int
	index       int
	name        string
	function    string
	input       map[string]any
}

// readDocuments reads every YAML or JSON document of a file (- is stdin) as
// an unstructured object; empty documents are skipped.
func readDocuments(file string) ([]map[string]any, error) {
	var raw []byte
	var err error
	if file == "-" {
		raw, err = io.ReadAll(os.Stdin)
	} else {
		raw, err = os.ReadFile(file) //nolint:gosec // The user's own argument.
	}
	if err != nil {
		return nil, errors.Wrapf(err, "cannot read %s", file)
	}
	dec := utilyaml.NewYAMLOrJSONDecoder(bytes.NewReader(raw), 4096)
	var docs []map[string]any
	for {
		var doc map[string]any
		if err := dec.Decode(&doc); err != nil {
			if errors.Is(err, io.EOF) {
				return docs, nil
			}
			return nil, errors.Wrapf(err, "cannot parse %s", file)
		}
		if doc == nil {
			continue
		}
		docs = append(docs, doc)
	}
}

// findSteps returns the function-wasm Inputs of the documents: every
// pipeline step of a Composition whose input is one (and, with functionName,
// whose functionRef.name matches), and every bare Input document.
func findSteps(file string, docs []map[string]any, functionName string) []step {
	var steps []step
	for i, doc := range docs {
		apiVersion, _ := doc["apiVersion"].(string)
		kind, _ := doc["kind"].(string)
		switch {
		case apiVersion == inputAPIVersion && kind == inputKind:
			if functionName == "" {
				metadata, _ := doc["metadata"].(map[string]any)
				name, _ := metadata["name"].(string)
				steps = append(steps, step{file: file, document: i, index: -1, name: name, input: doc})
			}
		case apiVersion == compositionAPIVersion && kind == compositionKind:
			metadata, _ := doc["metadata"].(map[string]any)
			name, _ := metadata["name"].(string)
			spec, _ := doc["spec"].(map[string]any)
			pipeline, _ := spec["pipeline"].([]any)
			for j, item := range pipeline {
				entry, _ := item.(map[string]any)
				input, _ := entry["input"].(map[string]any)
				if v, _ := input["apiVersion"].(string); v != inputAPIVersion {
					continue
				}
				if k, _ := input["kind"].(string); k != inputKind {
					continue
				}
				fnRef, _ := entry["functionRef"].(map[string]any)
				fn, _ := fnRef["name"].(string)
				if functionName != "" && fn != functionName {
					continue
				}
				stepName, _ := entry["step"].(string)
				steps = append(steps, step{file: file, composition: name, document: i, index: j, name: stepName, function: fn, input: input})
			}
		}
	}
	return steps
}

// validator judges steps against one set of ceilings.
type validator struct {
	ceilings     admission.Ceilings
	resolver     *module.Resolver
	engine       *engine.Engine
	xr           map[string]any
	cosignKey    bool
	functionName string
}

func (v *validator) validate(ctx context.Context, s step) stepResult {
	r := stepResult{File: s.file, Composition: s.composition, Document: s.document, Index: s.index, Step: s.name, Function: s.function, Status: "ok"}
	refuse := func(err error) stepResult {
		r.Status, r.Message = "refused", err.Error()
		return r
	}

	in, warnings, err := decodeInput(s.input)
	if err != nil {
		return refuse(err)
	}
	r.Warnings = warnings

	// The runtime's admission, verbatim.
	admitted, err := admission.Admit(in, v.ceilings)
	if err != nil {
		return refuse(err)
	}
	r.Details = describeAdmitted(in, admitted)

	// The module: materialised against the XR when one is given, as the
	// runtime does on every request; otherwise checked for the fence a
	// composite-chosen source requires and reported as such.
	src := in.Module
	switch {
	case src.From != "" && v.xr != nil:
		src, err = module.FromComposite(in.Module, in.Policy, v.xr)
		if err != nil {
			return refuse(errors.Wrap(err, "cannot resolve module"))
		}
		r.Module = describeSource(src) + " (from " + in.Module.From + ")"
	case src.From != "":
		if err := module.ValidateFrom(src, in.Policy); err != nil {
			return refuse(errors.Wrap(err, "cannot resolve module"))
		}
		r.Module = describeSource(src)
		if in.Policy != nil && len(in.Policy.RepositoryAllowList) > 0 {
			r.Module += " (policy admits " + strings.Join(in.Policy.RepositoryAllowList, ", ") + ")"
		}
	default:
		r.Module = describeSource(src)
	}
	r.Warnings = append(r.Warnings, v.warnings(in)...)

	if v.resolver == nil || src.From != "" {
		return r
	}
	if src.OCI != nil && src.OCI.Credentials != "" {
		r.Warnings = append(r.Warnings, fmt.Sprintf("module.oci.credentials %q is a step Secret this tool cannot read; the module is pulled with the local Docker config instead", src.OCI.Credentials))
	}
	ref, err := v.resolver.Resolve(ctx, src, nil)
	if err != nil {
		return refuse(errors.Wrap(err, "cannot resolve module"))
	}
	if err := ref.Verify(ctx); err != nil {
		return refuse(errors.Wrapf(err, "cannot verify module %s", ref.Description))
	}
	wasm, err := ref.Fetch(ctx)
	if err != nil {
		return refuse(errors.Wrapf(err, "cannot load module %s: cannot fetch module", ref.Description))
	}
	shape, err := v.engine.Inspect(wasm)
	if err != nil {
		return refuse(errors.Wrapf(err, "cannot load module %s", ref.Description))
	}
	if shape.ABIError != nil {
		return refuse(errors.Wrapf(shape.ABIError, "cannot load module %s", ref.Description))
	}
	r.Resolved = &resolvedModule{Digest: ref.Digest, Size: len(wasm), ABI: "v1", Imports: shape.HostImports()}
	// The module's manifest, held against the grants the step was admitted
	// with — the check the runtime makes between load and run.
	raw, found, err := ref.Manifest(ctx)
	if err != nil {
		return refuse(errors.Wrapf(err, "cannot read the manifest of module %s", ref.Description))
	}
	if found {
		m, err := manifest.Parse(raw)
		if err != nil {
			return refuse(errors.Wrapf(err, "module %s has an invalid manifest", ref.Description))
		}
		r.Resolved.Manifest = m
		grants := manifest.Grants{PrivateTmp: admitted.Grant.PrivateTmp}
		if admitted.HTTP != nil && in.Sandbox != nil && in.Sandbox.Egress != nil {
			grants.HTTP = in.Sandbox.Egress.HTTP
		}
		if err := m.Check(grants, in.Config, manifest.RuntimeVersion()); err != nil {
			return refuse(errors.Errorf("module %s %v", ref.Description, err))
		}
	}
	return r
}

// decodeInput turns the unstructured Input into the typed one the runtime
// decodes — strictly first, so a field the runtime would silently ignore
// becomes a warning naming it, then as the runtime does.
func decodeInput(raw map[string]any) (*v1beta1.Input, []string, error) {
	b, err := json.Marshal(raw)
	if err != nil {
		return nil, nil, err
	}
	in := &v1beta1.Input{}
	dec := json.NewDecoder(bytes.NewReader(b))
	dec.DisallowUnknownFields()
	if err := dec.Decode(in); err == nil {
		return in, nil, nil
	} else if !strings.Contains(err.Error(), "unknown field") {
		return nil, nil, errors.Wrap(err, "cannot decode the Input")
	}
	// Unknown fields only: decode leniently, as request.GetInput does, and
	// warn about each field the runtime will ignore.
	var warnings []string
	for {
		dec := json.NewDecoder(bytes.NewReader(b))
		dec.DisallowUnknownFields()
		err := dec.Decode(&v1beta1.Input{})
		if err == nil {
			break
		}
		field, ok := unknownField(err)
		if !ok {
			return nil, nil, errors.Wrap(err, "cannot decode the Input")
		}
		warnings = append(warnings, fmt.Sprintf("unknown field %q is ignored by the runtime", field))
		if b, err = withoutField(b, field); err != nil {
			return nil, nil, err
		}
	}
	*in = v1beta1.Input{}
	if err := json.Unmarshal(b, in); err != nil {
		return nil, nil, errors.Wrap(err, "cannot decode the Input")
	}
	return in, warnings, nil
}

// unknownField extracts the field name from encoding/json's unknown-field
// error.
func unknownField(err error) (string, bool) {
	msg := err.Error()
	i := strings.Index(msg, `unknown field "`)
	if i < 0 {
		return "", false
	}
	rest := msg[i+len(`unknown field "`):]
	end := strings.IndexByte(rest, '"')
	if end < 0 {
		return "", false
	}
	return rest[:end], true
}

// withoutField removes a top-level or nested field named field from a JSON
// object — the first occurrence found walking the object — so decoding can
// go on to the next unknown one.
func withoutField(b []byte, field string) ([]byte, error) {
	var obj map[string]any
	if err := json.Unmarshal(b, &obj); err != nil {
		return nil, err
	}
	if !removeField(obj, field) {
		return nil, errors.Errorf("cannot decode the Input: unknown field %q", field)
	}
	return json.Marshal(obj)
}

func removeField(obj map[string]any, field string) bool {
	if _, ok := obj[field]; ok {
		delete(obj, field)
		return true
	}
	keys := make([]string, 0, len(obj))
	for k := range obj {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		if child, ok := obj[k].(map[string]any); ok && removeField(child, field) {
			return true
		}
	}
	return false
}

// describeSource names a source the way the runtime's messages do.
func describeSource(src v1beta1.ModuleSource) string {
	switch {
	case src.From != "":
		return "chosen by the composite resource from " + src.From
	case src.OCI != nil:
		return "oci " + src.OCI.Ref
	case src.HTTP != nil:
		return "http " + src.HTTP.URL
	default:
		return "path " + src.Path
	}
}

// describeAdmitted lists what the step was granted: limits, egress hosts,
// the private /tmp, environment keys.
func describeAdmitted(in *v1beta1.Input, a admission.Admitted) []string {
	var out []string
	if in.Limits != nil {
		var limits []string
		if in.Limits.Timeout != nil {
			limits = append(limits, "timeout "+in.Limits.Timeout.Duration.String())
		}
		if in.Limits.Memory != nil {
			limits = append(limits, "memory "+in.Limits.Memory.String())
		}
		if len(limits) > 0 {
			out = append(out, "limits "+strings.Join(limits, " "))
		}
	}
	if a.HTTP != nil {
		var hosts []string
		for _, r := range in.Sandbox.Egress.HTTP {
			if r.Host != "" {
				hosts = append(hosts, r.Host)
			} else {
				hosts = append(hosts, r.HostPattern)
			}
		}
		out = append(out, "egress "+strings.Join(hosts, " "))
	}
	if a.Grant.PrivateTmp {
		out = append(out, "private /tmp")
	}
	if len(a.Grant.Env) > 0 {
		keys := make([]string, 0, len(a.Grant.Env))
		for k := range a.Grant.Env {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		out = append(out, "env "+strings.Join(keys, " "))
	}
	return out
}

// warnings are the short fixed list of accepted-but-unwise findings.
func (v *validator) warnings(in *v1beta1.Input) []string {
	var out []string
	if in.Module.Type == v1beta1.ModuleTypePath {
		out = append(out, "module.type Path names a file under the runtime's --module-dir and carries no digest; a cluster Composition should pin an OCI or HTTP source by digest")
	}
	if in.Sandbox != nil && in.Sandbox.Egress != nil && len(in.Sandbox.Egress.HTTP) > 0 && !v.cosignKey {
		out = append(out, "sandbox.egress is granted to a module that is not signature-verified: no --cosign-key was given")
	}
	if in.Limits != nil {
		if in.Limits.Timeout != nil && in.Limits.Timeout.Duration == v.ceilings.Engine.Timeout {
			out = append(out, fmt.Sprintf("limits.timeout %s equals --module-timeout: it narrows nothing", in.Limits.Timeout.Duration))
		}
		if in.Limits.Memory != nil && in.Limits.Memory.Value() == v.ceilings.Engine.MemoryLimit {
			out = append(out, fmt.Sprintf("limits.memory %s equals --module-memory-limit (%s): it narrows nothing", in.Limits.Memory, resource.NewQuantity(v.ceilings.Engine.MemoryLimit, resource.BinarySI)))
		}
	}
	return out
}

// humanBytes renders a size for the text output.
func humanBytes(n int) string {
	switch {
	case n >= 1<<20:
		return fmt.Sprintf("%.1f MB", float64(n)/(1<<20))
	case n >= 1<<10:
		return fmt.Sprintf("%.1f KB", float64(n)/(1<<10))
	}
	return fmt.Sprintf("%d B", n)
}
