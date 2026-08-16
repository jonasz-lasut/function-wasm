package module

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

// fileStamp remembers the digest of a served file by size and modification
// time, so an unchanged module is not re-hashed on every request.
type fileStamp struct {
	size   int64
	mtime  time.Time
	digest string
}

func (r *Resolver) resolvePath(src v1beta1.ModuleSource) (*Ref, error) {
	if r.opts.Dir == "" {
		return nil, errors.New("module.path is refused: the function was started without --module-dir")
	}
	if filepath.IsAbs(src.Path) {
		return nil, fmt.Errorf("module.path %q must be relative to the module directory", src.Path)
	}
	full := filepath.Join(r.opts.Dir, src.Path)
	if rel, err := filepath.Rel(r.opts.Dir, full); err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return nil, fmt.Errorf("module.path %q escapes the module directory", src.Path)
	}
	digest, err := r.fileDigest(full)
	if err != nil {
		return nil, err
	}
	return &Ref{
		Digest:      digest,
		Description: "module file " + src.Path,
		// Served files are on disk already; the blob store is skipped.
		fetch: func(context.Context) ([]byte, error) {
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
				return nil, fmt.Errorf("module file changed while being read: content is %s, expected %s", got, digest)
			}
			return b, nil
		},
	}, nil
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
