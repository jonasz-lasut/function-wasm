//go:build wasip1

package wasmfn

import (
	"runtime"
	"unsafe"
)

//go:wasmimport wasmfn log
func hostLog(level int32, ptr, size uint32)

func init() {
	logSink = hostSink
}

// stderrSink is only referenced by the portable default; on wasip1 the init
// above replaces it before any guest code can log.
func stderrSink(int32, []byte) {}

func hostSink(level int32, payload []byte) {
	if len(payload) == 0 {
		return
	}
	hostLog(level, uint32(uintptr(unsafe.Pointer(unsafe.SliceData(payload)))), uint32(len(payload)))
	runtime.KeepAlive(payload)
}
