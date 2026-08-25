//go:build wasip1

package main

import (
	"errors"
	"unsafe"
)

var errHostResponse = errors.New("wasmfn: the host answered with a buffer this guest did not allocate")

//go:wasmimport wasmfn http
func hostHTTP(ptr, size uint32) uint64

func init() {
	httpSink = func(payload []byte) ([]byte, error) {
		// The host reads the request during the call, then re-enters
		// wasmfnAlloc for its response — a buffer that lands in buffers like
		// any other — and hands back its pointer and length.
		packed := hostHTTP(uint32(uintptr(unsafe.Pointer(unsafe.SliceData(payload)))), uint32(len(payload)))
		ptr, n := uint32(packed>>32), uint32(packed)
		out, ok := buffers[ptr]
		if !ok || uint32(len(out)) < n {
			return nil, errHostResponse
		}
		delete(buffers, ptr)
		return out[:n], nil
	}
}
