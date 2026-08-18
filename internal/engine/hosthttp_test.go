package engine

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"

	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"

	"github.com/jonasz-lasut/function-wasm/input/v1beta1"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// httpImport declares the wasmfn.http import and a JSON request in guest
// memory (below the response and heap offsets testwasm uses), and returns a
// wasmfn_run body that calls the import calls times and returns the last
// answer as the message of a Result — RunFunctionResponse field 3 wrapping
// Result field 2, lengths as two-byte varints — so a test reads the bytes
// the host wrote into guest memory.
func httpImport(t *testing.T, req egress.Request, calls int) (extra, body string) {
	t.Helper()
	raw, err := json.Marshal(req)
	if err != nil {
		t.Fatal(err)
	}
	var lit strings.Builder
	for _, c := range raw {
		fmt.Fprintf(&lit, "\\%02x", c)
	}
	extra = `(import "wasmfn" "http" (func $http (param i32 i32) (result i64)))
  (data (i32.const 16) "` + lit.String() + `")`
	size := strconv.Itoa(len(raw))
	var b strings.Builder
	b.WriteString("(local $r i64) (local $rp i32) (local $rl i32) (local $out i32) (local $n i32)\n")
	for range calls - 1 {
		b.WriteString("    (drop (call $http (i32.const 16) (i32.const " + size + ")))\n")
	}
	b.WriteString(`    (local.set $r (call $http (i32.const 16) (i32.const ` + size + `)))
    (local.set $rp (i32.wrap_i64 (i64.shr_u (local.get $r) (i64.const 32))))
    (local.set $rl (i32.wrap_i64 (local.get $r)))
    (local.set $out (call $wasmfn_alloc (i32.add (local.get $rl) (i32.const 6))))
    (i32.store8 (local.get $out) (i32.const 0x1a))
    (local.set $n (i32.add (local.get $rl) (i32.const 3)))
    (i32.store8 offset=1 (local.get $out) (i32.or (i32.and (local.get $n) (i32.const 0x7f)) (i32.const 0x80)))
    (i32.store8 offset=2 (local.get $out) (i32.shr_u (local.get $n) (i32.const 7)))
    (i32.store8 offset=3 (local.get $out) (i32.const 0x12))
    (i32.store8 offset=4 (local.get $out) (i32.or (i32.and (local.get $rl) (i32.const 0x7f)) (i32.const 0x80)))
    (i32.store8 offset=5 (local.get $out) (i32.shr_u (local.get $rl) (i32.const 7)))
    (memory.copy (i32.add (local.get $out) (i32.const 6)) (local.get $rp) (local.get $rl))
    (i64.or (i64.shl (i64.extend_i32_u (local.get $out)) (i64.const 32)) (i64.extend_i32_u (i32.add (local.get $rl) (i32.const 6))))`)
	return extra, b.String()
}

// seen records what the test server received.
type seen struct {
	mu       sync.Mutex
	requests []string // "METHOD /path[?query]" plus notable headers
}

func (s *seen) add(r *http.Request) {
	s.mu.Lock()
	defer s.mu.Unlock()
	line := r.Method + " " + r.URL.RequestURI()
	if v := r.Header.Get("X-Test"); v != "" {
		line += " x-test=" + v
	}
	if r.Host != "" {
		line += " host=" + r.Host
	}
	s.requests = append(s.requests, line)
}

func (s *seen) reset() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.requests = nil
}

func (s *seen) list() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]string(nil), s.requests...)
}

func TestRunHTTP(t *testing.T) {
	var got seen
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		got.add(r)
		switch r.URL.Path {
		case "/echo":
			body, _ := io.ReadAll(r.Body)
			w.Header().Set("Content-Type", "text/plain")
			w.Header().Set("X-Echo", r.Header.Get("X-Test"))
			fmt.Fprintf(w, "%s %s", r.Method, body)
		case "/big":
			_, _ = w.Write(make([]byte, 100))
		case "/slow":
			time.Sleep(300 * time.Millisecond)
			_, _ = w.Write([]byte("late"))
		case "/redirect":
			http.Redirect(w, r, "/echo", http.StatusFound)
		case "/redirect-out":
			http.Redirect(w, r, "http://127.0.0.2:1/", http.StatusFound)
		case "/redirect-loop":
			http.Redirect(w, r, "/redirect-loop", http.StatusFound)
		default:
			http.NotFound(w, r)
		}
	}))
	defer srv.Close()
	host := strings.TrimPrefix(srv.URL, "http://") // 127.0.0.1:port
	port := host[strings.LastIndex(host, ":")+1:]

	// Loopback is on the default block list; the tests' ceiling admits it.
	loopback := egress.Policy{AllowedCIDRs: []string{"127.0.0.0/8", "::1/128"}}
	get := []string{"GET"}

	type args struct {
		cfg    Config
		policy *egress.Policy // nil: no egress at all (RunOptions.HTTP nil)
		rules  []v1beta1.SandboxHTTPRule
		req    egress.Request
		calls  int
	}
	type want struct {
		rsp egress.Response
		// errIs is a run error; with rsp set too, either satisfies the case.
		errIs   error
		echo    string
		seen    []string
		outcome string
	}
	cases := map[string]struct {
		reason string
		skip   func() string
		args   args
		want   want
	}{
		"Allowed": {
			reason: "A request within the grant is performed; status, headers and body come back, the server saw method, path, query and headers.",
			args: args{
				policy: &loopback,
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: []string{"POST"}, PathPrefix: "/echo"}},
				req:    egress.Request{Method: "POST", URL: srv.URL + "/echo?token=s3cret-query", Headers: map[string][]string{"X-Test": {"s3cret-header"}, "Host": {"evil"}}, Body: []byte("s3cret-body")},
			},
			want: want{
				rsp:     egress.Response{Status: 200, Body: []byte("POST s3cret-body")},
				echo:    "s3cret-header",
				seen:    []string{"POST /echo?token=s3cret-query x-test=s3cret-header host=" + host},
				outcome: metrics.OutcomeOK,
			},
		},
		"HostPattern": {
			reason: "A hostPattern rule admits names under it; the host resolves the name itself and dials the checked address.",
			skip: func() string {
				if _, err := net.DefaultResolver.LookupNetIP(context.Background(), "ip", "api.localhost"); err != nil {
					return "api.localhost does not resolve here: " + err.Error()
				}
				return ""
			},
			args: args{
				policy: &loopback,
				rules:  []v1beta1.SandboxHTTPRule{{HostPattern: "*.localhost", Methods: get}},
				req:    egress.Request{URL: "http://api.localhost:" + port + "/echo"},
			},
			want: want{
				rsp:     egress.Response{Status: 200, Body: []byte("GET ")},
				seen:    []string{"GET /echo host=api.localhost:" + port},
				outcome: metrics.OutcomeOK,
			},
		},
		"BlockedAddress": {
			reason: "A host that resolves to a blocked address is refused by the dialer, whatever the grant says.",
			args: args{
				policy: &egress.Policy{},
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{URL: srv.URL + "/echo"},
			},
			want: want{
				rsp:     egress.Response{Error: "sandbox.egress: 127.0.0.1 resolves to an address the egress policy blocks"},
				outcome: metrics.OutcomeRefused,
			},
		},
		"HostOutsideGrant": {
			reason: "A host no rule names is refused before any I/O.",
			args: args{
				policy: &loopback,
				rules:  []v1beta1.SandboxHTTPRule{{Host: "api.example.com", Methods: get}},
				req:    egress.Request{URL: srv.URL + "/echo"},
			},
			want: want{
				rsp:     egress.Response{Error: `sandbox.egress: no rule admits host "127.0.0.1"`},
				outcome: metrics.OutcomeRefused,
			},
		},
		"MethodNotAllowed": {
			reason: "A method the host's rules do not list is refused.",
			args: args{
				policy: &loopback,
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{Method: "DELETE", URL: srv.URL + "/echo"},
			},
			want: want{
				rsp:     egress.Response{Error: `sandbox.egress: no rule for host "127.0.0.1" admits DELETE /echo`},
				outcome: metrics.OutcomeRefused,
			},
		},
		"PathOutsidePrefix": {
			reason: "A path outside the rule's prefix is refused, and dot segments cannot sneak under it.",
			args: args{
				policy: &loopback,
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get, PathPrefix: "/echo"}},
				req:    egress.Request{URL: srv.URL + "/echo/../big"},
			},
			want: want{
				rsp:     egress.Response{Error: `sandbox.egress: the URL path "/echo/../big" must be normalized (no . or .. segments, no empty segments)`},
				outcome: metrics.OutcomeRefused,
			},
		},
		"SchemeRefused": {
			reason: "Only http and https are performed.",
			args: args{
				policy: &loopback,
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{URL: "ftp://127.0.0.1/x"},
			},
			want: want{
				rsp:     egress.Response{Error: `sandbox.egress: only http and https URLs are allowed, not "ftp"`},
				outcome: metrics.OutcomeRefused,
			},
		},
		"MaxRequests": {
			reason: "The second request of a run whose budget is one is refused.",
			args: args{
				policy: &egress.Policy{AllowedCIDRs: loopback.AllowedCIDRs, MaxRequests: 1},
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{URL: srv.URL + "/echo"},
				calls:  2,
			},
			want: want{
				rsp:     egress.Response{Error: "sandbox.egress: this run already made 1 requests (maxRequests)"},
				seen:    []string{"GET /echo host=" + host},
				outcome: metrics.OutcomeBudget,
			},
		},
		"MaxResponseBytes": {
			reason: "A body over the budget is an error, not a truncated body.",
			args: args{
				policy: &egress.Policy{AllowedCIDRs: loopback.AllowedCIDRs, MaxResponseBytes: 64},
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{URL: srv.URL + "/big"},
			},
			want: want{
				rsp:     egress.Response{Error: "sandbox.egress: the response body exceeds 64 bytes (maxResponseBytes)"},
				seen:    []string{"GET /big host=" + host},
				outcome: metrics.OutcomeBudget,
			},
		},
		"Timeout": {
			reason: "A request slower than the policy's timeout is an error naming it.",
			args: args{
				policy: &egress.Policy{AllowedCIDRs: loopback.AllowedCIDRs, Timeout: metav1.Duration{Duration: 50 * time.Millisecond}},
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{URL: srv.URL + "/slow"},
			},
			want: want{
				rsp:     egress.Response{Error: "sandbox.egress: the request exceeded its 50ms timeout"},
				seen:    []string{"GET /slow host=" + host},
				outcome: metrics.OutcomeBudget,
			},
		},
		"RunDeadline": {
			reason: "A request never outlives its run: the run's deadline cuts it short and the run is then interrupted as usual.",
			args: args{
				cfg:    Config{Timeout: 100 * time.Millisecond},
				policy: &loopback,
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{URL: srv.URL + "/slow"},
			},
			want: want{
				errIs:   ErrTimeout,
				rsp:     egress.Response{Error: "sandbox.egress: the request exceeded the run's remaining deadline"},
				seen:    []string{"GET /slow host=" + host},
				outcome: metrics.OutcomeBudget,
			},
		},
		"RedirectFollowed": {
			reason: "A redirect within the grant is followed and the final response returned.",
			args: args{
				policy: &loopback,
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{URL: srv.URL + "/redirect"},
			},
			want: want{
				rsp:     egress.Response{Status: 200, Body: []byte("GET ")},
				seen:    []string{"GET /redirect host=" + host, "GET /echo host=" + host},
				outcome: metrics.OutcomeOK,
			},
		},
		"RedirectOutsideGrant": {
			reason: "A redirect to a host outside the grant is refused at the hop.",
			args: args{
				policy: &loopback,
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{URL: srv.URL + "/redirect-out"},
			},
			want: want{
				rsp:     egress.Response{Error: `redirect to http://127.0.0.2:1/ refused: sandbox.egress: no rule admits host "127.0.0.2"`},
				seen:    []string{"GET /redirect-out host=" + host},
				outcome: metrics.OutcomeRefused,
			},
		},
		"TooManyRedirects": {
			reason: "Redirects stop at the budget.",
			args: args{
				policy: &egress.Policy{AllowedCIDRs: loopback.AllowedCIDRs, MaxRedirects: 2},
				rules:  []v1beta1.SandboxHTTPRule{{Host: "127.0.0.1", Methods: get}},
				req:    egress.Request{URL: srv.URL + "/redirect-loop"},
			},
			want: want{
				rsp:     egress.Response{Error: "stopped after 2 redirects (maxRedirects)"},
				seen:    []string{"GET /redirect-loop host=" + host, "GET /redirect-loop host=" + host, "GET /redirect-loop host=" + host},
				outcome: metrics.OutcomeBudget,
			},
		},
		"NoGrant": {
			reason: "A module without an egress grant gets a refusal from the import, never a trap.",
			args: args{
				req: egress.Request{URL: srv.URL + "/echo"},
			},
			want: want{
				rsp:     egress.Response{Error: noEgress},
				outcome: metrics.OutcomeRefused,
			},
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			if tc.skip != nil {
				if why := tc.skip(); why != "" {
					t.Skip(why)
				}
			}
			got.reset()
			before, _ := metrics.Sample("function_wasm_module_http_requests_total", map[string]string{"outcome": tc.want.outcome})

			e, err := New(tc.args.cfg)
			if err != nil {
				t.Fatalf("New(): %v", err)
			}
			defer e.Close()
			calls := max(tc.args.calls, 1)
			extra, body := httpImport(t, tc.args.req, calls)
			m, err := e.Compile(testwasm.Fixed(t, &fnv1.RunFunctionResponse{}, testwasm.Options{Extra: extra, Body: body}))
			if err != nil {
				t.Fatalf("Compile(): %v", err)
			}
			log := &recorder{}
			var opts RunOptions
			if tc.args.policy != nil {
				ceiling, err := egress.New(*tc.args.policy)
				if err != nil {
					t.Fatalf("egress.New(): %v", err)
				}
				grant, err := ceiling.Grant(tc.args.rules)
				if err != nil {
					t.Fatalf("Grant(): %v", err)
				}
				opts.HTTP = grant.Client(log, "sha256:test")
			}

			rsp, err := e.Run(context.Background(), m, request(), log, opts)

			switch {
			case err != nil && tc.want.errIs != nil:
				if !errors.Is(err, tc.want.errIs) {
					t.Fatalf("\n%s\nRun(): want error %v, got %v", tc.reason, tc.want.errIs, err)
				}
			case err != nil:
				t.Fatalf("\n%s\nRun(): unexpected error %v", tc.reason, err)
			default:
				if len(rsp.GetResults()) != 1 {
					t.Fatalf("\n%s\nRun(): want one result carrying the host's answer, got %v", tc.reason, rsp)
				}
				var answer egress.Response
				if err := json.Unmarshal([]byte(rsp.GetResults()[0].GetMessage()), &answer); err != nil {
					t.Fatalf("\n%s\nRun(): the guest returned %q, not the host's JSON answer: %v", tc.reason, rsp.GetResults()[0].GetMessage(), err)
				}
				// Headers are the server's (Date, Content-Length, ...): only
				// the one the handler echoes is asserted.
				if tc.want.echo != "" {
					if diff := cmp.Diff([]string{tc.want.echo}, answer.Headers["X-Echo"]); diff != "" {
						t.Errorf("\n%s\nRun(): response headers: -want, +got:\n%s", tc.reason, diff)
					}
				}
				answer.Headers = nil
				if diff := cmp.Diff(tc.want.rsp, answer); diff != "" {
					t.Errorf("\n%s\nRun(): host answer -want, +got:\n%s", tc.reason, diff)
				}
			}
			if diff := cmp.Diff(tc.want.seen, got.list(), cmpopts.EquateEmpty()); diff != "" {
				t.Errorf("\n%s\nserver saw: -want, +got:\n%s", tc.reason, diff)
			}
			after, _ := metrics.Sample("function_wasm_module_http_requests_total", map[string]string{"outcome": tc.want.outcome})
			if after != before+1 {
				t.Errorf("\n%s\nhttp_requests_total{outcome=%q}: want %v, got %v", tc.reason, tc.want.outcome, before+1, after)
			}
			var audit []record
			for _, r := range log.records {
				if r.Msg == "Module HTTP request" {
					audit = append(audit, r)
				}
			}
			if len(audit) != calls {
				t.Errorf("\n%s\nwant %d audit lines, got %v", tc.reason, calls, audit)
			}
			for _, r := range audit {
				kv := fmt.Sprint(r.KV...)
				if strings.Contains(kv, "s3cret") {
					t.Errorf("\n%s\naudit line must not carry headers, query or body, got %v", tc.reason, r.KV)
				}
				if !strings.Contains(kv, "outcome") || !strings.Contains(kv, "method") {
					t.Errorf("\n%s\naudit line must name method and outcome, got %v", tc.reason, r.KV)
				}
			}
		})
	}
}

// TestCompileHTTPImport pins the type check of the wasmfn.http import: a
// module importing it with another signature fails at load, and one that
// imports it correctly loads on a runtime without egress (the import is
// always provided; the grant decides at run time).
func TestCompileHTTPImport(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	cases := map[string]struct {
		reason string
		extra  string
		want   string
	}{
		"Right": {reason: "The ABI's type loads.", extra: `(import "wasmfn" "http" (func $http (param i32 i32) (result i64)))`},
		"Wrong": {
			reason: "Another type fails once, at load, with one line.",
			extra:  `(import "wasmfn" "http" (func $http (param i32) (result i32)))`,
			want:   "module imports wasmfn.http with the wrong type, ABI v1 requires (i32, i32) -> (i64)",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := e.Compile(testwasm.Fixed(t, cannedResponse(), testwasm.Options{Extra: tc.extra}))
			if tc.want == "" {
				if err != nil {
					t.Fatalf("\n%s\nCompile(): unexpected error %v", tc.reason, err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("\n%s\nCompile(): want error containing %q, got %v", tc.reason, tc.want, err)
			}
		})
	}
}
