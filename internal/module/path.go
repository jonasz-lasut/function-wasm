package module

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
)

// fileStamp remembers the digest of a served file by size and modification
// time, so an unchanged module is not re-hashed on every request.
type fileStamp struct {
	size   int64
	mtime  time.Time
	digest string
}

func (r *Resolver) resolvePath(src v1beta1.ModuleSource) (*Ref, error) {
	full, err := r.confinedPath("module.path", src.Path)
	if err != nil {
		return nil, err
	}
	digest, err := r.fileDigest(full)
	if err != nil {
		return nil, err
	}
	out := &Ref{
		Digest:      digest,
		Description: "module file " + src.Path,
		// Served files are on disk already; the blob store is skipped.
		fetch: timed("path", func(context.Context) ([]byte, error) {
			f, err := os.Open(full) //nolint:gosec // full is confined to the module directory above.
			if err != nil {
				return nil, fmt.Errorf("cannot read module file: %w", err)
			}
			defer func() { _ = f.Close() }()
			b, err := readCapped(f, r.opts.MaxSize)
			if err != nil {
				return nil, err
			}
			if got := digestOf(b); got != digest {
				// The stamp lied (a same-size rewrite within the mtime
				// granularity): forget it so the next request re-hashes.
				r.files.Delete(full)
				return nil, fmt.Errorf("module file changed while being read: content is %s, expected %s", got, digest)
			}
			return b, nil
		}),
	}
	// The module's manifest, when the source names one: a wasmfn.yaml under
	// --module-dir, read fresh each request and normalized to JSON like an OCI
	// manifest layer. It carries no digest - the directory is the operator's.
	if src.ManifestPath != "" {
		manifestFull, err := r.confinedPath("module.manifestPath", src.ManifestPath)
		if err != nil {
			return nil, err
		}
		out.manifest = func(context.Context) ([]byte, bool, error) {
			f, err := os.Open(manifestFull) //nolint:gosec // manifestFull is confined to the module directory above.
			if err != nil {
				return nil, false, fmt.Errorf("cannot read manifest file: %w", err)
			}
			defer func() { _ = f.Close() }()
			b, err := readCapped(f, manifest.MaxSize)
			if err != nil {
				return nil, false, err
			}
			j, err := manifestJSON(b)
			if err != nil {
				return nil, false, err
			}
			return j, true, nil
		}
	}
	return out, nil
}

// confinedPath resolves rel under the module directory, refusing an absolute
// path or one that escapes the directory; field names it in the errors.
func (r *Resolver) confinedPath(field, rel string) (string, error) {
	if r.opts.Dir == "" {
		return "", fmt.Errorf("%s is refused: the function was started without --module-dir", field)
	}
	if filepath.IsAbs(rel) {
		return "", fmt.Errorf("%s %q must be relative to the module directory", field, rel)
	}
	full := filepath.Join(r.opts.Dir, rel)
	if rl, err := filepath.Rel(r.opts.Dir, full); err != nil || rl == ".." || strings.HasPrefix(rl, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("%s %q escapes the module directory", field, rel)
	}
	return full, nil
}

func (r *Resolver) fileDigest(path string) (string, error) {
	st, err := os.Stat(path)
	if err != nil {
		return "", fmt.Errorf("cannot stat module file: %w", err)
	}
	if st.IsDir() {
		return "", fmt.Errorf("module file %q is a directory", path)
	}
	if st.Size() > r.opts.MaxSize {
		return "", fmt.Errorf("module file is %d bytes, the limit is %d", st.Size(), r.opts.MaxSize)
	}
	if v, ok := r.files.Load(path); ok {
		if s := v.(fileStamp); s.size == st.Size() && s.mtime.Equal(st.ModTime()) {
			return s.digest, nil
		}
	}
	f, err := os.Open(path) //nolint:gosec // path is confined to the module directory by resolvePath.
	if err != nil {
		return "", fmt.Errorf("cannot read module file: %w", err)
	}
	defer func() { _ = f.Close() }()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", fmt.Errorf("cannot hash module file: %w", err)
	}
	digest := "sha256:" + hex.EncodeToString(h.Sum(nil))
	r.files.Store(path, fileStamp{size: st.Size(), mtime: st.ModTime(), digest: digest})
	return digest, nil
}
