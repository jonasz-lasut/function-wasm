package main

import (
	"context"
	"errors"
	"fmt"
	"io"

	"github.com/google/go-containerregistry/pkg/name"
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/remote"
	"sigs.k8s.io/yaml"

	"github.com/jonasz-lasut/function-wasm/internal/manifest"
	"github.com/jonasz-lasut/function-wasm/internal/module"
)

// ManifestCmd works on the module manifest: the wasmfn.yaml guestfn push
// publishes beside the module, and the copy an artifact carries.
type ManifestCmd struct {
	Validate ManifestValidateCmd `cmd:"" help:"Check a manifest file (wasmfn.yaml) the way guestfn build and push do, and print what it declares."`
	Show     ManifestShowCmd     `cmd:"" help:"Print the manifest an artifact carries as its manifest layer, without pulling the module."`
}

// ManifestValidateCmd checks a manifest file.
type ManifestValidateCmd struct {
	File string `arg:"" optional:"" help:"The manifest file (default wasmfn.yaml)."`
}

// Run loads (and so validates) the file and prints its summary.
func (c *ManifestValidateCmd) Run(stdout io.Writer) error {
	// kong applies no default to an omitted optional positional argument.
	if c.File == "" {
		c.File = manifest.FileName
	}
	m, err := manifest.Load(c.File)
	if err != nil {
		return err
	}
	_, _ = fmt.Fprintf(stdout, "%s: valid (%s)\n", c.File, manifestText(m))
	return nil
}

// ManifestShowCmd prints an artifact's manifest.
type ManifestShowCmd struct {
	Ref    string `arg:"" help:"An OCI reference (tag or digest) of an artifact guestfn push published."`
	Output string `help:"yaml or json." enum:"yaml,json" default:"yaml"`
}

// Run prints the manifest as YAML (or JSON); an artifact without one is an
// error, since there is nothing to show.
func (c *ManifestShowCmd) Run(ctx context.Context, stdout io.Writer) error {
	ref, err := name.ParseReference(c.Ref)
	if err != nil {
		return fmt.Errorf("cannot parse reference: %w", err)
	}
	opts := remoteOpts(ctx)
	_, om, err := module.ParseRemoteManifest(ref, "name", opts...)
	if err != nil {
		return err
	}
	ml, ok := module.ManifestLayer(om)
	if !ok {
		return fmt.Errorf("%s carries no %s layer: it was pushed without a %s (guestfn push publishes the manifest beside the module)", ref, manifest.LayerMediaType, manifest.FileName)
	}
	m, err := fetchManifest(ref, ml, opts)
	if err != nil {
		return fmt.Errorf("%s: %w", ref, err)
	}
	if c.Output == "json" {
		js, err := m.JSON()
		if err != nil {
			return err
		}
		_, _ = fmt.Fprintln(stdout, string(js))
		return nil
	}
	out, err := yaml.Marshal(m)
	if err != nil {
		return err
	}
	_, _ = stdout.Write(out)
	return nil
}

// fetchManifest pulls the manifest layer — kilobytes — and parses it as the
// runtime would.
func fetchManifest(ref name.Reference, layer v1.Descriptor, opts []remote.Option) (*manifest.Manifest, error) {
	raw, err := pullLayer(ref, layer, opts, manifest.MaxSize)
	if err != nil {
		return nil, fmt.Errorf("cannot fetch the manifest layer: %w", err)
	}
	m, err := manifest.Parse(raw)
	if err != nil {
		return nil, fmt.Errorf("the manifest layer is invalid: %w", err)
	}
	return m, nil
}

// pullLayer fetches one layer of an artifact and returns its bytes — a raw
// layer, or /fn.wasm out of a tar layer — bounded by limit.
func pullLayer(ref name.Reference, layer v1.Descriptor, opts []remote.Option, limit int64) ([]byte, error) {
	l, err := remote.Layer(ref.Context().Digest(layer.Digest.String()), opts...)
	if err != nil {
		return nil, fmt.Errorf("cannot fetch layer: %w", err)
	}
	rc, err := l.Compressed()
	if err != nil {
		return nil, fmt.Errorf("cannot read layer: %w", err)
	}
	defer func() { _ = rc.Close() }()
	b, err := io.ReadAll(io.LimitReader(rc, limit+1))
	if err != nil {
		return nil, fmt.Errorf("cannot read layer: %w", err)
	}
	if int64(len(b)) > limit {
		return nil, errors.New("layer exceeds the size limit")
	}
	if module.IsTarLayer(layer.MediaType) {
		return module.ExtractWasm(b, limit)
	}
	return b, nil
}
