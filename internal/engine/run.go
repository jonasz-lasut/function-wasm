package engine

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"math"
	"strings"
	"time"

	"github.com/bytecodealliance/wasmtime-go/v47"
	"github.com/crossplane/function-sdk-go/logging"
	fnv1 "github.com/crossplane/function-sdk-go/proto/v1"
	"google.golang.org/protobuf/proto"

	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

// call is the per-Run state host functions reach through the store data.
type call struct {
	log logging.Logger
}

// Run instantiates m and hands it req. The returned response is whatever the
// guest produced; an error means the guest could not be run to completion
// (instantiation failure, trap, exit, deadline, memory limit or an ABI
// violation) and carries no response.
func (e *Engine) Run(ctx context.Context, m *Module, req *fnv1.RunFunctionRequest, log logging.Logger) (rsp *fnv1.RunFunctionResponse, err error) {
	start := time.Now()
	defer func() {
		metrics.RunDuration.WithLabelValues(outcome(err)).Observe(time.Since(start).Seconds())
	}()
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	in, err := proto.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("cannot encode request: %w", err)
	}
	if len(in) > math.MaxInt32 {
		return nil, fmt.Errorf("request of %d bytes exceeds what a 32-bit guest can address", len(in))
	}

	store := wasmtime.NewStoreWithData(e.engine, &call{log: log})
	defer store.Close()

	ticks, budget := e.deadlineTicks(ctx)
	store.SetEpochDeadline(ticks)
	store.Limiter(e.cfg.MemoryLimit, -1, -1, -1, -1)

	wasi := wasmtime.NewWasiConfig()
	wasi.SetArgv([]string{argv0})
	wasi.InheritStdout()
	wasi.InheritStderr()
	store.SetWasi(wasi)

	inst, err := e.linker.Instantiate(store, m.module)
	if err != nil {
		return nil, guestError("cannot instantiate module", err, budget, log)
	}
	if initialize := inst.GetFunc(store, ExportInitialize); initialize != nil {
		if _, err := initialize.Call(store); err != nil {
			return nil, guestError(ExportInitialize+" failed", err, budget, log)
		}
	}

	memory := inst.GetExport(store, ExportMemory).Memory()
	alloc := inst.GetFunc(store, ExportAlloc)
	run := inst.GetFunc(store, ExportRun)

	// wasm i32 and i64 values carry the ABI's unsigned pointers and lengths;
	// the conversions below reinterpret bits, they do not change values.
	size := int32(len(in)) //nolint:gosec // Bounded above.
	ret, err := alloc.Call(store, size)
	if err != nil {
		return nil, guestError(ExportAlloc+" failed", err, budget, log)
	}
	ptr := uint32(ret.(int32)) //nolint:gosec // i32 reinterpreted as a pointer.
	if err := checkBounds(memory.DataSize(store), ptr, uint32(size)); err != nil {
		return nil, fmt.Errorf("%s returned an invalid buffer: %w", ExportAlloc, err)
	}
	copy(memory.UnsafeData(store)[ptr:], in)

	ret, err = run.Call(store, int32(ptr), size) //nolint:gosec // Pointer passed back as i32.
	if err != nil {
		return nil, guestError(ExportRun+" failed", err, budget, log)
	}
	packed := uint64(ret.(int64))                        //nolint:gosec // i64 reinterpreted as packed ptr<<32|len.
	outPtr, outLen := uint32(packed>>32), uint32(packed) //nolint:gosec // Unpacking the halves.
	if err := checkBounds(memory.DataSize(store), outPtr, outLen); err != nil {
		return nil, fmt.Errorf("%s returned an invalid response buffer: %w", ExportRun, err)
	}
	// The store dies with this call, so the response is copied out.
	out := bytes.Clone(memory.UnsafeData(store)[outPtr : outPtr+outLen])

	rsp = &fnv1.RunFunctionResponse{}
	if err := proto.Unmarshal(out, rsp); err != nil {
		return nil, fmt.Errorf("cannot decode response: %w", err)
	}
	return rsp, nil
}

func outcome(err error) string {
	switch {
	case err == nil:
		return metrics.OutcomeOK
	case errors.Is(err, ErrTimeout):
		return metrics.OutcomeTimeout
	default:
		return metrics.OutcomeError
	}
}

func checkBounds(size uintptr, ptr, n uint32) error {
	if uint64(ptr)+uint64(n) > uint64(size) {
		return fmt.Errorf("[%#x, %#x) exceeds the %d bytes of guest memory", ptr, uint64(ptr)+uint64(n), size)
	}
	return nil
}

// guestError turns wasmtime's failure into something an operator can act on
// from an XR condition: a deadline interrupt becomes ErrTimeout, a WASI exit
// reports its status and a trap is named by its code. wasmtime's full message
// carries a wasm backtrace that is only useful next to the guest's own stderr,
// so it goes to the debug log.
func guestError(what string, err error, budget time.Duration, log logging.Logger) error {
	var trap *wasmtime.Trap
	if errors.As(err, &trap) {
		if code := trap.Code(); code != nil && *code == wasmtime.Interrupt {
			return fmt.Errorf("%s: %w (%s)", what, ErrTimeout, budget)
		}
		if log != nil {
			log.Debug("Guest trapped", "trap", trap.Message())
		}
		return fmt.Errorf("%s: %s", what, trapText(trap))
	}
	var werr *wasmtime.Error
	if errors.As(err, &werr) {
		if status, ok := werr.ExitStatus(); ok {
			return fmt.Errorf("%s: module exited with status %d", what, status)
		}
	}
	return fmt.Errorf("%s: %w", what, err)
}

// trapText names a trap without wasmtime's backtrace.
func trapText(trap *wasmtime.Trap) string {
	code := trap.Code()
	if code == nil {
		return "trap: " + lastLine(trap.Message())
	}
	switch *code {
	case wasmtime.StackOverflow:
		return "trap: call stack exhausted"
	case wasmtime.MemoryOutOfBounds:
		return "trap: out-of-bounds memory access"
	case wasmtime.UnreachableCodeReached:
		return "trap: unreachable code reached (a Go guest prints the panic to stderr)"
	case wasmtime.OutOfFuel:
		return "trap: out of fuel"
	case wasmtime.HeapMisaligned, wasmtime.TableOutOfBounds, wasmtime.IndirectCallToNull,
		wasmtime.BadSignature, wasmtime.IntegerOverflow, wasmtime.IntegerDivisionByZero,
		wasmtime.BadConversionToInteger, wasmtime.Interrupt:
		return "trap: " + lastLine(trap.Message())
	}
	return "trap: " + lastLine(trap.Message())
}

// lastLine returns the innermost cause of a multi-line wasmtime message.
func lastLine(msg string) string {
	msg = strings.TrimSpace(msg)
	if i := strings.LastIndex(msg, "\n"); i >= 0 {
		return strings.TrimSpace(msg[i+1:])
	}
	return msg
}
