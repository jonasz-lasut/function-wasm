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

	"github.com/google/go-containerregistry/pkg/authn"
	"github.com/google/go-containerregistry/pkg/name"
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/remote"

	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
)

// InspectCmd prints what a module is made of — what the runtime sees when
// it loads it, read by the runtime's own engine — and what a registry says
// about an artifact.
type InspectCmd struct {
	Target  string `arg:"" help:"A module file, compiled with wasmtime for the runtime's own view (seconds for a large Go module); or an OCI reference (tag or digest) described from its manifest — media types, layer size, annotations — without pulling."`
	Pull    bool   `help:"For a reference: also pull the module layer and read the module, as for a file."`
	Output  string `help:"text or json." enum:"text,json" default:"text"`
	MaxSize int    `help:"Largest module to read in MB." default:"128"`
}

// inspection is everything inspect prints, in JSON field order.
type inspection struct {
	Target string `json:"target"`
	// Reference describes the artifact when Target is a reference.
	Reference *referenceInfo `json:"reference,omitempty"`
	// Module describes the module bytes — a file, or a pulled layer.
	Module *moduleInfo `json:"module,omitempty"`
}

type referenceInfo struct {
	Digest      string            `json:"digest"`
	MediaType   string            `json:"mediaType"`
	Size        int64             `json:"size"`
	Config      descriptorInfo    `json:"config"`
	Layers      []descriptorInfo  `json:"layers"`
	Annotations map[string]string `json:"annotations,omitempty"`
	// ModuleLayer is the layer the runtime would take as the module, by the
	// resolver's own rule.
	ModuleLayer *descriptorInfo `json:"moduleLayer,omitempty"`
	// ModuleLayerError says why no layer would do.
	ModuleLayerError string `json:"moduleLayerError,omitempty"`
}

type descriptorInfo struct {
	MediaType   string            `json:"mediaType"`
	Digest      string            `json:"digest"`
	Size        int64             `json:"size"`
	Annotations map[string]string `json:"annotations,omitempty"`
}

type moduleInfo struct {
	Size int `json:"size"`
	// ABI is "v1" when the module passes the runtime's check; otherwise
	// empty, with ABIError saying what the runtime says at load.
	ABI      string       `json:"abi,omitempty"`
	ABIError string       `json:"abiError,omitempty"`
	Exports  []externInfo `json:"exports"`
	Imports  []externInfo `json:"imports"`
	Memories []memoryInfo `json:"memories"`
}

type externInfo struct {
	Module string `json:"module,omitempty"`
	Name   string `json:"name"`
	Kind   string `json:"kind"`
	Type   string `json:"type,omitempty"`
}

type memoryInfo struct {
	MinPages uint64  `json:"minPages"`
	MaxPages *uint64 `json:"maxPages,omitempty"`
	Shared   bool    `json:"shared,omitempty"`
	Memory64 bool    `json:"memory64,omitempty"`
}

// Run inspects the target: a file when one exists at that path, otherwise a
// reference.
func (c *InspectCmd) Run(ctx context.Context, stdout io.Writer) error {
	out := inspection{Target: c.Target}
	if st, err := os.Stat(c.Target); err == nil && !st.IsDir() {
		wasm, err := os.ReadFile(c.Target)
		if err != nil {
			return err
		}
		out.Module, err = describeModule(wasm)
		if err != nil {
			return fmt.Errorf("%s: %w", c.Target, err)
		}
		return c.print(stdout, out)
	}
	ref, err := name.ParseReference(c.Target)
	if err != nil {
		return fmt.Errorf("%s is neither a file nor an OCI reference: %w", c.Target, err)
	}
	opts := []remote.Option{remote.WithAuthFromKeychain(authn.DefaultKeychain), remote.WithContext(ctx)}
	desc, err := remote.Get(ref, opts...)
	if err != nil {
		return fmt.Errorf("cannot fetch manifest %s: %w", ref, err)
	}
	if desc.MediaType.IsIndex() {
		return fmt.Errorf("%s is an image index; inspect the manifest holding the module", ref)
	}
	m, err := v1.ParseManifest(bytes.NewReader(desc.Manifest))
	if err != nil {
		return fmt.Errorf("cannot parse manifest %s: %w", ref, err)
	}
	info := &referenceInfo{
		Digest:      desc.Digest.String(),
		MediaType:   string(desc.MediaType),
		Size:        desc.Size,
		Config:      descriptor(m.Config),
		Annotations: m.Annotations,
	}
	for _, l := range m.Layers {
		info.Layers = append(info.Layers, descriptor(l))
	}
	layer, err := module.WasmLayer(m)
	if err != nil {
		info.ModuleLayerError = err.Error()
	} else {
		d := descriptor(layer)
		info.ModuleLayer = &d
	}
	out.Reference = info
	if c.Pull && info.ModuleLayer != nil {
		l, err := remote.Layer(ref.Context().Digest(layer.Digest.String()), opts...)
		if err != nil {
			return fmt.Errorf("cannot fetch module layer: %w", err)
		}
		rc, err := l.Compressed()
		if err != nil {
			return fmt.Errorf("cannot read module layer: %w", err)
		}
		defer func() { _ = rc.Close() }()
		limit := int64(c.MaxSize) << 20
		wasm, err := io.ReadAll(io.LimitReader(rc, limit+1))
		if err != nil {
			return fmt.Errorf("cannot read module layer: %w", err)
		}
		if int64(len(wasm)) > limit {
			return fmt.Errorf("module layer exceeds --max-size (%d MB)", c.MaxSize)
		}
		if module.IsTarLayer(layer.MediaType) {
			if wasm, err = module.ExtractWasm(wasm, limit); err != nil {
				return err
			}
		}
		if out.Module, err = describeModule(wasm); err != nil {
			return fmt.Errorf("%s: %w", ref, err)
		}
	}
	return c.print(stdout, out)
}

func descriptor(d v1.Descriptor) descriptorInfo {
	return descriptorInfo{MediaType: string(d.MediaType), Digest: d.Digest.String(), Size: d.Size, Annotations: d.Annotations}
}

// describeModule compiles a module with the runtime's engine and reports
// what it sees.
func describeModule(wasm []byte) (*moduleInfo, error) {
	shape, err := inspectModule(wasm)
	if err != nil {
		return nil, err
	}
	info := &moduleInfo{Size: len(wasm), ABI: "v1"}
	if shape.ABIError != nil {
		info.ABI, info.ABIError = "", shape.ABIError.Error()
	}
	for _, e := range shape.Exports {
		info.Exports = append(info.Exports, externInfo{Name: e.Name, Kind: e.Kind, Type: e.Type})
	}
	for _, i := range shape.Imports {
		info.Imports = append(info.Imports, externInfo{Module: i.Module, Name: i.Name, Kind: i.Kind, Type: i.Type})
	}
	for _, mem := range shape.Memories {
		info.Memories = append(info.Memories, memoryInfo{MinPages: mem.Min, MaxPages: mem.Max, Shared: mem.Shared, Memory64: mem.Memory64})
	}
	return info, nil
}

func (c *InspectCmd) print(w io.Writer, out inspection) error {
	if c.Output == "json" {
		enc := json.NewEncoder(w)
		enc.SetIndent("", "  ")
		enc.SetEscapeHTML(false)
		return enc.Encode(out)
	}
	if r := out.Reference; r != nil {
		_, _ = fmt.Fprintf(w, "%s: manifest %s (%s, %s)\n", out.Target, r.Digest, r.MediaType, humanBytes(r.Size))
		_, _ = fmt.Fprintf(w, "  config: %s %s (%s)\n", r.Config.MediaType, r.Config.Digest, humanBytes(r.Config.Size))
		for _, l := range r.Layers {
			_, _ = fmt.Fprintf(w, "  layer: %s %s (%s)%s\n", l.MediaType, l.Digest, humanBytes(l.Size), annotationsText(l.Annotations))
		}
		if len(r.Annotations) > 0 {
			_, _ = fmt.Fprintf(w, "  annotations:%s\n", annotationsText(r.Annotations))
		}
		switch {
		case r.ModuleLayer != nil:
			_, _ = fmt.Fprintf(w, "  module layer: %s %s (%s)\n", r.ModuleLayer.MediaType, r.ModuleLayer.Digest, humanBytes(r.ModuleLayer.Size))
		default:
			_, _ = fmt.Fprintf(w, "  module layer: none — the runtime would refuse it: %s\n", r.ModuleLayerError)
		}
	}
	if m := out.Module; m != nil {
		verdict := "ABI " + m.ABI
		if m.ABI == "" {
			verdict = "not ABI v1: " + m.ABIError
		}
		if out.Reference == nil {
			_, _ = fmt.Fprintf(w, "%s: %s, %s\n", out.Target, humanBytes(int64(m.Size)), verdict)
		} else {
			_, _ = fmt.Fprintf(w, "  module: %s, %s\n", humanBytes(int64(m.Size)), verdict)
		}
		_, _ = fmt.Fprintf(w, "  exports: %s\n", externsText(m.Exports))
		_, _ = fmt.Fprintf(w, "  imports: %s\n", importsText(m.Imports))
		for _, mem := range m.Memories {
			line := fmt.Sprintf("  memory: %d pages (%s) initial", mem.MinPages, pagesText(mem.MinPages))
			if mem.MaxPages != nil {
				line += fmt.Sprintf(", %d pages (%s) maximum", *mem.MaxPages, pagesText(*mem.MaxPages))
			} else {
				line += ", no maximum"
			}
			if mem.Shared {
				line += ", shared"
			}
			if mem.Memory64 {
				line += ", 64-bit"
			}
			_, _ = fmt.Fprintln(w, line)
		}
	}
	return nil
}

func externsText(xs []externInfo) string {
	if len(xs) == 0 {
		return "none"
	}
	parts := make([]string, 0, len(xs))
	for _, x := range xs {
		if x.Type != "" {
			parts = append(parts, x.Name+" "+x.Type)
		} else {
			parts = append(parts, x.Name+" ("+x.Kind+")")
		}
	}
	return strings.Join(parts, ", ")
}

// importsText lists host imports one by one and WASI as a count: forty
// wasi_snapshot_preview1 names say nothing a reader needs.
func importsText(xs []externInfo) string {
	if len(xs) == 0 {
		return "none"
	}
	wasi := 0
	var parts []string
	for _, x := range xs {
		if x.Module == engine.WASIModule {
			wasi++
			continue
		}
		if x.Type != "" {
			parts = append(parts, x.Module+"."+x.Name+" "+x.Type)
		} else {
			parts = append(parts, x.Module+"."+x.Name+" ("+x.Kind+")")
		}
	}
	if wasi > 0 {
		parts = append([]string{fmt.Sprintf("%s (%d)", engine.WASIModule, wasi)}, parts...)
	}
	return strings.Join(parts, ", ")
}

// pagesText renders a page count as bytes; a count past what fits in bytes
// (a memory64 declaration) is shown as pages only.
func pagesText(pages uint64) string {
	if pages > 1<<40 {
		return fmt.Sprintf("%d pages", pages)
	}
	return humanBytes(int64(pages) << 16) //nolint:gosec // Bounded just above.
}

func annotationsText(a map[string]string) string {
	if len(a) == 0 {
		return ""
	}
	keys := make([]string, 0, len(a))
	for k := range a {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var b strings.Builder
	for _, k := range keys {
		fmt.Fprintf(&b, " %s=%s", k, a[k])
	}
	return b.String()
}

// humanBytes renders a size for the text output.
func humanBytes(n int64) string {
	switch {
	case n >= 1<<20:
		return fmt.Sprintf("%.1f MB", float64(n)/(1<<20))
	case n >= 1<<10:
		return fmt.Sprintf("%.1f KB", float64(n)/(1<<10))
	}
	return fmt.Sprintf("%d B", n)
}
