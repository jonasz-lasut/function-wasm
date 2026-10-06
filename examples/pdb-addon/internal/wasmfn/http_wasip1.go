//go:build wasip1

package wasmfn

import (
	"errors"
	"fmt"
	"runtime"
	"unsafe"
)

//go:wasmimport wasmfn http
func hostHTTP(ptr, size uint32) uint64

// hostHTTPCall calls the wasmfn.http import. The host writes its response
// into a buffer it obtains from this module's wasmfn_alloc — re-entering the
// guest while this call is in flight — so the response is found among the
// pinned buffers, like a request is, and released once copied out.
func hostHTTPCall(payload []byte) ([]byte, error) {
	packed := hostHTTP(uint32(uintptr(unsafe.Pointer(unsafe.SliceData(payload)))), uint32(len(payload)))
	runtime.KeepAlive(payload)
	if packed == 0 {
		return nil, errors.New("wasmfn: the host returned no HTTP response")
	}
	ptr, size := uint32(packed>>32), uint32(packed)
	buf, ok := buffers[ptr]
	if !ok || uint32(len(buf)) < size {
		return nil, fmt.Errorf("wasmfn: the host's HTTP response at %#x (%d bytes) was not allocated with wasmfn_alloc", ptr, size)
	}
	delete(buffers, ptr)
	return buf[:size], nil
}
