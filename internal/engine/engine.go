// Package engine runs function-wasm guest modules with wasmtime.
//
// It implements the host half of ABI v1 (docs/abi.md): a guest is a wasip1
// module exporting memory, wasmfn_alloc(size u32) -> u32 and
// wasmfn_run(ptr u32, len u32) -> u64, exchanging protobuf-encoded
// RunFunctionRequest / RunFunctionResponse messages through its linear memory.
// Every Run gets a fresh store and instance; the Engine, its linker and the
// compiled modules are shared.
package engine

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/bytecodealliance/wasmtime-go/v47"

	"github.com/jonasz-lasut/function-wasm/internal/metrics"
)

// Export names of ABI v1.
const (
	ExportMemory     = "memory"
	ExportInitialize = "_initialize"
	ExportAlloc      = "wasmfn_alloc"
	ExportRun        = "wasmfn_run"

	// HostModule is the import module name of the host functions a guest may use.
	HostModule = "wasmfn"
	// HostLog is the structured logging import: log(level u32, ptr u32, len u32).
	HostLog = "log"

	wasiModule = "wasi_snapshot_preview1"

	// argv0 is what a guest sees as os.Args[0]. WASI guests written in Go
	// (via klog's init) index os.Args[0], so an empty argv traps at
	// _initialize.
	argv0 = "function"

	// epochTick is how often the engine's epoch counter advances; a Run's
	// deadline is expressed in ticks, so it is also the timeout granularity.
	epochTick = 10 * time.Millisecond
)

// Config bounds what a single Run may consume.
type Config struct {
	// Timeout is the wall-clock budget of one Run, applied on top of the
	// request context's deadline. Zero means DefaultTimeout.
	Timeout time.Duration
	// MemoryLimit caps a guest's linear memory in bytes. Zero means
	// DefaultMemoryLimit.
	MemoryLimit int64
	// CacheDir enables wasmtime's on-disk compiled-code cache in that
	// directory so restarts do not recompile modules. Empty disables it.
	CacheDir string
}

// Defaults applied for zero Config fields.
const (
	DefaultTimeout     = 30 * time.Second
	DefaultMemoryLimit = 512 << 20
)

// ErrTimeout reports that a guest exceeded its Run deadline.
var ErrTimeout = errors.New("module exceeded its execution deadline")

// Engine compiles and runs guest modules. It is safe for concurrent use.
type Engine struct {
	cfg    Config
	engine *wasmtime.Engine
	linker *wasmtime.Linker
	stop   chan struct{}
	done   chan struct{}
}

// New creates an Engine. Close it when done to stop its epoch ticker.
func New(cfg Config) (*Engine, error) {
	if cfg.Timeout <= 0 {
		cfg.Timeout = DefaultTimeout
	}
	if cfg.MemoryLimit <= 0 {
		cfg.MemoryLimit = DefaultMemoryLimit
	}

	wc := wasmtime.NewConfig()
	wc.SetEpochInterruption(true)
	if cfg.CacheDir != "" {
		path, err := writeCacheConfig(cfg.CacheDir)
		if err != nil {
			return nil, err
		}
		if err := wc.CacheConfigLoad(path); err != nil {
			return nil, fmt.Errorf("cannot enable wasmtime cache in %q: %w", cfg.CacheDir, err)
		}
	}
	engine := wasmtime.NewEngineWithConfig(wc)

	linker := wasmtime.NewLinker(engine)
	if err := linker.DefineWasi(); err != nil {
		return nil, fmt.Errorf("cannot define WASI imports: %w", err)
	}
	if err := linker.FuncWrap(HostModule, HostLog, hostLog); err != nil {
		return nil, fmt.Errorf("cannot define %s.%s import: %w", HostModule, HostLog, err)
	}

	e := &Engine{cfg: cfg, engine: engine, linker: linker, stop: make(chan struct{}), done: make(chan struct{})}
	go e.tick()
	return e, nil
}

// tick advances the engine epoch so stores with a deadline get interrupted.
func (e *Engine) tick() {
	defer close(e.done)
	t := time.NewTicker(epochTick)
	defer t.Stop()
	for {
		select {
		case <-t.C:
			e.engine.IncrementEpoch()
		case <-e.stop:
			return
		}
	}
}

// Close stops the epoch ticker. Compiled modules and running stores stay
// valid until garbage collected.
func (e *Engine) Close() {
	close(e.stop)
	<-e.done
}

// Module is a compiled, ABI-checked guest module. It is safe for concurrent
// Runs.
type Module struct {
	module *wasmtime.Module
}

// Compile compiles wasm bytes and verifies they export ABI v1.
func (e *Engine) Compile(wasm []byte) (*Module, error) {
	start := time.Now()
	m, err := wasmtime.NewModule(e.engine, wasm)
	metrics.CompileDuration.Observe(time.Since(start).Seconds())
	if err != nil {
		// wasmtime's message continues with a multi-line cause chain that
		// does not belong in an XR condition.
		return nil, fmt.Errorf("cannot compile module: %s", firstLine(err.Error()))
	}
	if err := checkABI(m); err != nil {
		return nil, err
	}
	return &Module{module: m}, nil
}

// checkABI verifies the exports and imports ABI v1 requires before a module
// is cached, so a wrong module fails once at load rather than on every run.
func checkABI(m *wasmtime.Module) error {
	exports := map[string]*wasmtime.ExternType{}
	for _, ex := range m.Exports() {
		exports[ex.Name()] = ex.Type()
	}
	if ty := exports[ExportMemory]; ty == nil || ty.MemoryType() == nil {
		return fmt.Errorf("module does not export a memory named %q", ExportMemory)
	}
	if err := checkFunc(exports, ExportAlloc, []wasmtime.ValKind{wasmtime.KindI32}, []wasmtime.ValKind{wasmtime.KindI32}); err != nil {
		return err
	}
	if err := checkFunc(exports, ExportRun, []wasmtime.ValKind{wasmtime.KindI32, wasmtime.KindI32}, []wasmtime.ValKind{wasmtime.KindI64}); err != nil {
		return err
	}
	if _, ok := exports[ExportInitialize]; ok {
		if err := checkFunc(exports, ExportInitialize, nil, nil); err != nil {
			return err
		}
	}
	for _, im := range m.Imports() {
		name := "?"
		if im.Name() != nil {
			name = *im.Name()
		}
		switch {
		case im.Module() == wasiModule:
		case im.Module() == HostModule && name == HostLog:
		default:
			return fmt.Errorf("module imports %s.%s, which the host does not provide", im.Module(), name)
		}
	}
	return nil
}

func checkFunc(exports map[string]*wasmtime.ExternType, name string, params, results []wasmtime.ValKind) error {
	ty := exports[name]
	if ty == nil {
		return fmt.Errorf("module does not export %q", name)
	}
	ft := ty.FuncType()
	if ft == nil {
		return fmt.Errorf("export %q is not a function", name)
	}
	if !kindsEqual(ft.Params(), params) || !kindsEqual(ft.Results(), results) {
		return fmt.Errorf("export %q has signature %s, ABI v1 requires %s", name, signature(ft.Params(), ft.Results()), signatureKinds(params, results))
	}
	return nil
}

func kindsEqual(got []*wasmtime.ValType, want []wasmtime.ValKind) bool {
	if len(got) != len(want) {
		return false
	}
	for i := range got {
		if got[i].Kind() != want[i] {
			return false
		}
	}
	return true
}

func signature(params, results []*wasmtime.ValType) string {
	p := make([]wasmtime.ValKind, len(params))
	for i := range params {
		p[i] = params[i].Kind()
	}
	r := make([]wasmtime.ValKind, len(results))
	for i := range results {
		r[i] = results[i].Kind()
	}
	return signatureKinds(p, r)
}

func signatureKinds(params, results []wasmtime.ValKind) string {
	return fmt.Sprintf("(%s) -> (%s)", kinds(params), kinds(results))
}

func kinds(ks []wasmtime.ValKind) string {
	s := ""
	for i, k := range ks {
		if i > 0 {
			s += ", "
		}
		s += k.String()
	}
	return s
}

func firstLine(s string) string {
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		return strings.TrimSpace(s[:i])
	}
	return strings.TrimSpace(s)
}

// deadlineTicks converts the effective budget of a Run — the smaller of the
// engine timeout and the context deadline — into epoch ticks.
func (e *Engine) deadlineTicks(ctx context.Context) (uint64, time.Duration) {
	budget := e.cfg.Timeout
	if d, ok := ctx.Deadline(); ok {
		if until := time.Until(d); until < budget {
			budget = until
		}
	}
	if budget <= 0 {
		return 1, budget
	}
	ticks := uint64((budget + epochTick - 1) / epochTick)
	if ticks == 0 {
		ticks = 1
	}
	return ticks, budget
}

// writeCacheConfig writes the TOML file wasmtime loads its compiled-code cache
// settings from. Only the directory is customised, so wasmtime's defaults for
// size limits and cleanup apply.
func writeCacheConfig(dir string) (string, error) {
	dir, err := filepath.Abs(dir)
	if err != nil {
		return "", fmt.Errorf("cannot resolve wasmtime cache directory: %w", err)
	}
	if err := os.MkdirAll(dir, 0o750); err != nil {
		return "", fmt.Errorf("cannot create wasmtime cache directory: %w", err)
	}
	path := filepath.Join(dir, "wasmtime-cache.toml")
	toml := "[cache]\ndirectory = " + strconv.Quote(dir) + "\n"
	if err := os.WriteFile(path, []byte(toml), 0o600); err != nil {
		return "", fmt.Errorf("cannot write wasmtime cache config: %w", err)
	}
	return path, nil
}
