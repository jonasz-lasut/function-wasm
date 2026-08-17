package engine

import (
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/google/go-cmp/cmp/cmpopts"

	"github.com/jonasz-lasut/function-wasm/internal/testwasm"
)

// TestInspect pins what Inspect reports — wasmtime's own view of a module's
// exports, imports and memories, and checkABI's verdict — for the fixture
// modules the runtime's tests use.
func TestInspect(t *testing.T) {
	type want struct {
		exports  []Extern
		imports  []Extern
		memories []MemoryLimits
		host     []string
		abiErr   string
		err      string
	}
	cases := map[string]struct {
		reason string
		wasm   func(t *testing.T) []byte
		want   want
	}{
		"Fixture": {
			reason: "The ABI v1 fixture: a memory, the allocator and wasmfn_run, no imports, no verdict.",
			wasm:   func(t *testing.T) []byte { return testwasm.Fixed(t, cannedResponse(), testwasm.Options{}) },
			want: want{
				exports: []Extern{
					{Name: "memory", Kind: "memory"},
					{Name: "wasmfn_alloc", Kind: "func", Type: "(i32) -> (i32)"},
					{Name: "wasmfn_run", Kind: "func", Type: "(i32, i32) -> (i64)"},
				},
				memories: []MemoryLimits{{Min: 2}},
			},
		},
		"ImportsAndInitialize": {
			reason: "Host imports are listed with their types; HostImports names them without WASI.",
			wasm: func(t *testing.T) []byte {
				return testwasm.Fixed(t, cannedResponse(), testwasm.Options{
					Extra:      `(import "wasmfn" "log" (func $log (param i32 i32 i32))) (import "wasmfn" "http" (func $http (param i32 i32) (result i64))) (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))`,
					Initialize: "nop",
				})
			},
			want: want{
				exports: []Extern{
					{Name: "memory", Kind: "memory"},
					{Name: "_initialize", Kind: "func", Type: "() -> ()"},
					{Name: "wasmfn_alloc", Kind: "func", Type: "(i32) -> (i32)"},
					{Name: "wasmfn_run", Kind: "func", Type: "(i32, i32) -> (i64)"},
				},
				imports: []Extern{
					{Module: "wasmfn", Name: "log", Kind: "func", Type: "(i32, i32, i32) -> ()"},
					{Module: "wasmfn", Name: "http", Kind: "func", Type: "(i32, i32) -> (i64)"},
					{Module: "wasi_snapshot_preview1", Name: "proc_exit", Kind: "func", Type: "(i32) -> ()"},
				},
				memories: []MemoryLimits{{Min: 2}},
				host:     []string{"wasmfn.log", "wasmfn.http"},
			},
		},
		"MissingRun": {
			reason: "A module wasmtime compiles but that lacks the ABI is described, with the load-time refusal as its verdict.",
			wasm:   func(t *testing.T) []byte { return testwasm.Fixed(t, cannedResponse(), testwasm.Options{SkipRun: true}) },
			want: want{
				exports:  []Extern{{Name: "memory", Kind: "memory"}, {Name: "wasmfn_alloc", Kind: "func", Type: "(i32) -> (i32)"}},
				memories: []MemoryLimits{{Min: 2}},
				abiErr:   `module does not export "wasmfn_run"`,
			},
		},
		"UnknownImport": {
			reason: "So is one importing what the host does not provide.",
			wasm: func(t *testing.T) []byte {
				return testwasm.Fixed(t, cannedResponse(), testwasm.Options{Extra: `(import "env" "magic" (func $magic))`})
			},
			want: want{
				exports: []Extern{
					{Name: "memory", Kind: "memory"},
					{Name: "wasmfn_alloc", Kind: "func", Type: "(i32) -> (i32)"},
					{Name: "wasmfn_run", Kind: "func", Type: "(i32, i32) -> (i64)"},
				},
				imports:  []Extern{{Module: "env", Name: "magic", Kind: "func", Type: "() -> ()"}},
				memories: []MemoryLimits{{Min: 2}},
				host:     []string{"env.magic"},
				abiErr:   "module imports env.magic, which the host does not provide",
			},
		},
		"BoundedMemory": {
			reason: "Memory limits carry the maximum when there is one; an imported memory is listed too.",
			wasm: func(t *testing.T) []byte {
				return testwasm.Build(t, `(module (import "env" "mem" (memory 3)) (memory (export "memory") 1 4))`)
			},
			want: want{
				exports:  []Extern{{Name: "memory", Kind: "memory"}},
				imports:  []Extern{{Module: "env", Name: "mem", Kind: "memory"}},
				memories: []MemoryLimits{{Min: 3}, {Min: 1, Max: new(uint64(4))}},
				host:     []string{"env.mem"},
				abiErr:   `module does not export "wasmfn_alloc"`,
			},
		},
		"NotWasm": {
			reason: "Bytes wasmtime cannot compile are an error, as at load.",
			wasm:   func(*testing.T) []byte { return []byte("not wasm") },
			want:   want{err: "cannot compile module"},
		},
	}
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			s, err := e.Inspect(tc.wasm(t))
			if tc.want.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.want.err) {
					t.Fatalf("\n%s\nInspect(): want error containing %q, got %v", tc.reason, tc.want.err, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("\n%s\nInspect(): unexpected error %v", tc.reason, err)
			}
			got := want{exports: s.Exports, imports: s.Imports, memories: s.Memories, host: s.HostImports()}
			if s.ABIError != nil {
				got.abiErr = s.ABIError.Error()
			}
			if diff := cmp.Diff(tc.want, got, cmp.AllowUnexported(want{}), cmpopts.EquateEmpty()); diff != "" {
				t.Errorf("\n%s\nInspect(): -want, +got:\n%s", tc.reason, diff)
			}
		})
	}
}

// TestModuleShape pins that a compiled module describes itself without a
// second compile and never carries an ABI verdict — Compile refused it
// otherwise.
func TestModuleShape(t *testing.T) {
	e, err := New(Config{})
	if err != nil {
		t.Fatal(err)
	}
	defer e.Close()
	m, err := e.Compile(testwasm.Fixed(t, cannedResponse(), testwasm.Options{Extra: `(import "wasmfn" "log" (func $log (param i32 i32 i32)))`}))
	if err != nil {
		t.Fatal(err)
	}
	defer m.Release()
	s := m.Shape()
	if s.ABIError != nil {
		t.Errorf("a compiled module has no ABI verdict, got %v", s.ABIError)
	}
	if diff := cmp.Diff([]string{"wasmfn.log"}, s.HostImports()); diff != "" {
		t.Errorf("HostImports(): -want, +got:\n%s", diff)
	}
	if got := s.Imports[0].String(); got != "wasmfn.log (i32, i32, i32) -> ()" {
		t.Errorf("Extern.String() = %q", got)
	}
	if got := s.Exports[0].String(); got != "memory (memory)" {
		t.Errorf("Extern.String() = %q", got)
	}
}
