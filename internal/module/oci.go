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

// resolveOCI resolves without touching the registry: the reference pins the
// manifest, the manifest pins its layer, and the layer is the module. The
// manifest digest keys the caches; the manifest is fetched only when the
// module has to be, inside Fetch, and the layer is stored in the blob store
// under its own digest, so a module whose compiled artifact is gone costs one
// manifest read and no download.
func (r *Resolver) resolveOCI(ctx context.Context, src v1beta1.ModuleSource, auth authn.Authenticator) (*Ref, error) {
	ref, err := name.NewDigest(src.OCI.Ref)
	if err != nil {
		return nil, fmt.Errorf("module.oci.ref is not a valid digest reference: %w", err)
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
	return &Ref{
		Digest:      ref.DigestStr(),
		Description: "oci " + src.OCI.Ref,
		fetch: timed("oci", func(ctx context.Context) ([]byte, error) {
			opts := append(opts, remote.WithContext(ctx))
			if r.opts.Verifier != nil {
				if err := r.opts.Verifier.Verify(ctx, ref, opts); err != nil {
					return nil, err
				}
			}
			layer, err := moduleLayer(ref, opts)
			if err != nil {
				return nil, err
			}
			b, err := r.verified(ctx, "module layer", layer.Digest.String(), func(_ context.Context) ([]byte, error) {
				l, err := remote.Layer(ref.Context().Digest(layer.Digest.String()), opts...)
				if err != nil {
					return nil, fmt.Errorf("cannot fetch module layer: %w", err)
				}
				rc, err := l.Compressed()
				if err != nil {
					return nil, fmt.Errorf("cannot read module layer: %w", err)
				}
				defer func() { _ = rc.Close() }()
				b, err := readCapped(rc, r.opts.MaxSize)
				if err != nil {
					return nil, fmt.Errorf("cannot read module layer: %w", err)
				}
				return b, nil
			})
			if err != nil {
				return nil, err
			}
			if isTarLayer(layer.MediaType) {
				return extractWasm(b, r.opts.MaxSize)
			}
			return b, nil
		}),
	}, nil
}

// moduleLayer fetches a manifest and picks the layer holding the module: a
// wasm-typed layer if there is one, otherwise the only layer.
func moduleLayer(ref name.Digest, opts []remote.Option) (v1.Descriptor, error) {
	desc, err := remote.Get(ref, opts...)
	if err != nil {
		return v1.Descriptor{}, fmt.Errorf("cannot fetch manifest %s: %w", ref, err)
	}
	if desc.MediaType.IsIndex() {
		return v1.Descriptor{}, fmt.Errorf("%s is an image index; reference the manifest holding the module", ref)
	}
	m, err := v1.ParseManifest(bytes.NewReader(desc.Manifest))
	if err != nil {
		return v1.Descriptor{}, fmt.Errorf("cannot parse manifest %s: %w", ref, err)
	}
	for _, l := range m.Layers {
		if wasmLayerTypes[l.MediaType] {
			return l, nil
		}
	}
	if len(m.Layers) == 1 {
		return m.Layers[0], nil
	}
	return v1.Descriptor{}, fmt.Errorf("%s has %d layers and none is a wasm layer", ref, len(m.Layers))
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
