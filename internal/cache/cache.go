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
	"os"
	"path"
	"path/filepath"
	"sort"
	"strings"
	"time"

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
	// dir is the store's directory on the host filesystem when it has one,
	// so an entry can be handed to code that maps files (see Path).
	dir string
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
	s.touch(name)
	return b, true
}

// touch records a use: the sweep drops the least recently used entries, and
// a read is a use. Best effort — a read-only volume still serves.
func (s *Store) touch(name string) {
	now := time.Now()
	_ = s.fs.Chtimes(name, now, now)
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

// OpenDir returns a store on the host filesystem directory dir, creating it.
// Its entries have a Path, so artifacts can be mapped instead of read.
func OpenDir(dir string, verify bool) (*Store, error) {
	if err := os.MkdirAll(dir, 0o750); err != nil {
		return nil, fmt.Errorf("cannot create cache directory %s: %w", dir, err)
	}
	s := New(afero.NewBasePathFs(afero.NewOsFs(), dir), verify)
	s.dir = dir
	return s, nil
}

// Path returns the host filesystem path of the entry stored under digest,
// when the store is on the host filesystem and the entry exists. Callers
// that map the file get its content without a copy; verification is the
// caller's (the compiled store does not verify — wasmtime validates what it
// loads).
func (s *Store) Path(digest string) (string, bool) {
	if s.dir == "" {
		return "", false
	}
	p := filepath.Join(s.dir, fileName(digest))
	if st, err := os.Stat(p); err != nil || st.IsDir() {
		return "", false
	}
	s.touch(fileName(digest))
	return p, true
}

// Entry describes a stored blob for the sweep.
type Entry struct {
	Store    *Store
	Name     string
	Size     int64
	LastUsed time.Time
}

// Entries lists the store's blobs, temporary files excluded.
func (s *Store) Entries() ([]Entry, error) {
	infos, err := afero.ReadDir(s.fs, ".")
	if err != nil {
		return nil, err
	}
	var out []Entry
	for _, e := range infos {
		if e.IsDir() || strings.Contains(e.Name(), ".put-") {
			continue
		}
		out = append(out, Entry{Store: s, Name: e.Name(), Size: e.Size(), LastUsed: e.ModTime()})
	}
	return out, nil
}

// Bytes is the total size of the store's blobs.
func (s *Store) Bytes() int64 {
	entries, err := s.Entries()
	if err != nil {
		return 0
	}
	var n int64
	for _, e := range entries {
		n += e.Size
	}
	return n
}

// Sweep removes least recently used blobs across stores until they hold at
// most maxBytes together — down to nine tenths of it, so consecutive sweeps
// do not each remove one entry. Entries are immutable and reproducible, so
// removing one is always safe: the next request for it fetches or compiles
// again. It reports the bytes freed. maxBytes <= 0 sweeps nothing.
func Sweep(stores []*Store, maxBytes int64) (freed int64, err error) {
	if maxBytes <= 0 {
		return 0, nil
	}
	var all []Entry
	var total int64
	for _, s := range stores {
		entries, err := s.Entries()
		if err != nil {
			return 0, err
		}
		for _, e := range entries {
			total += e.Size
		}
		all = append(all, entries...)
	}
	if total <= maxBytes {
		return 0, nil
	}
	sort.Slice(all, func(i, j int) bool { return all[i].LastUsed.Before(all[j].LastUsed) })
	target := maxBytes / 10 * 9
	for _, e := range all {
		if total <= target {
			break
		}
		if err := e.Store.fs.Remove(e.Name); err != nil && !errors.Is(err, fs.ErrNotExist) {
			return freed, err
		}
		total -= e.Size
		freed += e.Size
	}
	return freed, nil
}

// StaleVersionAge is how long a compiled-artifact directory of another
// wasmtime version must have gone without a write before it is removed:
// during a rolling upgrade old and new pods share a volume, and each would
// otherwise delete the other's artifacts at startup.
const StaleVersionAge = 24 * time.Hour

// RemoveOthers deletes the subdirectories of dir on base other than keep
// whose newest entry is older than StaleVersionAge — compiled artifacts of
// wasmtime versions no running engine uses any more. now is the reference
// time (tests pass a fixed one).
func RemoveOthers(base afero.Fs, dir, keep string, now time.Time) error {
	entries, err := afero.ReadDir(base, dir)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil
		}
		return err
	}
	for _, e := range entries {
		if !e.IsDir() || e.Name() == keep {
			continue
		}
		sub := path.Join(dir, e.Name())
		newest := e.ModTime()
		files, err := afero.ReadDir(base, sub)
		if err != nil {
			return err
		}
		for _, f := range files {
			if f.ModTime().After(newest) {
				newest = f.ModTime()
			}
		}
		if now.Sub(newest) < StaleVersionAge {
			continue
		}
		if err := base.RemoveAll(sub); err != nil {
			return err
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
