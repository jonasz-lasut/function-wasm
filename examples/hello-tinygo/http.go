package main

import (
	"encoding/json"
	"errors"
	"strconv"
	"strings"
)

// HTTP egress through the host (docs/abi.md, "HTTP egress"): the guest never
// opens a socket; it hands the host a request and the host performs it within
// the Composition's sandbox.egress grant and the operator's egress policy, or
// answers with a refusal. The wire format is JSON both ways; the host writes
// its response into a buffer allocated through this guest's own wasmfn_alloc.

// HTTPRequest is one request for the host to perform.
type HTTPRequest struct {
	// Method of the request; empty means GET.
	Method string `json:"method,omitempty"`
	// URL, http or https, absolute.
	URL string `json:"url"`
	// Headers to send. Host, Content-Length and hop-by-hop headers are the
	// host's to set and are dropped.
	Headers map[string][]string `json:"headers,omitempty"`
	// Body bytes (base64 on the wire, as encoding/json renders []byte).
	Body []byte `json:"body,omitempty"`
}

// HTTPResponse is what the server answered: its status, whatever it is (a
// 503 is a response, not an error), headers and body.
type HTTPResponse struct {
	Status  int                 `json:"status"`
	Headers map[string][]string `json:"headers,omitempty"`
	Body    []byte              `json:"body,omitempty"`
	// Error is set instead of a status when the host did not perform the
	// request: refused by the grant or the policy, over a budget, or failed.
	Error string `json:"error,omitempty"`
}

// HTTPError is the host's reason for not performing a request. It is a
// distinct type so a guest can tell a refusal from a server error.
type HTTPError struct{ Reason string }

func (e *HTTPError) Error() string { return e.Reason }

// httpSink hands a JSON request to the host and returns its JSON response;
// the wasip1 build points it at the wasmfn.http import, other builds have no
// host and refuse — a native test substitutes its own.
var httpSink = noHostHTTP

func noHostHTTP([]byte) ([]byte, error) {
	return nil, errors.New("wasmfn: no host HTTP in this build (not running under function-wasm)")
}

// HTTPGet performs a GET through the host.
func HTTPGet(url string) (*HTTPResponse, error) {
	return HTTPDo(&HTTPRequest{Method: "GET", URL: url})
}

// HTTPDo performs req through the host. A request the host does not perform
// is an *HTTPError naming the reason; a response from the server, whatever
// its status, is returned as is.
func HTTPDo(req *HTTPRequest) (*HTTPResponse, error) {
	payload, err := json.Marshal(req)
	if err != nil {
		return nil, err
	}
	out, err := httpSink(payload)
	if err != nil {
		return nil, err
	}
	rsp := &HTTPResponse{}
	if err := json.Unmarshal(out, rsp); err != nil {
		return nil, errors.New("wasmfn: cannot decode the host's HTTP response: " + err.Error())
	}
	if rsp.Error != "" {
		return nil, &HTTPError{Reason: rsp.Error}
	}
	return rsp, nil
}

// httpGetText performs a GET and returns the body of a 200 as text, trimmed;
// any other status is an error naming it.
func httpGetText(url string) (string, error) {
	rsp, err := HTTPGet(url)
	if err != nil {
		return "", err
	}
	if rsp.Status != 200 {
		return "", errors.New("GET " + url + ": status " + strconv.Itoa(rsp.Status))
	}
	return strings.TrimSpace(string(rsp.Body)), nil
}
