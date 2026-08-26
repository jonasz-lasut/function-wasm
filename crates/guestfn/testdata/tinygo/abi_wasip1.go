//go:build wasip1

package main

import "unsafe"

// buffers pins memory the host reads or writes through raw pointers so the
// garbage collector keeps it alive (TinyGo's collector does not move objects):
// input buffers handed out by wasmfn_alloc until wasmfn_run consumes them, and
// the last response until the next call.
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
	in, ok := buffers[ptr]
	if !ok || uint32(len(in)) < size {
		out := encode(fatal(nil, "wasmfn_run: input was not allocated with wasmfn_alloc"))
		clear(buffers)
		return uint64(pin(out))<<32 | uint64(len(out))
	}
	out := handle(in[:size])
	clear(buffers)
	return uint64(pin(out))<<32 | uint64(len(out))
}

//go:wasmimport wasmfn log
func hostLog(level int32, ptr, size uint32)

func init() {
	logSink = func(level int32, payload []byte) {
		if len(payload) == 0 {
			return
		}
		hostLog(level, uint32(uintptr(unsafe.Pointer(unsafe.SliceData(payload)))), uint32(len(payload)))
	}
}

// stderrSink is only referenced by the portable default; on wasip1 init
// replaces it before any guest code can log.
func stderrSink(int32, []byte) {}
