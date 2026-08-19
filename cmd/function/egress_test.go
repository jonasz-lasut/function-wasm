package main

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"

	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"

	"github.com/jonasz-lasut/function-wasm/internal/authz"
	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// loopbackCIDRs admit the loopback addresses httptest listens on, which the
// default block list refuses. In the runtime these prefixes come from an
// operator Cedar dialAddress permit; a test passes them straight to New.
func loopbackCIDRs() []netip.Prefix {
	return []netip.Prefix{netip.MustParsePrefix("127.0.0.0/8"), netip.MustParsePrefix("::1/128")}
}

func egressRules(rules ...map[string]any) map[string]any {
	list := make([]any, 0, len(rules))
	for _, r := range rules {
		list = append(list, r)
	}
	return map[string]any{"egress": map[string]any{"http": list}}
}

// TestRunFunctionEgress pins how a sandbox.egress grant is admitted before any
// module runs: refused with no --sandbox-policy-file to grant it, otherwise
// compiled and the module runs. The operator host allowlist is Cedar's
// grantEgress (see validate_test).
func TestRunFunctionEgress(t *testing.T) {
	moduleDir := t.TempDir()
	if err := os.WriteFile(filepath.Join(moduleDir, "fn.wasm"), testwasm.Fixed(t, guestResponse(), testwasm.Options{}), 0o600); err != nil {
		t.Fatal(err)
	}
	// The egress mechanism. The operator host allowlist is Cedar's (grantEgress,
	// at admission); the permissive policy admits any granted host, so this
	// exercises the grant-compile and run path, not host capping (that is the
	// OperatorPolicy* cases in validate_test).
	open, err := egress.New()
	if err != nil {
		t.Fatal(err)
	}

	type args struct {
		egress *egress.Egress
		policy *authz.OperatorPolicy
		req    *fnv1.RunFunctionRequest
	}
	type want struct {
		rsp *fnv1.RunFunctionResponse
	}
	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"NoPolicy": {
			reason: "With no --sandbox-policy-file to grant egress, a grant is a fatal result, before the module is resolved.",
			args: args{req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("missing.wasm"), "sandbox": egressRules(map[string]any{"host": "api.example.com", "methods": []any{"GET"}})}),
			}},
			want: want{rsp: fatal("sandbox.egress is refused: the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)")},
		},
		"Granted": {
			reason: "A policy grant lets the module run; a module that never calls the import is unaffected. Host capping is Cedar's grantEgress, exercised in validate's OperatorPolicy cases, not here.",
			args: args{egress: open, policy: permissiveSandboxPolicy(t), req: &fnv1.RunFunctionRequest{
				Meta:  &fnv1.RequestMeta{Tag: "hello"},
				Input: inputWith(t, map[string]any{"module": pathModule("fn.wasm"), "sandbox": egressRules(map[string]any{"host": "api.example.com", "methods": []any{"GET"}}, map[string]any{"hostPattern": "*.eu.internal.example.com", "methods": []any{"POST"}, "pathPrefix": "/v1/"})}),
			}},
			want: want{rsp: guestResponse()},
		},
	}

	eng, err := engine.New(engine.Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	resolver, err := module.NewResolver(module.Options{Dir: moduleDir})
	if err != nil {
		t.Fatal(err)
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			f := &Function{log: logging.NewNopLogger(), ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver, egress: tc.args.egress, policy: tc.args.policy}
			rsp, err := f.RunFunction(context.Background(), tc.args.req)
			if err != nil {
				t.Fatalf("\n%s\nRunFunction(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want.rsp, rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want rsp, +got rsp:\n%s", tc.reason, diff)
			}
		})
	}
}

// TestRunFunctionEgressGuest runs a real Go guest that calls
// wasmfn.HTTPClient through the whole host: the grant admits or refuses, the
// host performs the request and re-enters the guest's wasmfn_alloc for the
// answer, and the audit line carries the module's identity.
func TestRunFunctionEgressGuest(t *testing.T) {
	wasm := testwasm.BuildGuest(t, filepath.Join("..", "..", "internal", "testwasm", "testdata", "httpguest"))
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "httpguest.wasm"), wasm, 0o600); err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		fmt.Fprintf(w, "%s %s guest=%s body=%s", r.Method, r.URL.Path, r.Header.Get("X-Guest"), body)
	}))
	defer srv.Close()

	ceiling, err := egress.New(egress.WithAllowedCIDRs(loopbackCIDRs()))
	if err != nil {
		t.Fatal(err)
	}
	eng, err := engine.New(engine.Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	resolver, err := module.NewResolver(module.Options{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	log := newRecorder()
	f := &Function{log: log, ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver, egress: ceiling, policy: permissiveSandboxPolicy(t)}
	audit := "Module HTTP request tag=hello module=module file httpguest.wasm digest=" + digestOf(wasm) + " method="

	normal := func(msg string) *fnv1.RunFunctionResponse {
		return &fnv1.RunFunctionResponse{
			Meta:    &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(ttl)},
			Results: []*fnv1.Result{{Severity: fnv1.Severity_SEVERITY_NORMAL, Message: msg}},
		}
	}
	grant := egressRules(map[string]any{"host": "127.0.0.1", "methods": []any{"GET", "POST"}, "pathPrefix": "/api/"})

	cases := map[string]struct {
		reason string
		input  map[string]any
		want   *fnv1.RunFunctionResponse
		log    string
	}{
		"Get": {
			reason: "The guest's GET goes through the host and the server's answer comes back through wasmfn_alloc.",
			input:  map[string]any{"module": pathModule("httpguest.wasm"), "sandbox": grant, "config": map[string]any{"url": srv.URL + "/api/items"}},
			want:   normal("200 GET /api/items guest=httpguest body="),
			log:    audit + "GET outcome=ok",
		},
		"PostBody": {
			reason: "A request body travels to the server.",
			input:  map[string]any{"module": pathModule("httpguest.wasm"), "sandbox": grant, "config": map[string]any{"url": srv.URL + "/api/items", "method": "POST", "body": "hi"}},
			want:   normal("200 POST /api/items guest=httpguest body=hi"),
			log:    audit + "POST outcome=ok",
		},
		"MethodRefused": {
			reason: "A method outside the grant is a transport error in the guest, which it reports as a fatal result.",
			input:  map[string]any{"module": pathModule("httpguest.wasm"), "sandbox": grant, "config": map[string]any{"url": srv.URL + "/api/items", "method": "DELETE"}},
			want:   fatal(`Delete "` + srv.URL + `/api/items": wasmfn: sandbox.egress: no rule for host "127.0.0.1" admits DELETE /api/items`),
			log:    audit + "DELETE outcome=refused",
		},
		"PathRefused": {
			reason: "A path outside the grant's prefix is refused.",
			input:  map[string]any{"module": pathModule("httpguest.wasm"), "sandbox": grant, "config": map[string]any{"url": srv.URL + "/admin"}},
			want:   fatal(`Get "` + srv.URL + `/admin": wasmfn: sandbox.egress: no rule for host "127.0.0.1" admits GET /admin`),
			log:    audit + "GET outcome=refused",
		},
		"NoGrant": {
			reason: "Without a sandbox.egress grant the import refuses and the guest sees the reason.",
			input:  map[string]any{"module": pathModule("httpguest.wasm"), "config": map[string]any{"url": srv.URL + "/api/items"}},
			want:   fatal(`Get "` + srv.URL + `/api/items": wasmfn: sandbox.egress: HTTP egress is not granted to this module: the Composition's Input names no sandbox.egress.http rule`),
			log:    audit + "GET outcome=refused",
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			*log.seen = nil
			rsp, err := f.RunFunction(context.Background(), &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    inputWith(t, tc.input),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: resource.MustStructJSON(`{"apiVersion":"example.org/v1","kind":"XR","metadata":{"name":"my-xr"}}`)}},
			})
			if err != nil {
				t.Fatalf("\n%s\nRunFunction(): unexpected error %v", tc.reason, err)
			}
			if diff := cmp.Diff(tc.want, rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want rsp, +got rsp:\n%s", tc.reason, diff)
			}
			found := false
			for _, line := range *log.seen {
				if strings.HasPrefix(line, tc.log) {
					found = true
				}
			}
			if !found {
				t.Errorf("\n%s\nlogs: missing a line starting with %q in %q", tc.reason, tc.log, *log.seen)
			}
		})
	}
}
