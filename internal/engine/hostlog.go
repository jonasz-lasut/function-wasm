package engine

import (
	"encoding/json"

	"github.com/bytecodealliance/wasmtime-go/v47"
)

// levelDebug is the wasmfn.log level a guest sends for a debug record; any
// other level (0, info) logs at info.
const levelDebug int32 = 1

// logRecord is the JSON payload of one wasmfn.log call.
type logRecord struct {
	Msg string `json:"msg"`
	// KV are alternating keys and values, as logging.Logger takes them.
	KV []any `json:"kv"`
}

// hostLog implements the wasmfn.log import: it reads a JSON logRecord from
// guest memory and forwards it to the Run's logger. Malformed records are
// logged as-is rather than dropped or turned into traps, so a guest can never
// lose its own diagnostics.
func hostLog(caller *wasmtime.Caller, level, ptr, size int32) {
	c, ok := caller.Data().(*call)
	if !ok || c.log == nil {
		return
	}
	memory := caller.GetExport(ExportMemory)
	if memory == nil || memory.Memory() == nil {
		return
	}
	data := memory.Memory().UnsafeData(caller)
	// wasm i32 values carry unsigned pointers and lengths: reinterpret, then
	// slice in two steps so nothing is added in a type that can overflow.
	p, n := uint32(ptr), uint32(size) //nolint:gosec // i32 reinterpreted as u32.
	if checkBounds(uintptr(len(data)), p, n) != nil {
		c.log.Info("guest log record out of bounds", "ptr", p, "len", n)
		return
	}
	payload := data[p:][:n]

	rec := logRecord{}
	if err := json.Unmarshal(payload, &rec); err != nil {
		c.log.Info(string(payload), "wasmfn-log-error", err.Error())
		return
	}
	if len(rec.KV)%2 != 0 {
		rec.KV = append(rec.KV, "(missing value)")
	}
	if level == levelDebug {
		c.log.Debug(rec.Msg, rec.KV...)
		return
	}
	c.log.Info(rec.Msg, rec.KV...)
}
