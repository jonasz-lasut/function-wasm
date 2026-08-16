package engine

import (
	"context"
	"errors"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/go-cmp/cmp"
	"google.golang.org/protobuf/testing/protocmp"
	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/structpb"

	xplogging "github.com/crossplane/crossplane-runtime/v2/pkg/logging"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"github.com/crossplane/function-sdk-go/response"

	"github.com/jonasz-lasut/function-wasm/internal/metrics"
	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// recorder captures guest log records.
type recorder struct {
	mu      sync.Mutex
	records []record
}

type record struct {
	Level string
	Msg   string
	KV    []any
}

func (r *recorder) Info(msg string, kv ...any)  { r.add("info", msg, kv) }
func (r *recorder) Debug(msg string, kv ...any) { r.add("debug", msg, kv) }
func (r *recorder) WithValues(...any) xplogging.Logger {
	return r
}

func (r *recorder) add(level, msg string, kv []any) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.records = append(r.records, record{Level: level, Msg: msg, KV: kv})
}

var _ logging.Logger = &recorder{}

func request() *fnv1.RunFunctionRequest {
	return &fnv1.RunFunctionRequest{Meta: &fnv1.RequestMeta{Tag: "test"}}
}

func cannedResponse() *fnv1.RunFunctionResponse {
	rsp := &fnv1.RunFunctionResponse{
		Meta: &fnv1.ResponseMeta{Tag: "test", Ttl: durationpb.New(60 * time.Second)},
		Desired: &fnv1.State{Resources: map[string]*fnv1.Resource{
			"cm": {Resource: mustStruct(map[string]any{"apiVersion": "v1", "kind": "ConfigMap"})},
		}},
	}
	response.Normal(rsp, "hello from wat")
	return rsp
}

func mustStruct(m map[string]any) *structpb.Struct {
	s, err := structpb.NewStruct(m)
	if err != nil {
		panic(err)
	}
	return s
}

// logImport declares the wasmfn.log import and a JSON record in guest memory
// (well below the response and heap offsets used by testwasm), and calls it.
// payload is WAT-escaped, so its unescaped length is what the guest passes.
func logImport(level int, payload string) (extra, body string) {
	extra = `(import "wasmfn" "log" (func $log (param i32 i32 i32)))
  (data (i32.const 16) "` + payload + `")`
	size := len(strings.ReplaceAll(payload, `\"`, `"`))
	body = "(call $log (i32.const " + itoa(level) + ") (i32.const 16) (i32.const " + itoa(size) + "))\n    "
	return extra, body
}

func itoa(i int) string {
	return strconv.Itoa(i)
}

func TestCompile(t *testing.T) {
	type want struct {
		err string
	}
	cases := map[string]struct {
		reason string
		wasm   func(t *testing.T) []byte
		want   want
	}{
		"Valid": {
			reason: "A module with the ABI v1 exports compiles.",
			wasm:   func(t *testing.T) []byte { return testwasm.Fixed(t, cannedResponse(), testwasm.Options{}) },
		},
		"NotWasm": {
			reason: "Bytes that are not a wasm module are rejected.",
			wasm:   func(*testing.T) []byte { return []byte("not wasm") },
			want:   want{err: "cannot compile module"},
		},
		"MissingRun": {
			reason: "A module without wasmfn_run is rejected at load time.",
			wasm:   func(t *testing.T) []byte { return testwasm.Fixed(t, cannedResponse(), testwasm.Options{SkipRun: true}) },
			want:   want{err: `does not export "wasmfn_run"`},
		},
		"BadRunSignature": {
			reason: "wasmfn_run must be (i32, i32) -> i64.",
			wasm: func(t *testing.T) []byte {
				return testwasm.Fixed(t, cannedResponse(), testwasm.Options{RunSignature: "(param i32) (result i32)", Body: "(i32.const 0)"})
			},
			want: want{err: `export "wasmfn_run" has signature (i32) -> (i32), ABI v1 requires (i32, i32) -> (i64)`},
		},
		"UnknownImport": {
			reason: "Imports the host cannot satisfy fail at load, not at run.",
			wasm: func(t *testing.T) []byte {
				return testwasm.Fixed(t, cannedResponse(), testwasm.Options{Extra: `(import "env" "magic" (func $magic))`})
			},
			want: want{err: "module imports env.magic, which the host does not provide"},
		},
	}

	e, err := New(Config{})
	if err != nil {
		t.Fatalf("New(): %v", err)
	}
	defer e.Close()

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := e.Compile(tc.wasm(t))
			if tc.want.err == "" {
				if err != nil {
					t.Fatalf("\n%s\nCompile(): unexpected error: %v", tc.reason, err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), tc.want.err) {
				t.Fatalf("\n%s\nCompile(): want error containing %q, got %v", tc.reason, tc.want.err, err)
			}
		})
	}
}

func TestRun(t *testing.T) {
	type args struct {
		cfg  Config
		ctx  func() (context.Context, context.CancelFunc)
		opts testwasm.Options
	}
	type want struct {
		rsp     *fnv1.RunFunctionResponse
		err     string
		errIs   error
		records []record
		// trapLogged expects the full wasmtime trap (with backtrace) at debug level.
		trapLogged bool
	}
	logExtra, logBody := logImport(0, `{\"msg\":\"hello\",\"kv\":[\"k\",\"v\"]}`)
	debugExtra, debugBody := logImport(1, `{\"msg\":\"dbg\"}`)
	badExtra, badBody := logImport(0, `not json`)

	cases := map[string]struct {
		reason string
		args   args
		want   want
	}{
		"RoundTrip": {
			reason: "The guest's response comes back verbatim.",
			args:   args{opts: testwasm.Options{}},
			want:   want{rsp: cannedResponse()},
		},
		"EmptyResponse": {
			reason: "A zero packed pointer decodes as an empty response.",
			args:   args{opts: testwasm.Options{Body: "(i64.const 0)"}},
			want:   want{rsp: &fnv1.RunFunctionResponse{}},
		},
		"InitializeRunsFirst": {
			reason: "_initialize is called before wasmfn_run, so a failing one is reported as such.",
			args:   args{opts: testwasm.Options{Initialize: "unreachable"}},
			want:   want{err: "_initialize failed: trap", trapLogged: true},
		},
		"Trap": {
			reason: "A trap inside wasmfn_run is reported with wasmtime's message.",
			args:   args{opts: testwasm.Options{Body: "unreachable"}},
			want:   want{err: "wasmfn_run failed: trap: unreachable code reached", trapLogged: true},
		},
		"EngineTimeout": {
			reason: "A guest that never returns is interrupted at the engine timeout.",
			args:   args{cfg: Config{Timeout: 50 * time.Millisecond}, opts: testwasm.Options{Body: "(loop $l (br $l)) (i64.const 0)"}},
			want:   want{errIs: ErrTimeout},
		},
		"ContextDeadline": {
			reason: "A request deadline shorter than the engine timeout wins.",
			args: args{
				ctx: func() (context.Context, context.CancelFunc) {
					return context.WithTimeout(context.Background(), 30*time.Millisecond)
				},
				opts: testwasm.Options{Body: "(loop $l (br $l)) (i64.const 0)"},
			},
			want: want{errIs: ErrTimeout},
		},
		"MemoryLimit": {
			reason: "Growing beyond the memory limit fails inside the guest, which then traps.",
			args: args{
				cfg:  Config{MemoryLimit: 3 * 65536},
				opts: testwasm.Options{Body: "(if (i32.eq (memory.grow (i32.const 64)) (i32.const -1)) (then unreachable)) (i64.const 0)"},
			},
			want: want{err: "wasmfn_run failed: trap", trapLogged: true},
		},
		"MemoryWithinLimit": {
			reason: "The same growth succeeds under the default limit.",
			args:   args{opts: testwasm.Options{Body: "(if (i32.eq (memory.grow (i32.const 64)) (i32.const -1)) (then unreachable)) (i64.const 0)"}},
			want:   want{rsp: &fnv1.RunFunctionResponse{}},
		},
		"ResponseOutOfBounds": {
			reason: "A response pointer outside guest memory is rejected instead of read.",
			args:   args{opts: testwasm.Options{Body: "(i64.or (i64.shl (i64.const 0x7fffffff) (i64.const 32)) (i64.const 16))"}},
			want:   want{err: "wasmfn_run returned an invalid response buffer"},
		},
		"Exit": {
			reason: "A WASI exit reports the status instead of a generic trap.",
			args: args{opts: testwasm.Options{
				Extra: `(import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))`,
				Body:  "(call $exit (i32.const 3)) (i64.const 0)",
			}},
			want: want{err: "wasmfn_run failed: module exited with status 3"},
		},
		"HostLogInfo": {
			reason: "wasmfn.log records reach the logger with their key/values.",
			args:   args{opts: testwasm.Options{Extra: logExtra, Body: logBody + "(i64.const 0)"}},
			want:   want{rsp: &fnv1.RunFunctionResponse{}, records: []record{{Level: "info", Msg: "hello", KV: []any{"k", "v"}}}},
		},
		"HostLogDebug": {
			reason: "Level 1 records are logged at debug.",
			args:   args{opts: testwasm.Options{Extra: debugExtra, Body: debugBody + "(i64.const 0)"}},
			want:   want{rsp: &fnv1.RunFunctionResponse{}, records: []record{{Level: "debug", Msg: "dbg"}}},
		},
		"HostLogMalformed": {
			reason: "A record that is not JSON is logged raw rather than dropped.",
			args:   args{opts: testwasm.Options{Extra: badExtra, Body: badBody + "(i64.const 0)"}},
			want: want{rsp: &fnv1.RunFunctionResponse{}, records: []record{
				{Level: "info", Msg: "not json", KV: []any{"wasmfn-log-error", "invalid character 'o' in literal null (expecting 'u')"}},
			}},
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			e, err := New(tc.args.cfg)
			if err != nil {
				t.Fatalf("New(): %v", err)
			}
			defer e.Close()
			m, err := e.Compile(testwasm.Fixed(t, cannedResponse(), tc.args.opts))
			if err != nil {
				t.Fatalf("Compile(): %v", err)
			}
			ctx, cancel := context.Background(), context.CancelFunc(func() {})
			if tc.args.ctx != nil {
				ctx, cancel = tc.args.ctx()
			}
			defer cancel()
			log := &recorder{}

			got, err := e.Run(ctx, m, request(), log)

			switch {
			case tc.want.errIs != nil:
				if !errors.Is(err, tc.want.errIs) {
					t.Fatalf("\n%s\nRun(): want error %v, got %v", tc.reason, tc.want.errIs, err)
				}
			case tc.want.err != "":
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nRun(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
			default:
				if err != nil {
					t.Fatalf("\n%s\nRun(): unexpected error: %v", tc.reason, err)
				}
			}
			if diff := cmp.Diff(tc.want.rsp, got, protocmp.Transform()); diff != "" {
				t.Errorf("\n%s\nRun(): -want, +got:\n%s", tc.reason, diff)
			}
			var records []record
			trapLogged := false
			for _, r := range log.records {
				if r.Level == "debug" && r.Msg == "Guest trapped" && len(r.KV) == 2 && strings.Contains(r.KV[1].(string), "wasm backtrace") {
					trapLogged = true
					continue
				}
				records = append(records, r)
			}
			if diff := cmp.Diff(tc.want.records, records, cmp.AllowUnexported(record{})); diff != "" {
				t.Errorf("\n%s\nRun() logs: -want, +got:\n%s", tc.reason, diff)
			}
			if diff := cmp.Diff(tc.want.trapLogged, trapLogged); diff != "" {
				t.Errorf("\n%s\nRun() trap logged at debug: -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

// TestRunMetrics checks that runs are counted by outcome and compiles are
// timed; the values themselves are not asserted.
func TestRunMetrics(t *testing.T) {
	before := map[string]float64{}
	for _, o := range []string{metrics.OutcomeOK, metrics.OutcomeError, metrics.OutcomeTimeout} {
		before[o], _ = metrics.Sample("function_wasm_module_run_duration_seconds", map[string]string{"outcome": o})
	}
	compiles, _ := metrics.Sample("function_wasm_module_compile_duration_seconds", nil)

	e, err := New(Config{Timeout: 50 * time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	for _, opts := range []testwasm.Options{{}, {Body: "unreachable"}, {Body: "(loop $l (br $l)) (i64.const 0)"}} {
		m, err := e.Compile(testwasm.Fixed(t, cannedResponse(), opts))
		if err != nil {
			t.Fatal(err)
		}
		_, _ = e.Run(context.Background(), m, request(), &recorder{})
	}

	for _, o := range []string{metrics.OutcomeOK, metrics.OutcomeError, metrics.OutcomeTimeout} {
		got, ok := metrics.Sample("function_wasm_module_run_duration_seconds", map[string]string{"outcome": o})
		if !ok || got != before[o]+1 {
			t.Errorf("run_duration_seconds{outcome=%q}: want %v, got %v (found %v)", o, before[o]+1, got, ok)
		}
	}
	if got, _ := metrics.Sample("function_wasm_module_compile_duration_seconds", nil); got != compiles+3 {
		t.Errorf("compile_duration_seconds count: want %v, got %v", compiles+3, got)
	}
}

func TestRunConcurrent(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatalf("New(): %v", err)
	}
	defer e.Close()
	m, err := e.Compile(testwasm.Fixed(t, cannedResponse(), testwasm.Options{}))
	if err != nil {
		t.Fatalf("Compile(): %v", err)
	}

	var wg sync.WaitGroup
	errs := make(chan error, 32)
	for range 32 {
		wg.Go(func() {
			got, err := e.Run(context.Background(), m, request(), &recorder{})
			if err != nil {
				errs <- err
				return
			}
			if diff := cmp.Diff(cannedResponse(), got, protocmp.Transform()); diff != "" {
				errs <- errors.New(diff)
			}
		})
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Errorf("concurrent Run(): %v", err)
	}
}
