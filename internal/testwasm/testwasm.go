// Package testwasm builds tiny hand-written guest modules for tests. They
// implement ABI v1 in WebAssembly text so the host can be exercised without a
// Go-to-wasm build, and each one misbehaves in exactly one way.
package testwasm

import (
	"fmt"
	"strings"
	"testing"

	"github.com/bytecodealliance/wasmtime-go/v47"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"google.golang.org/protobuf/proto"
)

// responseOffset is where the canned response bytes live in guest memory;
// the bump allocator starts above the first page so the two never overlap.
const (
	responseOffset = 1024
	heapStart      = 65536
)

// Options tweak the behaviour of the module Fixed builds.
type Options struct {
	// Body replaces the body of wasmfn_run; it must leave an i64 on the
	// stack. Empty returns the canned response.
	Body string
	// Initialize adds an _initialize export with this body.
	Initialize string
	// Extra is appended verbatim inside the module (imports, globals, funcs).
	Extra string
	// SkipRun omits the wasmfn_run export.
	SkipRun bool
	// RunSignature overrides the wasmfn_run signature, e.g. for a bad-ABI test.
	RunSignature string
}

// Fixed compiles a module whose wasmfn_run returns rsp regardless of the
// request, using a bump allocator that grows memory as needed.
func Fixed(t *testing.T, rsp *fnv1.RunFunctionResponse, o Options) []byte {
	t.Helper()
	raw, err := proto.Marshal(rsp)
	if err != nil {
		t.Fatalf("cannot marshal response: %v", err)
	}
	return Build(t, Fixture(raw, o))
}

// Fixture returns the WAT of a module that returns raw as its response.
func Fixture(raw []byte, o Options) string {
	body := o.Body
	if body == "" {
		body = fmt.Sprintf("(i64.or (i64.shl (i64.const %d) (i64.const 32)) (i64.const %d))", responseOffset, len(raw))
	}
	sig := o.RunSignature
	if sig == "" {
		sig = "(param $ptr i32) (param $len i32) (result i64)"
	}
	var b strings.Builder
	b.WriteString("(module\n")
	b.WriteString(o.Extra)
	b.WriteString("\n  (memory (export \"memory\") 2)\n")
	fmt.Fprintf(&b, "  (global $next (mut i32) (i32.const %d))\n", heapStart)
	fmt.Fprintf(&b, "  (data (i32.const %d) \"%s\")\n", responseOffset, escape(raw))
	if o.Initialize != "" {
		fmt.Fprintf(&b, "  (func (export \"_initialize\") %s)\n", o.Initialize)
	}
	// The allocator carries a name so a Body can call it — the way the host
	// does through wasmfn.http.
	b.WriteString(`  (func $wasmfn_alloc (export "wasmfn_alloc") (param $size i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $next))
    (global.set $next (i32.add (local.get $p) (local.get $size)))
    (block $fits
      (br_if $fits (i32.le_u (global.get $next) (i32.mul (memory.size) (i32.const 65536))))
      (drop (memory.grow (i32.add (i32.div_u (i32.sub (global.get $next) (i32.mul (memory.size) (i32.const 65536))) (i32.const 65536)) (i32.const 1)))))
    (local.get $p))
`)
	if !o.SkipRun {
		fmt.Fprintf(&b, "  (func (export \"wasmfn_run\") %s\n    %s)\n", sig, body)
	}
	b.WriteString(")\n")
	return b.String()
}

// Build compiles WAT to wasm.
func Build(t *testing.T, wat string) []byte {
	t.Helper()
	wasm, err := wasmtime.Wat2Wasm(wat)
	if err != nil {
		t.Fatalf("cannot assemble fixture:\n%s\n%v", wat, err)
	}
	return wasm
}

// escape renders bytes as a WAT string literal.
func escape(raw []byte) string {
	var b strings.Builder
	for _, c := range raw {
		fmt.Fprintf(&b, "\\%02x", c)
	}
	return b.String()
}
