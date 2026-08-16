// Package cache is the on-disk side of function-wasm's two caches: fetched
// modules and compiled wasmtime artifacts, both content-addressed by the
// module's digest and both under one directory the runtime owns.
//
// Layout (see docs/one-pager-cache.md):
//
//	/tmp/function-wasm-cache/
//	  modules/sha256-<hex>                    the module bytes as fetched, verified on read
//	  compiled/<wasmtime>-<goarch>/sha256-<hex>  wasmtime's serialized code for that module
//
// The stores are afero filesystems so tests run against an in-memory one and
// a different base directory is a one-line change.
package cache

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"path"
	"strings"

	"github.com/spf13/afero"
)

// DefaultDir is where the runtime keeps both caches. It is a fixed location:
// a pod that must survive restarts backs it with a volume, one with a
// read-only root filesystem mounts an emptyDir there.
const DefaultDir = "/tmp/function-wasm-cache"

// Subdirectories of DefaultDir.
const (
	ModulesDir  = "modules"
	CompiledDir = "compiled"
)

// Store is a content-addressed store of one kind of artifact.
type Store struct {
	fs     afero.Fs
	verify bool
}

// New returns a store on fs. With verify, Get recomputes the digest of what
// it read and drops entries that do not match — for fetched modules, whose
// digest is their address; serialized code is addressed by the module's
// digest, not its own, and wasmtime validates it on load instead.
func New(fs afero.Fs, verify bool) *Store {
	return &Store{fs: fs, verify: verify}
}

// Get returns the artifact stored under digest.
func (s *Store) Get(digest string) ([]byte, bool) {
	name := fileName(digest)
	b, err := afero.ReadFile(s.fs, name)
	if err != nil {
		return nil, false
	}
	if s.verify && digestOf(b) != digest {
		_ = s.fs.Remove(name)
		return nil, false
	}
	return b, true
}

// Put stores b under digest, through a temporary file and a rename so a crash
// never leaves a partial entry that a later Get would serve.
func (s *Store) Put(digest string, b []byte) error {
	if s.verify && digestOf(b) != digest {
		return fmt.Errorf("content is %s, not %s", digestOf(b), digest)
	}
	// The temporary name is built here rather than by afero.TempFile: on a
	// base-path filesystem TempFile reports the real path, which Rename
	// would prefix again.
	var nonce [8]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return fmt.Errorf("cannot name cache entry: %w", err)
	}
	tmp := fileName(digest) + ".put-" + hex.EncodeToString(nonce[:])
	if err := afero.WriteFile(s.fs, tmp, b, 0o600); err != nil {
		_ = s.fs.Remove(tmp)
		return fmt.Errorf("cannot write cache entry: %w", err)
	}
	if err := s.fs.Rename(tmp, fileName(digest)); err != nil {
		_ = s.fs.Remove(tmp)
		return fmt.Errorf("cannot store cache entry: %w", err)
	}
	return nil
}

// Len counts stored artifacts (for tests and diagnostics).
func (s *Store) Len() int {
	entries, err := afero.ReadDir(s.fs, ".")
	if err != nil {
		return 0
	}
	n := 0
	for _, e := range entries {
		if !e.IsDir() && !strings.Contains(e.Name(), ".put-") {
			n++
		}
	}
	return n
}

// Subdir returns a store rooted at dir under fs, creating it.
func Subdir(base afero.Fs, dir string, verify bool) (*Store, error) {
	if err := base.MkdirAll(dir, 0o750); err != nil {
		return nil, fmt.Errorf("cannot create cache directory %s: %w", dir, err)
	}
	return New(afero.NewBasePathFs(base, dir), verify), nil
}

// RemoveOthers deletes every subdirectory of dir on base except keep — used
// to drop compiled artifacts of other wasmtime versions at startup, which no
// running engine can load.
func RemoveOthers(base afero.Fs, dir, keep string) error {
	entries, err := afero.ReadDir(base, dir)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil
		}
		return err
	}
	for _, e := range entries {
		if e.IsDir() && e.Name() != keep {
			if err := base.RemoveAll(path.Join(dir, e.Name())); err != nil {
				return err
			}
		}
	}
	return nil
}

func fileName(digest string) string {
	return strings.Replace(digest, ":", "-", 1)
}

func digestOf(b []byte) string {
	sum := sha256.Sum256(b)
	return "sha256:" + hex.EncodeToString(sum[:])
}
