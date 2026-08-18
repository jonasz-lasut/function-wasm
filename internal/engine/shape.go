package engine

import (
	"fmt"

	"github.com/bytecodealliance/wasmtime-go/v47"
)

// Shape is what a module exports and imports, as wasmtime decoded it — the
// exports, imports and memories checkABI judges — with the ABI verdict. It
// is what guestfn build, push and inspect and function validate --resolve
// show: the runtime's own view of a module, read by the runtime's own
// decoder, so a verdict printed on a laptop is the verdict a load reaches.
type Shape struct {
	// Exports in declaration order.
	Exports []Extern
	// Imports in declaration order.
	Imports []Extern
	// Memories the module defines or imports.
	Memories []MemoryLimits
	// ABIError is checkABI's verdict: nil when the module implements ABI v1,
	// otherwise the load-time refusal, word for word.
	ABIError error
}

// Extern is one export or import.
type Extern struct {
	// Module is an import's module; empty for an export.
	Module string
	Name   string
	// Kind is func, memory, table or global.
	Kind string
	// Type is a function's signature, "(i32, i32) -> (i64)"; empty for
	// other kinds.
	Type string
}

// String renders the extern as a listing does: "wasmfn.log (i32, i32, i32)
// -> ()", "memory (memory)".
func (x Extern) String() string {
	name := x.Name
	if x.Module != "" {
		name = x.Module + "." + name
	}
	if x.Type != "" {
		return name + " " + x.Type
	}
	return name + " (" + x.Kind + ")"
}

// MemoryLimits are a memory's limits in 64 KiB pages.
type MemoryLimits struct {
	Min uint64
	// Max is nil when unbounded.
	Max      *uint64
	Shared   bool
	Memory64 bool
}

// HostImports lists the imports outside WASI — the wasmfn.* functions the
// module uses — as "wasmfn.log".
func (s *Shape) HostImports() []string {
	var out []string
	for _, im := range s.Imports {
		if im.Module != WASIModule {
			out = append(out, im.Module+"."+im.Name)
		}
	}
	return out
}

// Inspect compiles wasm and reports its Shape; the compiled code is
// released before it returns. wasmtime reads a module only by compiling it,
// so this costs a compile — seconds and about a gigabyte for a large Go
// guest — which is the price of the runtime's exact verdict rather than a
// second decoder's. A module wasmtime cannot compile is an error, as at
// load; one it compiles but that lacks the ABI is a Shape with ABIError set.
func (e *Engine) Inspect(wasm []byte) (*Shape, error) {
	m, err := wasmtime.NewModule(e.engine, wasm)
	if err != nil {
		return nil, fmt.Errorf("cannot compile module: %s", firstLine(err.Error()))
	}
	defer m.Close()
	return shapeOf(m), nil
}

// Shape describes a compiled, ABI-checked module: what Inspect reports,
// without another compile.
func (m *Module) Shape() *Shape {
	return shapeOf(m.module)
}

func shapeOf(m *wasmtime.Module) *Shape {
	s := &Shape{ABIError: checkABI(m)}
	for _, ex := range m.Exports() {
		kind, ty := externKind(ex.Type())
		s.Exports = append(s.Exports, Extern{Name: ex.Name(), Kind: kind, Type: ty})
		if mt := ex.Type().MemoryType(); mt != nil {
			s.Memories = append(s.Memories, memoryLimits(mt))
		}
	}
	for _, im := range m.Imports() {
		name := "?"
		if im.Name() != nil {
			name = *im.Name()
		}
		kind, ty := externKind(im.Type())
		s.Imports = append(s.Imports, Extern{Module: im.Module(), Name: name, Kind: kind, Type: ty})
		if mt := im.Type().MemoryType(); mt != nil {
			s.Memories = append([]MemoryLimits{memoryLimits(mt)}, s.Memories...)
		}
	}
	return s
}

func externKind(ty *wasmtime.ExternType) (kind, sig string) {
	switch {
	case ty == nil:
		return "?", ""
	case ty.FuncType() != nil:
		ft := ty.FuncType()
		return "func", signature(ft.Params(), ft.Results())
	case ty.MemoryType() != nil:
		return "memory", ""
	case ty.TableType() != nil:
		return "table", ""
	case ty.GlobalType() != nil:
		return "global", ""
	}
	return "?", ""
}

func memoryLimits(mt *wasmtime.MemoryType) MemoryLimits {
	l := MemoryLimits{Min: mt.Minimum(), Shared: mt.IsShared(), Memory64: mt.Is64()}
	if ok, max := mt.Maximum(); ok {
		l.Max = &max
	}
	return l
}
