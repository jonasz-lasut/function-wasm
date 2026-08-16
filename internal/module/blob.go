package module

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// blobStore is a content-addressed cache of fetched modules on disk. Entries
// are immutable, so there is nothing to invalidate; a corrupt entry is
// dropped on read.
type blobStore struct {
	dir string
}

func newBlobStore(dir string) (*blobStore, error) {
	if err := os.MkdirAll(dir, 0o750); err != nil {
		return nil, fmt.Errorf("cannot create module cache directory: %w", err)
	}
	return &blobStore{dir: dir}, nil
}

func (s *blobStore) path(digest string) string {
	return filepath.Join(s.dir, strings.ReplaceAll(digest, ":", "-"))
}

func (s *blobStore) get(digest string) ([]byte, bool) {
	b, err := os.ReadFile(s.path(digest))
	if err != nil {
		return nil, false
	}
	if digestOf(b) != digest {
		_ = os.Remove(s.path(digest))
		return nil, false
	}
	return b, true
}

// put writes through a temporary file so a crash never leaves a partial
// entry that would be served as a module.
func (s *blobStore) put(digest string, b []byte) {
	tmp, err := os.CreateTemp(s.dir, "blob-*")
	if err != nil {
		return
	}
	if _, err := tmp.Write(b); err != nil {
		_ = tmp.Close()
		_ = os.Remove(tmp.Name())
		return
	}
	if err := tmp.Close(); err != nil {
		_ = os.Remove(tmp.Name())
		return
	}
	if err := os.Rename(tmp.Name(), s.path(digest)); err != nil {
		_ = os.Remove(tmp.Name())
	}
}
