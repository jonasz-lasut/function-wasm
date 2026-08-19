package main

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"

	xplogging "github.com/crossplane/crossplane-runtime/v2/pkg/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/resource"

	"github.com/jonasz-lasut/function-wasm/internal/egress"
	"github.com/jonasz-lasut/function-wasm/internal/engine"
	"github.com/jonasz-lasut/function-wasm/internal/module"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// recorder renders each log line as "msg k=v ..." including the values the
// host attached with WithValues, so a guest line proves it carried the host's
// context.
type recorder struct {
	mu   *sync.Mutex
	kv   []any
	seen *[]string
}

func newRecorder() *recorder {
	return &recorder{mu: &sync.Mutex{}, seen: new([]string)}
}

func (r *recorder) Info(msg string, kv ...any)  { r.add(msg, kv) }
func (r *recorder) Debug(msg string, kv ...any) { r.add(msg, kv) }
func (r *recorder) WithValues(kv ...any) xplogging.Logger {
	return &recorder{mu: r.mu, kv: slices.Concat(r.kv, kv), seen: r.seen}
}

func (r *recorder) add(msg string, kv []any) {
	all := slices.Concat(r.kv, kv)
	line := msg
	for i := 0; i+1 < len(all); i += 2 {
		line += fmt.Sprintf(" %v=%v", all[i], all[i+1])
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	*r.seen = append(*r.seen, line)
}

// TestRunFunctionGuests runs the example guests — the same greeting function
// written with function-sdk-go (Go), with TinyGo and vtprotobuf, and in Rust
// with prost — through the whole host: path source, compile, per-request
// instance, guest logging. Every guest must produce the same response.
func TestRunFunctionGuests(t *testing.T) {
	guests := map[string]func(t *testing.T) []byte{
		"go": func(t *testing.T) []byte {
			return testwasm.BuildGuest(t, filepath.Join("..", "..", "examples", "hello-go"))
		},
		"tinygo": func(t *testing.T) []byte {
			return testwasm.BuildTinyGoGuest(t, filepath.Join("..", "..", "examples", "hello-tinygo"))
		},
		"rust": func(t *testing.T) []byte {
			return testwasm.BuildRustGuest(t, filepath.Join("..", "..", "examples", "hello-rust"))
		},
	}
	for guest, build := range guests {
		t.Run(guest, func(t *testing.T) {
			runGuestCases(t, guest, build(t))
		})
	}
}

func runGuestCases(t *testing.T, guest string, wasm []byte) {
	t.Helper()
	dir := t.TempDir()
	file := guest + ".wasm"
	if err := os.WriteFile(filepath.Join(dir, file), wasm, 0o600); err != nil {
		t.Fatal(err)
	}

	eng, err := engine.New(engine.Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Close()
	// What guestfn inspect and validate --resolve report: every guest is
	// ABI v1 and imports both host functions.
	shape, err := eng.Inspect(wasm)
	if err != nil {
		t.Fatalf("Inspect(): %v", err)
	}
	if shape.ABIError != nil {
		t.Fatalf("Inspect(): %v", shape.ABIError)
	}
	if imports := shape.HostImports(); !slices.Contains(imports, "wasmfn.log") || !slices.Contains(imports, "wasmfn.http") {
		t.Fatalf("guest imports %v, want wasmfn.log and wasmfn.http", imports)
	}
	resolver, err := module.NewResolver(module.Options{Dir: dir})
	if err != nil {
		t.Fatal(err)
	}
	log := newRecorder()
	// A greeting server for the guests' greetingUrl, reachable through the
	// host's egress: the ceiling lifts the loopback ranges the block list
	// would otherwise refuse (every resolved address is judged).
	greetings := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/en" {
			http.NotFound(w, r)
			return
		}
		_, _ = w.Write([]byte("howdy\n"))
	}))
	defer greetings.Close()
	ceiling, err := egress.New(egress.WithAllowedCIDRs(loopbackCIDRs()))
	if err != nil {
		t.Fatal(err)
	}
	f := &Function{log: log, ttl: ttl, engine: eng, modules: engine.NewCache(eng, engine.CacheOptions{}), resolver: resolver, egress: ceiling, policy: permissiveSandboxPolicy(t)}
	greetingsHost, _, _ := net.SplitHostPort(strings.TrimPrefix(greetings.URL, "http://"))
	egressGrant := `"sandbox":{"egress":{"http":[{"host":"` + greetingsHost + `","methods":["GET"]}]}}`

	xr := resource.MustStructJSON(`{"apiVersion":"example.org/v1","kind":"XR","metadata":{"name":"my-xr"}}`)
	response := func(greeting string) *fnv1.RunFunctionResponse {
		return &fnv1.RunFunctionResponse{
			Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(60 * time.Second)},
			Desired: &fnv1.State{Resources: map[string]*fnv1.Resource{
				"greeting": {Resource: resource.MustStructJSON(`{"apiVersion":"v1","kind":"ConfigMap","data":{"greeting":"` + greeting + `"}}`)},
			}},
			Results:    []*fnv1.Result{{Severity: fnv1.Severity_SEVERITY_NORMAL, Message: "greeted my-xr", Target: fnv1.Target_TARGET_COMPOSITE.Enum()}},
			Conditions: []*fnv1.Condition{{Type: "FunctionSuccess", Status: fnv1.Status_STATUS_CONDITION_TRUE, Reason: "Success", Target: fnv1.Target_TARGET_COMPOSITE_AND_CLAIM.Enum()}},
		}
	}
	guestLog := "Running function tag=hello module=module file " + file + " digest=" + digestOf(wasm) + " tag=hello"

	type want struct {
		rsp  *fnv1.RunFunctionResponse
		logs []string
	}
	cases := map[string]struct {
		reason string
		input  string
		want   want
	}{
		"Default": {
			reason: "The guest composes a ConfigMap and its logs surface with the host's context.",
			input:  `{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{"type":"Path","path":"` + file + `"}}`,
			want:   want{rsp: response("hello my-xr"), logs: []string{"Running function tag=hello", guestLog}},
		},
		"Configured": {
			reason: "input.config reaches the guest.",
			input:  `{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{"type":"Path","path":"` + file + `"},"config":{"greeting":"hi"}}`,
			want:   want{rsp: response("hi my-xr"), logs: []string{"Running function tag=hello", guestLog}},
		},
		"GreetingFromURL": {
			reason: "The guest fetches its greeting through the host's wasmfn.http import within the Composition's egress grant — the same helper shape in Go (wasmfn.HTTPClient), TinyGo and Rust.",
			input:  `{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{"type":"Path","path":"` + file + `"},` + egressGrant + `,"config":{"greetingUrl":"` + greetings.URL + `/en"}}`,
			want:   want{rsp: response("howdy my-xr"), logs: []string{"Running function tag=hello", guestLog, "method=GET outcome=ok"}},
		},
		"GreetingURLWithoutGrant": {
			reason: "Without an egress grant the host refuses the import's call and the guest reports it as a fatal result — no trap, no crash.",
			input:  `{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{"type":"Path","path":"` + file + `"},"config":{"greetingUrl":"` + greetings.URL + `/en"}}`,
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(60 * time.Second)},
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_FATAL,
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}, logs: []string{"method=GET outcome=refused"}},
		},
		"BadConfig": {
			reason: "A config the guest cannot use is a fatal result from the guest, not a crash.",
			input:  `{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{"type":"Path","path":"` + file + `"},"config":{"greeting":7}}`,
			want: want{rsp: &fnv1.RunFunctionResponse{
				Meta: &fnv1.ResponseMeta{Tag: "hello", Ttl: durationpb.New(60 * time.Second)},
				Results: []*fnv1.Result{{
					Severity: fnv1.Severity_SEVERITY_FATAL,
					Target:   fnv1.Target_TARGET_COMPOSITE.Enum(),
				}},
			}},
		},
	}
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			*log.seen = nil
			req := &fnv1.RunFunctionRequest{
				Meta:     &fnv1.RequestMeta{Tag: "hello"},
				Input:    resource.MustStructJSON(tc.input),
				Observed: &fnv1.State{Composite: &fnv1.Resource{Resource: xr}},
			}
			rsp, err := f.RunFunction(context.Background(), req)
			if err != nil {
				t.Fatalf("\n%s\nRunFunction(): unexpected error %v", tc.reason, err)
			}
			// Fatal messages are worded per guest; only their presence is asserted.
			if tc.want.rsp.GetResults() != nil && tc.want.rsp.GetResults()[0].GetSeverity() == fnv1.Severity_SEVERITY_FATAL {
				for _, r := range rsp.GetResults() {
					if r.GetSeverity() == fnv1.Severity_SEVERITY_FATAL && r.GetMessage() != "" {
						r.Message = ""
					}
				}
			}
			if diff := cmp.Diff(tc.want.rsp, rsp, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRunFunction(): -want rsp, +got rsp:\n%s", tc.reason, diff)
			}
			for _, want := range tc.want.logs {
				if !slices.ContainsFunc(*log.seen, func(line string) bool { return strings.Contains(line, want) }) {
					t.Errorf("\n%s\nlogs: missing a line containing %q in %q", tc.reason, want, *log.seen)
				}
			}
		})
	}
}
