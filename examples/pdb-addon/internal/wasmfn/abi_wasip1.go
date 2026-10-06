//go:build wasip1

package wasmfn

import (
	"context"
	"fmt"
	"unsafe"
)

// buffers pins memory the host reads or writes through raw pointers so the Go
// garbage collector keeps it alive: input buffers handed out by wasmfn_alloc
// until wasmfn_run consumes them, and the last response until the next call.
// Go's wasm garbage collector does not move objects, so a pinned slice's
// address is stable.
var buffers = map[uint32][]byte{}

func pin(b []byte) uint32 {
	p := uint32(uintptr(unsafe.Pointer(unsafe.SliceData(b))))
	buffers[p] = b
	return p
}

//go:wasmexport wasmfn_alloc
func wasmfnAlloc(size uint32) uint32 {
	return pin(make([]byte, size))
}

//go:wasmexport wasmfn_run
func wasmfnRun(ptr, size uint32) uint64 {
	// The ABI requires the request to live in a wasmfn_alloc buffer, so the
	// slice is looked up rather than rebuilt from a raw address.
	in, ok := buffers[ptr]
	if !ok || uint32(len(in)) < size {
		out := encode(fatal(nil, fmt.Errorf("wasmfn_run: input at %#x (%d bytes) was not allocated with wasmfn_alloc", ptr, size)))
		clear(buffers)
		return uint64(pin(out))<<32 | uint64(len(out))
	}
	out := handle(context.Background(), in[:size])
	clear(buffers)
	return uint64(pin(out))<<32 | uint64(len(out))
}
