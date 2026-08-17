package module

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"errors"
	"fmt"
	"io"
	"path"
	"slices"
	"strings"

	"github.com/google/go-containerregistry/pkg/authn"
	"github.com/google/go-containerregistry/pkg/name"
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/remote"
	"github.com/google/go-containerregistry/pkg/v1/types"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
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
func (r *Resolver) resolveOCI(_ context.Context, src v1beta1.ModuleSource, auth authn.Authenticator) (*Ref, error) {
	ref, err := name.NewDigest(src.OCI.Ref)
	if err != nil {
		return nil, fmt.Errorf("module.oci.ref is not a valid digest reference: %w", err)
	}

	// The fetch ref and its auth: a mirror replaces where bytes come from
	// and uses the runtime's own keychain - the mirror is the operator's,
	// not the Composition author's, so the step credential is not sent to a
	// host the author did not name. The stated ref is unchanged in the
	// cache key, description, audit line and policy.
	fetchRef := ref
	opts := []remote.Option{}
	if r.client.Transport != nil {
		opts = append(opts, remote.WithTransport(r.client.Transport))
	}
	if mr, ok := r.mirrorOf(ref); ok {
		fetchRef = mr
		opts = append(opts, remote.WithAuthFromKeychain(r.opts.Keychain))
	} else if auth != nil {
		opts = append(opts, remote.WithAuth(auth))
	} else {
		opts = append(opts, remote.WithAuthFromKeychain(r.opts.Keychain))
	}
	opts = slices.Clip(opts)

	out := &Ref{
		Digest:      ref.DigestStr(),
		Description: "oci " + src.OCI.Ref,
		fetch: timed("oci", func(ctx context.Context) ([]byte, error) {
			opts := append(opts, remote.WithContext(ctx))

			// Pick the module layer: from the layout when it holds this
			// manifest, from the registry otherwise.
			var layer v1.Descriptor
			if m, ok := r.layoutManifest(ref.DigestStr()); ok {
				l, err := WasmLayer(m)
				if err != nil {
					return nil, fmt.Errorf("%s %w", fetchRef, err)
				}
				layer = l
			} else {
				l, err := moduleLayer(fetchRef, opts)
				if err != nil {
					return nil, err
				}
				layer = l
			}

			b, err := r.verified(ctx, "module layer", layer.Digest.String(), func(_ context.Context) ([]byte, error) {
				// Layout first, then network.
				if b, ok := r.layoutBlob(layer.Digest); ok {
					return b, nil
				}
				l, err := remote.Layer(fetchRef.Context().Digest(layer.Digest.String()), opts...)
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
			if IsTarLayer(layer.MediaType) {
				return ExtractWasm(b, r.opts.MaxSize)
			}
			return b, nil
		}),
	}
	// The manifest layer, when the artifact has one: fetched through the
	// same manifest, verified against its own digest, bounded to
	// manifest.MaxSize; the caller stores it beside the compiled artifact so
	// this runs once per digest per volume.
	out.manifest = func(ctx context.Context) ([]byte, bool, error) {
		opts := append(opts, remote.WithContext(ctx))

		var (
			layer v1.Descriptor
			found bool
		)
		if m, ok := r.layoutManifest(ref.DigestStr()); ok {
			layer, found = ManifestLayer(m)
		} else {
			var err error
			layer, found, err = manifestLayer(fetchRef, opts)
			if err != nil {
				return nil, false, err
			}
		}
		if !found {
			return nil, false, nil
		}

		b, err := r.verified(ctx, "manifest layer", layer.Digest.String(), func(_ context.Context) ([]byte, error) {
			if b, ok := r.layoutBlob(layer.Digest); ok {
				return b, nil
			}
			l, err := remote.Layer(fetchRef.Context().Digest(layer.Digest.String()), opts...)
			if err != nil {
				return nil, fmt.Errorf("cannot fetch manifest layer: %w", err)
			}
			rc, err := l.Compressed()
			if err != nil {
				return nil, fmt.Errorf("cannot read manifest layer: %w", err)
			}
			defer func() { _ = rc.Close() }()
			b, err := readCapped(rc, manifest.MaxSize)
			if err != nil {
				return nil, fmt.Errorf("cannot read manifest layer: %w", err)
			}
			return b, nil
		})
		if err != nil {
			return nil, false, err
		}
		return b, true, nil
	}
	if r.opts.Verifier != nil {
		out.verify = func(ctx context.Context) error {
			if err := r.opts.Verifier.VerifyFromLayout(ref, r.layout); err == nil {
				return nil
			}
			return r.opts.Verifier.Verify(ctx, fetchRef, append(opts, remote.WithContext(ctx)))
		}
	}
	return out, nil
}

// mirrorOf returns the mirror ref for ref when the resolver has a mirror
// configured for ref's registry, or false otherwise.
func (r *Resolver) mirrorOf(ref name.Digest) (name.Digest, bool) {
	if len(r.mirrors) == 0 {
		return name.Digest{}, false
	}
	mirror, ok := r.mirrors[ref.Context().RegistryStr()]
	if !ok {
		return name.Digest{}, false
	}
	repo := ref.Context().RepositoryStr()
	d, err := name.NewDigest(mirror + "/" + repo + "@" + ref.DigestStr())
	if err != nil {
		return name.Digest{}, false
	}
	return d, true
}

// manifestLayer fetches an artifact's manifest and picks its module-manifest
// layer, if it has one.
func manifestLayer(ref name.Digest, opts []remote.Option) (v1.Descriptor, bool, error) {
	desc, err := remote.Get(ref, opts...)
	if err != nil {
		return v1.Descriptor{}, false, fmt.Errorf("cannot fetch manifest %s: %w", ref, err)
	}
	if desc.MediaType.IsIndex() {
		return v1.Descriptor{}, false, fmt.Errorf("%s is an image index; reference the manifest holding the module", ref)
	}
	m, err := v1.ParseManifest(bytes.NewReader(desc.Manifest))
	if err != nil {
		return v1.Descriptor{}, false, fmt.Errorf("cannot parse manifest %s: %w", ref, err)
	}
	l, ok := ManifestLayer(m)
	return l, ok, nil
}

// ManifestLayer picks the module-manifest layer of an artifact's manifest —
// the layer of media type manifest.LayerMediaType — if there is one; the
// rule guestfn inspect and the resolver share.
func ManifestLayer(m *v1.Manifest) (v1.Descriptor, bool) {
	for _, l := range m.Layers {
		if string(l.MediaType) == manifest.LayerMediaType {
			return l, true
		}
	}
	return v1.Descriptor{}, false
}

// moduleLayer fetches a manifest and picks the layer holding the module.
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
	l, err := WasmLayer(m)
	if err != nil {
		return v1.Descriptor{}, fmt.Errorf("%s %w", ref, err)
	}
	return l, nil
}

// WasmLayer picks the layer of a manifest that holds the module: a
// wasm-typed layer if there is one, otherwise the only layer that is not the
// module-manifest layer — the rule the resolver applies to every OCI source,
// shared with guestfn inspect.
func WasmLayer(m *v1.Manifest) (v1.Descriptor, error) {
	var candidates []v1.Descriptor
	for _, l := range m.Layers {
		if wasmLayerTypes[l.MediaType] {
			return l, nil
		}
		if string(l.MediaType) != manifest.LayerMediaType {
			candidates = append(candidates, l)
		}
	}
	if len(candidates) == 1 {
		return candidates[0], nil
	}
	return v1.Descriptor{}, fmt.Errorf("has %d layers and none is a wasm layer", len(m.Layers))
}

// ScratchModulePath is where a FROM scratch image must hold the module:
// `COPY fn.wasm /`. It is the one path looked for in a tar layer — nothing
// is guessed from the archive's contents, and the name is not configurable.
const ScratchModulePath = "/fn.wasm"

// IsTarLayer reports whether a layer media type is a tar archive — a FROM
// scratch image holding the module as a file — rather than a raw module.
func IsTarLayer(mt types.MediaType) bool {
	return strings.Contains(string(mt), "tar")
}

// ExtractWasm returns ScratchModulePath from a (possibly gzipped) tar layer,
// which is how a `FROM scratch; COPY fn.wasm /` image stores a module; an
// archive without that exact entry is refused, whatever else it holds. The
// archive may expand to at most eight times limit before the entry is found:
// a gzip bomb costs bounded work, not the process.
func ExtractWasm(b []byte, limit int64) ([]byte, error) {
	var rd io.Reader = bytes.NewReader(b)
	if bytes.HasPrefix(b, []byte{0x1f, 0x8b}) {
		zr, err := gzip.NewReader(rd)
		if err != nil {
			return nil, fmt.Errorf("cannot decompress module layer: %w", err)
		}
		defer func() { _ = zr.Close() }()
		rd = zr
	}
	rd = &cappedReader{r: rd, left: 8 * limit}
	tr := tar.NewReader(rd)
	for {
		h, err := tr.Next()
		if errors.Is(err, io.EOF) {
			return nil, fmt.Errorf("module layer is a tar archive without %s: a FROM scratch image must COPY the module to %s", ScratchModulePath, ScratchModulePath)
		}
		if err != nil {
			return nil, fmt.Errorf("cannot read module layer archive: %w", err)
		}
		// Archives name the entry as fn.wasm, ./fn.wasm or /fn.wasm depending
		// on the builder; all are the root's fn.wasm, and nothing else is.
		if h.Typeflag == tar.TypeReg && path.Clean("/"+h.Name) == ScratchModulePath {
			return readCapped(tr, limit)
		}
	}
}

// cappedReader fails once more than its budget has been read.
type cappedReader struct {
	r    io.Reader
	left int64
}

func (c *cappedReader) Read(p []byte) (int, error) {
	if c.left <= 0 {
		return 0, fmt.Errorf("module layer archive exceeds the size limit before %s", ScratchModulePath)
	}
	if int64(len(p)) > c.left {
		p = p[:c.left]
	}
	n, err := c.r.Read(p)
	c.left -= int64(n)
	return n, err
}
