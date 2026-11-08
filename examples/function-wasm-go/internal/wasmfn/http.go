package wasmfn

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
)

// httpCall performs one request through the host. The wasip1 build wires it
// to wasi:http@0.2's outgoing-handler; other builds fail with ErrNoHostHTTP
// so a guest's native tests can swap in a fake.
var httpCall = hostHTTPCall

// ErrNoHostHTTP is what HTTPClient's transport returns outside a wasip1
// build: there is no host to ask.
var ErrNoHostHTTP = errors.New("wasmfn: no host HTTP in this build (not wasip1)")

// hostRequest is one request as the wasi:http glue takes it: the body read
// whole, as the host answers it (bodies are complete on both sides).
type hostRequest struct {
	Method  string
	URL     *url.URL
	Headers http.Header
	Body    []byte
}

// hostResponse is what the host answered: status, headers in wire order and
// the whole body.
type hostResponse struct {
	Status  int
	Headers [][2]string
	Body    []byte
}

// HTTPClient returns an *http.Client whose transport performs each request
// through the host - wasi:http - within the egress grant the module's
// manifest requires (requires.egress.http), as the runtime's policy layers
// permit it. Anything that takes an *http.Client (cloud SDKs, generated API
// clients) works unchanged; the host resolves names, refuses what the grant
// or its policy does not admit, terminates TLS, follows redirects within the
// grant and enforces the budgets. A request the host did not perform fails
// with an *HTTPError carrying the host's reason. Outside a wasip1 build every
// request fails with ErrNoHostHTTP.
func HTTPClient() *http.Client {
	return &http.Client{Transport: hostTransport{}}
}

// HTTPError is the error HTTPClient's transport returns when the host did
// not perform a request; Reason is the host's message.
type HTTPError struct {
	Reason string
}

func (e *HTTPError) Error() string { return "wasmfn: " + e.Reason }

// hostTransport is the RoundTripper behind HTTPClient.
type hostTransport struct{}

func (hostTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	if req.URL == nil {
		return nil, errors.New("wasmfn: request has no URL")
	}
	hreq := hostRequest{Method: req.Method, URL: req.URL, Headers: req.Header}
	if req.Body != nil {
		body, err := io.ReadAll(req.Body)
		_ = req.Body.Close()
		if err != nil {
			return nil, fmt.Errorf("wasmfn: cannot read request body: %w", err)
		}
		hreq.Body = body
	}
	hrsp, err := httpCall(hreq)
	if err != nil {
		return nil, err
	}
	header := http.Header{}
	for _, kv := range hrsp.Headers {
		header.Add(kv[0], kv[1])
	}
	return &http.Response{
		Status:        fmt.Sprintf("%d %s", hrsp.Status, http.StatusText(hrsp.Status)),
		StatusCode:    hrsp.Status,
		Proto:         "HTTP/1.1",
		ProtoMajor:    1,
		ProtoMinor:    1,
		Header:        header,
		Body:          io.NopCloser(bytes.NewReader(hrsp.Body)),
		ContentLength: int64(len(hrsp.Body)),
		Request:       req,
	}, nil
}
