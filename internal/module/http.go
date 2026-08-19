package module

import (
	"context"
	"fmt"
	"net/http"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/manifest"
)

func (r *Resolver) resolveHTTP(src v1beta1.ModuleSource) (*Ref, error) {
	url := src.HTTP.URL
	digest := src.HTTP.Digest
	out := &Ref{
		Digest:      digest,
		Description: "http " + url,
		fetch: timed("http", func(ctx context.Context) ([]byte, error) {
			return r.verified(ctx, "module", digest, func(ctx context.Context) ([]byte, error) {
				return r.httpGet(ctx, url, "module", r.opts.MaxSize)
			})
		}),
	}
	// The module's manifest, when the source names one: a wasmfn.yaml served
	// beside the module, verified against its own digest, bounded to
	// manifest.MaxSize and normalized to JSON like an OCI manifest layer.
	if src.HTTP.ManifestURL != "" {
		manifestURL, manifestDigest := src.HTTP.ManifestURL, src.HTTP.ManifestDigest
		// The manifest is content-addressed by its own digest, so it keys the
		// caches: two sources naming the same manifest share the parsed result.
		out.manifestKey = manifestDigest
		out.manifest = func(ctx context.Context) ([]byte, bool, error) {
			raw, err := r.verified(ctx, "manifest", manifestDigest, func(ctx context.Context) ([]byte, error) {
				return r.httpGet(ctx, manifestURL, "manifest", manifest.MaxSize)
			})
			if err != nil {
				return nil, false, err
			}
			j, err := manifestJSON(raw)
			if err != nil {
				return nil, false, err
			}
			return j, true, nil
		}
	}
	return out, nil
}

// httpGet fetches what names the resource in the errors (module, manifest)
// from url, bounded to limit.
func (r *Resolver) httpGet(ctx context.Context, url, what string, limit int64) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, fmt.Errorf("cannot build request: %w", err)
	}
	rsp, err := r.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("cannot download %s: %w", what, err)
	}
	defer func() { _ = rsp.Body.Close() }()
	if rsp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("cannot download %s: %s", what, rsp.Status)
	}
	return readCapped(rsp.Body, limit)
}
