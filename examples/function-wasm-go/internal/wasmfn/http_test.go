package wasmfn

import (
	"context"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

// TestHTTPClient pins the transport behind HTTPClient: what it hands the
// wasi:http glue for an *http.Request, and what it makes of the host's
// answer.
func TestHTTPClient(t *testing.T) {
	type got struct {
		status  int
		headers http.Header
		body    string
	}
	type sent struct {
		method  string
		url     string
		headers http.Header
		body    string
	}
	type want struct {
		sent sent
		got  got
		err  string
	}
	cases := map[string]struct {
		reason   string
		request  func() *http.Request
		response *hostResponse
		refusal  error
		want     want
	}{
		"GetOK": {
			reason: "A GET is handed over with its URL and headers, and the host's status, headers and body come back as an http.Response.",
			request: func() *http.Request {
				req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://api.example.com/v1/items?limit=1", nil)
				req.Header.Set("Accept", "application/json")
				return req
			},
			response: &hostResponse{Status: 200, Headers: [][2]string{{"content-type", "application/json"}}, Body: []byte(`{"ok":true}`)},
			want: want{
				sent: sent{method: "GET", url: "https://api.example.com/v1/items?limit=1", headers: http.Header{"Accept": {"application/json"}}},
				got:  got{status: 200, headers: http.Header{"Content-Type": {"application/json"}}, body: `{"ok":true}`},
			},
		},
		"PostBody": {
			reason: "A request body is read whole and handed over as bytes.",
			request: func() *http.Request {
				req, _ := http.NewRequestWithContext(context.Background(), http.MethodPost, "https://api.example.com/v1/items", strings.NewReader(`{"name":"x"}`))
				return req
			},
			response: &hostResponse{Status: 201},
			want: want{
				sent: sent{method: "POST", url: "https://api.example.com/v1/items", headers: http.Header{}, body: `{"name":"x"}`},
				got:  got{status: 201, headers: http.Header{}, body: ""},
			},
		},
		"ServerError": {
			reason: "A status from the server, whatever it is, is a response, not an error.",
			request: func() *http.Request {
				req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://api.example.com/", nil)
				return req
			},
			response: &hostResponse{Status: 503, Body: []byte("busy")},
			want: want{
				sent: sent{method: "GET", url: "https://api.example.com/", headers: http.Header{}},
				got:  got{status: 503, headers: http.Header{}, body: "busy"},
			},
		},
		"Refused": {
			reason: "A request the host did not perform is the glue's *HTTPError, carrying the host's reason.",
			request: func() *http.Request {
				req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://evil.example.com/", nil)
				return req
			},
			refusal: &HTTPError{Reason: `sandbox.egress: no rule admits host "evil.example.com"`},
			want: want{
				sent: sent{method: "GET", url: "https://evil.example.com/", headers: http.Header{}},
				err:  `wasmfn: sandbox.egress: no rule admits host "evil.example.com"`,
			},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var handed sent
			httpCall = func(req hostRequest) (*hostResponse, error) {
				handed = sent{method: req.Method, url: req.URL.String(), headers: req.Headers, body: string(req.Body)}
				return tc.response, tc.refusal
			}
			t.Cleanup(func() { httpCall = hostHTTPCall })

			rsp, err := HTTPClient().Do(tc.request())

			if diff := cmp.Diff(tc.want.sent, handed, cmp.AllowUnexported(sent{})); diff != "" {
				t.Errorf("\n%s\nrequest handed to the host: -want, +got:\n%s", tc.reason, diff)
			}
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nDo(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
				var herr *HTTPError
				if !errors.As(err, &herr) {
					t.Errorf("\n%s\nDo(): a host refusal must be an *HTTPError, got %T", tc.reason, errors.Unwrap(err))
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nDo(): unexpected error %v", tc.reason, err)
			}
			defer rsp.Body.Close() //nolint:errcheck // Test.
			body, _ := io.ReadAll(rsp.Body)
			g := got{status: rsp.StatusCode, headers: rsp.Header, body: string(body)}
			if diff := cmp.Diff(tc.want.got, g, cmp.AllowUnexported(got{})); diff != "" {
				t.Errorf("\n%s\nDo(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

// TestHTTPClientNative pins that outside wasip1 the client fails with
// ErrNoHostHTTP rather than reaching the network.
func TestHTTPClientNative(t *testing.T) {
	req, err := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://api.example.com/", nil)
	if err != nil {
		t.Fatal(err)
	}
	rsp, err := HTTPClient().Do(req)
	if !errors.Is(err, ErrNoHostHTTP) {
		t.Fatalf("want ErrNoHostHTTP, got %v", err)
	}
	if rsp != nil {
		_ = rsp.Body.Close()
	}
}
