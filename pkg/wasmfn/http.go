package wasmfn

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
)

// httpCall hands one JSON request to the host's wasmfn.http import and
// returns its JSON response. The wasip1 build wires it to the import; other
// builds fail with ErrNoHostHTTP so a guest's native tests can swap in a
// fake.
var httpCall = hostHTTPCall

// ErrNoHostHTTP is what HTTPClient's transport returns outside a wasip1
// build: there is no host to ask.
var ErrNoHostHTTP = errors.New("wasmfn: no host HTTP in this build (not wasip1)")

// hostRequest is the JSON payload of one wasmfn.http call (docs/abi.md).
type hostRequest struct {
	Method  string              `json:"method,omitempty"`
	URL     string              `json:"url"`
	Headers map[string][]string `json:"headers,omitempty"`
	Body    []byte              `json:"body,omitempty"`
}

// hostResponse is what wasmfn.http returns: the server's status, headers and
// body, or Status 0 and an Error when the host did not perform the request
// (no sandbox.egress grant, a host or method outside it, a budget, a
// transport failure).
type hostResponse struct {
	Status  int                 `json:"status"`
	Headers map[string][]string `json:"headers,omitempty"`
	Body    []byte              `json:"body,omitempty"`
	Error   string              `json:"error,omitempty"`
}

// HTTPClient returns an *http.Client whose transport performs each request
// through the host — the wasmfn.http import — within the sandbox.egress
// grant of the Composition that runs the module. Anything that takes an
// *http.Client (cloud SDKs, generated API clients) works unchanged; the host
// resolves names, refuses what the grant or its policy does not admit,
// terminates TLS, follows redirects within the grant and enforces the
// budgets. A request the host did not perform fails with an *HTTPError
// carrying the host's reason. Outside a wasip1 build every request fails
// with ErrNoHostHTTP.
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
	hreq := hostRequest{Method: req.Method, URL: req.URL.String(), Headers: req.Header}
	if req.Body != nil {
		body, err := io.ReadAll(req.Body)
		_ = req.Body.Close()
		if err != nil {
			return nil, fmt.Errorf("wasmfn: cannot read request body: %w", err)
		}
		hreq.Body = body
	}
	payload, err := json.Marshal(hreq)
	if err != nil {
		return nil, fmt.Errorf("wasmfn: cannot encode request: %w", err)
	}
	out, err := httpCall(payload)
	if err != nil {
		return nil, err
	}
	var hrsp hostResponse
	if err := json.Unmarshal(out, &hrsp); err != nil {
		return nil, fmt.Errorf("wasmfn: cannot decode the host's response: %w", err)
	}
	if hrsp.Status == 0 {
		if hrsp.Error == "" {
			hrsp.Error = "the host returned no status and no error"
		}
		return nil, &HTTPError{Reason: hrsp.Error}
	}
	header := http.Header{}
	for k, vs := range hrsp.Headers {
		header[http.CanonicalHeaderKey(k)] = vs
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
