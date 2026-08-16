package module

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/google/go-containerregistry/pkg/authn"
	"github.com/google/go-containerregistry/pkg/name"
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/remote"
	"github.com/google/go-containerregistry/pkg/v1/types"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

// Media types recognised as a raw wasm module layer: the CNCF wasm OCI
// artifact layout (application/wasm) and the older wasm-to-oci / Spin /
// wasmCloud conventions.
var wasmLayerTypes = map[types.MediaType]bool{
	"application/wasm":                                  true,
	"application/vnd.wasm.content.layer.v1+wasm":        true,
	"application/vnd.module.wasm.content.layer.v1+wasm": true,
}

// layerInfo is what a manifest resolves to: the one layer holding the module.
type layerInfo struct {
	digest    string
	mediaType types.MediaType
}

func (r *Resolver) resolveOCI(ctx context.Context, src v1beta1.ModuleSource, auth authn.Authenticator) (*Ref, error) {
	ref, err := name.ParseReference(src.OCI.Ref)
	if err != nil {
		return nil, fmt.Errorf("module.oci.ref is not a valid reference: %w", err)
	}
	opts := []remote.Option{remote.WithContext(ctx)}
	if r.client.Transport != nil {
		opts = append(opts, remote.WithTransport(r.client.Transport))
	}
	if auth != nil {
		opts = append(opts, remote.WithAuth(auth))
	} else {
		opts = append(opts, remote.WithAuthFromKeychain(r.opts.Keychain))
	}

	manifestDigest, err := r.manifestDigest(ref, opts)
	if err != nil {
		return nil, err
	}
	pinned := ref.Context().Digest(manifestDigest)
	if r.opts.Verifier != nil {
		if err := r.opts.Verifier.Verify(ctx, pinned, opts); err != nil {
			return nil, err
		}
	}
	layer, err := r.layers.get(manifestDigest, func() (layerInfo, error) {
		return moduleLayer(pinned, opts)
	})
	if err != nil {
		return nil, err
	}
	digest, err := pin(src.Digest, layer.digest)
	if err != nil {
		return nil, err
	}
	description := "oci " + src.OCI.Ref
	if _, isTag := ref.(name.Tag); isTag {
		description += " (" + manifestDigest + ")"
	}
	blob := ref.Context().Digest(layer.digest)
	fetch := r.verified("oci", digest, func(ctx context.Context) ([]byte, error) {
		l, err := remote.Layer(blob, append(opts, remote.WithContext(ctx))...)
		if err != nil {
			return nil, fmt.Errorf("cannot fetch module layer: %w", err)
		}
		rc, err := l.Compressed()
		if err != nil {
			return nil, fmt.Errorf("cannot read module layer: %w", err)
		}
		defer func() { _ = rc.Close() }()
		return readCapped(rc, r.opts.MaxSize)
	})
	return &Ref{
		Digest:      digest,
		Description: description,
		fetch: func(ctx context.Context) ([]byte, error) {
			b, err := fetch(ctx)
			if err != nil {
				return nil, err
			}
			if isTarLayer(layer.mediaType) {
				return extractWasm(b, r.opts.MaxSize)
			}
			return b, nil
		},
	}, nil
}

// manifestDigest resolves a reference to its manifest digest: a digest
// reference is its own answer, a tag is looked up and cached for TagTTL.
func (r *Resolver) manifestDigest(ref name.Reference, opts []remote.Option) (string, error) {
	if d, ok := ref.(name.Digest); ok {
		return d.DigestStr(), nil
	}
	return r.tags.get(ref.String(), func() (string, error) {
		desc, err := remote.Head(ref, opts...)
		if err != nil {
			return "", fmt.Errorf("cannot resolve %s: %w", ref, err)
		}
		return desc.Digest.String(), nil
	})
}

// moduleLayer fetches a manifest and picks the layer holding the module: a
// wasm-typed layer if there is one, otherwise the only layer.
func moduleLayer(ref name.Digest, opts []remote.Option) (layerInfo, error) {
	desc, err := remote.Get(ref, opts...)
	if err != nil {
		return layerInfo{}, fmt.Errorf("cannot fetch manifest %s: %w", ref, err)
	}
	if desc.MediaType.IsIndex() {
		return layerInfo{}, fmt.Errorf("%s is an image index; reference the manifest holding the module", ref)
	}
	m, err := v1.ParseManifest(bytes.NewReader(desc.Manifest))
	if err != nil {
		return layerInfo{}, fmt.Errorf("cannot parse manifest %s: %w", ref, err)
	}
	for _, l := range m.Layers {
		if wasmLayerTypes[l.MediaType] {
			return layerInfo{digest: l.Digest.String(), mediaType: l.MediaType}, nil
		}
	}
	if len(m.Layers) == 1 {
		return layerInfo{digest: m.Layers[0].Digest.String(), mediaType: m.Layers[0].MediaType}, nil
	}
	return layerInfo{}, fmt.Errorf("%s has %d layers and none is a wasm layer", ref, len(m.Layers))
}

func isTarLayer(mt types.MediaType) bool {
	return strings.Contains(string(mt), "tar")
}

// extractWasm returns the first .wasm file of a (possibly gzipped) tar layer,
// which is how a `FROM scratch; COPY fn.wasm /` image stores a module.
func extractWasm(b []byte, limit int64) ([]byte, error) {
	var rd io.Reader = bytes.NewReader(b)
	if bytes.HasPrefix(b, []byte{0x1f, 0x8b}) {
		zr, err := gzip.NewReader(rd)
		if err != nil {
			return nil, fmt.Errorf("cannot decompress module layer: %w", err)
		}
		defer func() { _ = zr.Close() }()
		rd = zr
	}
	tr := tar.NewReader(rd)
	for {
		h, err := tr.Next()
		if errors.Is(err, io.EOF) {
			return nil, errors.New("module layer is a tar archive without a .wasm file")
		}
		if err != nil {
			return nil, fmt.Errorf("cannot read module layer archive: %w", err)
		}
		if h.Typeflag == tar.TypeReg && strings.HasSuffix(h.Name, ".wasm") {
			return readCapped(tr, limit)
		}
	}
}
