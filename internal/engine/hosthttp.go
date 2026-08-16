package engine

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"net/url"
	"strconv"

	"github.com/bytecodealliance/wasmtime-go/v47"

	"github.com/jonasz-lasut/function-wasm/internal/egress/wire"
	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

// HTTPRequester answers the wasmfn.http import for one Run: the host side of
// the module's sandbox.egress grant. It never fails — whatever stops a
// request is a Response with Status 0 and an Error — so a guest always gets
// a well-formed answer and never a trap. *egress.Client implements it; the
// payload types live in internal/egress/wire so the engine does not depend
// on the policy.
type HTTPRequester interface {
	Do(ctx context.Context, req *wire.Request) *wire.Response
}

// noEgress is what a wasmfn.http call gets on a Run without a grant. (A
// grant on a runtime without --enable-sandbox-egress never reaches a run:
// it is a fatal result before the module is resolved.)
const noEgress = "sandbox.egress: HTTP egress is not granted to this module: the Composition's Input names no sandbox.egress.http rule"

// hostHTTP implements the wasmfn.http import (docs/abi.md): it reads a JSON
// wire.Request from guest memory, has the Run's HTTPRequester perform it,
// allocates the JSON wire.Response in the guest through its own
// wasmfn_alloc, copies it there and returns ptr<<32|len. Every request-level
// failure travels back inside the Response; only what leaves the instance
// unusable — a trap inside wasmfn_alloc — becomes a trap of the run.
func hostHTTP(caller *wasmtime.Caller, ptr, size int32) (int64, *wasmtime.Trap) {
	c, ok := caller.Data().(*call)
	if !ok {
		return 0, wasmtime.NewTrap("wasmfn.http called outside a run")
	}
	memory := caller.GetExport(ExportMemory)
	if memory == nil || memory.Memory() == nil {
		return 0, wasmtime.NewTrap("wasmfn.http: the module exports no memory")
	}
	// The request is copied out before anything else runs in the guest:
	// wasmfn_alloc may grow the memory and move it.
	data := memory.Memory().UnsafeData(caller)
	p, n := uint32(ptr), uint32(size) //nolint:gosec // i32 reinterpreted as u32.
	if err := checkBounds(uintptr(len(data)), p, n); err != nil {
		return 0, wasmtime.NewTrap("wasmfn.http: request buffer " + err.Error())
	}
	payload := bytes.Clone(data[p:][:n])

	out := encodeResponse(c.serveHTTP(payload))

	// The response lives in a buffer the guest allocated, so a guest built
	// with wasmfn finds it in its pinned buffers the way it finds the request.
	allocExport := caller.GetExport(ExportAlloc)
	if allocExport == nil || allocExport.Func() == nil {
		return 0, wasmtime.NewTrap("wasmfn.http: the module exports no " + ExportAlloc)
	}
	ret, err := allocExport.Func().Call(caller, int32(len(out))) //nolint:gosec // Bounded above.
	if err != nil {
		var trap *wasmtime.Trap
		if errors.As(err, &trap) {
			return 0, trap
		}
		return 0, wasmtime.NewTrap(ExportAlloc + " failed inside wasmfn.http: " + firstLine(err.Error()))
	}
	allocated, ok := ret.(int32)
	if !ok {
		return 0, wasmtime.NewTrap(fmt.Sprintf("%s returned %T, ABI v1 requires i32", ExportAlloc, ret))
	}
	outPtr := uint32(allocated) //nolint:gosec // i32 reinterpreted as a pointer.
	data = memory.Memory().UnsafeData(caller)
	if err := checkBounds(uintptr(len(data)), outPtr, uint32(len(out))); err != nil { //nolint:gosec // Bounded above.
		return 0, wasmtime.NewTrap(ExportAlloc + " returned an invalid buffer inside wasmfn.http: " + err.Error())
	}
	copy(data[outPtr:], out)
	return int64(uint64(outPtr)<<32 | uint64(len(out))), nil //nolint:gosec // Packed ptr<<32|len reinterpreted as i64.
}

// encodeResponse renders the JSON a guest reads. A Response always encodes
// (strings, ints, bytes and a header map), so the fallback is unreachable in
// practice; the size check keeps a 32-bit guest addressable.
func encodeResponse(rsp *wire.Response) []byte {
	out, err := json.Marshal(rsp)
	if err != nil {
		return []byte(`{"status":0,"error":` + strconv.Quote("sandbox.egress: cannot encode the response: "+err.Error()) + `}`)
	}
	if len(out) > math.MaxInt32 {
		return []byte(`{"status":0,"error":"sandbox.egress: the response exceeds what a 32-bit guest can address"}`)
	}
	return out
}

// serveHTTP decodes one request and answers it: through the Run's grant,
// bounded by the Run's remaining deadline, or with a refusal when the Run
// has none.
func (c *call) serveHTTP(payload []byte) *wire.Response {
	req := &wire.Request{}
	if err := json.Unmarshal(payload, req); err != nil {
		metrics.HTTPRequests.WithLabelValues(metrics.OutcomeError).Inc()
		msg := "sandbox.egress: cannot decode the request: " + err.Error()
		if c.log != nil {
			c.log.Info("Module HTTP request", "outcome", metrics.OutcomeError, "error", msg)
		}
		return &wire.Response{Error: msg}
	}
	if c.http == nil {
		metrics.HTTPRequests.WithLabelValues(metrics.OutcomeRefused).Inc()
		if c.log != nil {
			host, path := "", ""
			if u, err := url.Parse(req.URL); err == nil {
				host, path = u.Hostname(), u.Path
			}
			method := req.Method
			if method == "" {
				method = "GET"
			}
			// A guest without a grant that keeps calling gets one info line,
			// then debug: it is the guest looping, not the host.
			logf := c.log.Info
			if c.noGrantLogged {
				logf = c.log.Debug
			}
			c.noGrantLogged = true
			logf("Module HTTP request", "method", method, "outcome", metrics.OutcomeRefused, "host", host, "path", path, "error", noEgress)
		}
		return &wire.Response{Error: noEgress}
	}
	ctx := c.ctx
	if !c.deadline.IsZero() {
		var cancel context.CancelFunc
		ctx, cancel = context.WithDeadline(ctx, c.deadline)
		defer cancel()
	}
	return c.http.Do(ctx, req)
}
