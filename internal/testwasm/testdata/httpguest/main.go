// Package main is a test guest that performs one HTTP request through the
// host — wasmfn.HTTPClient over the wasmfn.http import — and reports the
// outcome as a result: "<status> <body>" on success, a fatal result carrying
// the transport error otherwise. The request comes from input.config:
// {url, method, body}. It speaks raw protobuf so it stays small.
package main

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"google.golang.org/protobuf/types/known/durationpb"

	"github.com/jonasz-lasut/function-wasm/pkg/wasmfn"
)

type config struct {
	URL    string `json:"url"`
	Method string `json:"method,omitempty"`
	Body   string `json:"body,omitempty"`
}

// Function is the guest.
type Function struct {
	client *http.Client
}

// RunFunction performs the configured request.
func (f *Function) RunFunction(ctx context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
	var cfg config
	if _, err := wasmfn.GetConfig(req, &cfg); err != nil {
		return nil, err
	}
	method := cfg.Method
	if method == "" {
		method = http.MethodGet
	}
	hreq, err := http.NewRequestWithContext(ctx, method, cfg.URL, strings.NewReader(cfg.Body))
	if err != nil {
		return nil, err
	}
	hreq.Header.Set("X-Guest", "httpguest")
	rsp, err := f.client.Do(hreq)
	if err != nil {
		return nil, err
	}
	defer rsp.Body.Close() //nolint:errcheck // Test guest.
	body, err := io.ReadAll(rsp.Body)
	if err != nil {
		return nil, err
	}
	return &fnv1.RunFunctionResponse{
		Meta: &fnv1.ResponseMeta{Tag: req.GetMeta().GetTag(), Ttl: durationpb.New(60 * time.Second)},
		Results: []*fnv1.Result{{
			Severity: fnv1.Severity_SEVERITY_NORMAL,
			Message:  fmt.Sprintf("%d %s", rsp.StatusCode, body),
		}},
	}, nil
}

func init() {
	wasmfn.Register(&Function{client: wasmfn.HTTPClient()})
}

func main() {}
