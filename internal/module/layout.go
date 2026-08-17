package module

import (
	"bytes"
	"fmt"

	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/layout"
)

// layoutManifest reads and parses a manifest from the OCI layout by its
// digest, returning nil when the layout is not configured or the manifest
// is not in it.
func (r *Resolver) layoutManifest(digest string) (*v1.Manifest, bool) {
	if r.layout == "" {
		return nil, false
	}
	h, err := v1.NewHash(digest)
	if err != nil {
		return nil, false
	}
	raw, err := r.layout.Bytes(h)
	if err != nil {
		return nil, false
	}
	m, err := v1.ParseManifest(bytes.NewReader(raw))
	if err != nil {
		return nil, false
	}
	return m, true
}

// layoutBlob reads a blob from the OCI layout by its hash, returning nil
// when the layout is not configured or the blob is not in it.
func (r *Resolver) layoutBlob(h v1.Hash) ([]byte, bool) {
	if r.layout == "" {
		return nil, false
	}
	b, err := r.layout.Bytes(h)
	if err != nil {
		return nil, false
	}
	return b, true
}

// openLayout opens the OCI layout at dir, or returns an empty Path when
// dir is empty.
func openLayout(dir string) (layout.Path, error) {
	if dir == "" {
		return "", nil
	}
	lp, err := layout.FromPath(dir)
	if err != nil {
		return "", fmt.Errorf("cannot open OCI layout at %s: %w", dir, err)
	}
	return lp, nil
}
