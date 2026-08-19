package wasmfn

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

// TestHTTPClient pins the codec of the wasmfn.http import: what the transport
// hands the host for an *http.Request, and what it makes of the host's answer.
func TestHTTPClient(t *testing.T) {
	type got struct {
		status  int
		headers http.Header
		body    string
	}
	type want struct {
		request hostRequest
		got     got
		err     string
	}
	cases := map[string]struct {
		reason   string
		request  func() *http.Request
		response string
		want     want
	}{
		"GetOK": {
			reason: "A GET is encoded with its URL and headers, and the host's status, headers and body come back as an http.Response.",
			request: func() *http.Request {
				req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://api.example.com/v1/items?limit=1", nil)
				req.Header.Set("Accept", "application/json")
				return req
			},
			response: `{"status":200,"headers":{"Content-Type":["application/json"]},"body":"eyJvayI6dHJ1ZX0="}`,
			want: want{
				request: hostRequest{Method: "GET", URL: "https://api.example.com/v1/items?limit=1", Headers: map[string][]string{"Accept": {"application/json"}}},
				got:     got{status: 200, headers: http.Header{"Content-Type": {"application/json"}}, body: `{"ok":true}`},
			},
		},
		"PostBody": {
			reason: "A request body is read and sent as bytes (base64 on the wire).",
			request: func() *http.Request {
				req, _ := http.NewRequestWithContext(context.Background(), http.MethodPost, "https://api.example.com/v1/items", strings.NewReader(`{"name":"x"}`))
				return req
			},
			response: `{"status":201}`,
			want: want{
				request: hostRequest{Method: "POST", URL: "https://api.example.com/v1/items", Body: []byte(`{"name":"x"}`)},
				got:     got{status: 201, headers: http.Header{}, body: ""},
			},
		},
		"ServerError": {
			reason: "A status from the server, whatever it is, is a response, not an error.",
			request: func() *http.Request {
				req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://api.example.com/", nil)
				return req
			},
			response: `{"status":503,"body":"YnVzeQ=="}`,
			want: want{
				request: hostRequest{Method: "GET", URL: "https://api.example.com/"},
				got:     got{status: 503, headers: http.Header{}, body: "busy"},
			},
		},
		"Refused": {
			reason: "Status 0 with an error is the host's refusal, surfaced as an *HTTPError.",
			request: func() *http.Request {
				req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://evil.example.com/", nil)
				return req
			},
			response: `{"status":0,"error":"sandbox.egress: no rule admits host \"evil.example.com\""}`,
			want: want{
				request: hostRequest{Method: "GET", URL: "https://evil.example.com/"},
				err:     `wasmfn: sandbox.egress: no rule admits host "evil.example.com"`,
			},
		},
		"Garbage": {
			reason: "A host answer that is not JSON is an error naming the decoder.",
			request: func() *http.Request {
				req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "https://api.example.com/", nil)
				return req
			},
			response: `nope`,
			want: want{
				request: hostRequest{Method: "GET", URL: "https://api.example.com/"},
				err:     "wasmfn: cannot decode the host's response",
			},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			var sent hostRequest
			httpCall = func(payload []byte) ([]byte, error) {
				if err := json.Unmarshal(payload, &sent); err != nil {
					t.Fatalf("payload is not JSON: %v", err)
				}
				return []byte(tc.response), nil
			}
			t.Cleanup(func() { httpCall = hostHTTPCall })

			rsp, err := HTTPClient().Do(tc.request())

			if diff := cmp.Diff(tc.want.request, sent); diff != "" {
				t.Errorf("\n%s\nrequest handed to the host: -want, +got:\n%s", tc.reason, diff)
			}
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nDo(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
				var herr *HTTPError
				if strings.HasPrefix(tc.want.err, "wasmfn: sandbox") && !errors.As(err, &herr) {
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
