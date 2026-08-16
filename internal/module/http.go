package module

import (
	"context"
	"fmt"
	"net/http"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
)

func (r *Resolver) resolveHTTP(src v1beta1.ModuleSource) (*Ref, error) {
	url := src.HTTP.URL
	return &Ref{
		Digest:      src.Digest,
		Description: url,
		fetch: r.verified("http", src.Digest, func(ctx context.Context) ([]byte, error) {
			req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
			if err != nil {
				return nil, fmt.Errorf("cannot build request: %w", err)
			}
			rsp, err := r.client.Do(req)
			if err != nil {
				return nil, fmt.Errorf("cannot download module: %w", err)
			}
			defer func() { _ = rsp.Body.Close() }()
			if rsp.StatusCode != http.StatusOK {
				return nil, fmt.Errorf("cannot download module: %s", rsp.Status)
			}
			return readCapped(rsp.Body, r.opts.MaxSize)
		}),
	}, nil
}
