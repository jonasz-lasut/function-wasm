package main

import (
	"context"

	"github.com/google/go-containerregistry/pkg/authn"
	"github.com/google/go-containerregistry/pkg/name"
	v1 "github.com/google/go-containerregistry/pkg/v1"
	"github.com/google/go-containerregistry/pkg/v1/remote"
)

// remoteOpts are the registry options every guestfn command uses: the default
// keychain (Docker config) and the command's context.
func remoteOpts(ctx context.Context) []remote.Option {
	return []remote.Option{remote.WithAuthFromKeychain(authn.DefaultKeychain), remote.WithContext(ctx)}
}

// pinnedRef renders ref pinned to digest — the reference a Composition should
// hold. A ref already pinned to a digest is returned unchanged; any other
// reference (a tag) keeps its readable form with the digest appended.
func pinnedRef(ref name.Reference, digest v1.Hash) string {
	pinned := ref.String()
	if _, ok := ref.(name.Digest); !ok {
		pinned += "@" + digest.String()
	}
	return pinned
}
