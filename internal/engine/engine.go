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
	"path"
	"reflect"
	"runtime"
	"runtime/debug"
	"strings"
	"sync"
	"sync/atomic"
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
	// HostHTTP is the egress import: http(req_ptr u32, req_len u32) -> u64,
	// answered within the Run's sandbox.egress grant (hosthttp.go).
	HostHTTP = "http"

	// WASIModule is the WASI preview 1 import module the host provides in
	// full.
	WASIModule = "wasi_snapshot_preview1"
	wasiModule = WASIModule

	// argv0 is what a guest sees as os.Args[0]. WASI guests written in Go
	// (via klog's init) index os.Args[0], so an empty argv traps at
	// _initialize.
	argv0 = "function"

	// epochTick is how often the engine's epoch counter advances; a Run's
	// deadline is expressed in ticks, so it is also the timeout granularity.
	epochTick = 10 * time.Millisecond
)

// Config bounds what a single Run may consume, and how many may run at once.
type Config struct {
	// Timeout is the wall-clock budget of one Run, applied on top of the
	// request context's deadline. Zero means DefaultTimeout.
	Timeout time.Duration
	// MemoryLimit caps a guest's linear memory in bytes. Zero means
	// DefaultMemoryLimit.
	MemoryLimit int64
	// MaxConcurrentRuns bounds how many Runs execute at once on the whole
	// Engine; a further Run waits for a slot under its own context and
	// fails, having consumed nothing, when that ends first. Zero or less
	// means no bound: concurrency is the caller's.
	MaxConcurrentRuns int
	// Fuel enables instruction counting (wasmtime fuel). When true the
	// engine counts every wasm instruction executed; InstructionLimit is
	// the ceiling for limits.instructions.
	Fuel bool
	// InstructionLimit is the maximum number of instructions one run may
	// execute. Zero means metered but unbounded (the histogram observes,
	// nothing is capped). Only meaningful when Fuel is true.
	InstructionLimit int64
	// MaxTotalRunMemory bounds the aggregate linear-memory reservation
	// of all running modules (bytes). A Run reserves its effective limit
	// (limits.memory or the ceiling) from this pool before it starts and
	// returns it after; a Run that cannot fit waits under its context.
	// Zero means no bound.
	MaxTotalRunMemory int64
}

// Defaults applied for zero Config fields.
const (
	DefaultTimeout     = 30 * time.Second
	DefaultMemoryLimit = 512 << 20
)

// RunOptions narrow one Run's budget below the Engine's Config — what a
// Composition asks for through the Input's limits — and carry the sandbox
// grants the Run gets. A zero budget field means the Config's value; a larger
// one is capped to it, so the Config stays the ceiling whatever a caller
// passes.
type RunOptions struct {
	// Timeout is this Run's wall-clock budget; the request context's
	// deadline still applies if shorter.
	Timeout time.Duration
	// MemoryLimit caps this Run's linear memory in bytes.
	MemoryLimit int64

	// Sandbox grants — filesystem and environment (docs/one-pager-sandbox.md,
	// "Filesystem" and "Environment"). Their zero values are the default
	// sandbox: no pre-opened directories, no environment variables. Host
	// directories are never pre-opened: the private /tmp is the only
	// filesystem a guest can be given.

	// PrivateTmp gives the guest a fresh, empty, writable /tmp for this Run
	// alone: a directory created under os.TempDir() before the instance
	// exists and removed after it is gone, whatever the outcome.
	PrivateTmp bool
	// Env are the guest's environment variables (WASI environ).
	Env map[string]string

	// Instructions caps the wasm instructions this Run may execute
	// (wasmtime fuel). Zero means the Config's InstructionLimit; a larger
	// value is capped to it. Only effective when Config.Fuel is true.
	Instructions int64

	// HTTP egress (sandbox.egress): what answers the wasmfn.http import
	// for this Run. Nil is no grant — every call gets a refusal, never a
	// trap.
	HTTP HTTPRequester

	// InputName is the Input's metadata.name, threaded here so the
	// run-duration and run-instructions metrics carry it when the
	// --metrics-label-input-name flag is on. Empty is fine.
	InputName string
}

// ErrTimeout reports that a guest exceeded its Run deadline.
var ErrTimeout = errors.New("module exceeded its execution deadline")

// ErrFuel reports that a guest exhausted its instruction budget.
var ErrFuel = errors.New("module exceeded its instruction budget")

// Engine compiles and runs guest modules. It is safe for concurrent use.
type Engine struct {
	cfg       Config
	engine    *wasmtime.Engine
	linker    *wasmtime.Linker
	runs      chan struct{}
	mem       *memPool
	active    atomic.Int64
	wake      chan struct{}
	stop      chan struct{}
	done      chan struct{}
	closeOnce sync.Once
}

// WithDefaults returns c with DefaultTimeout and DefaultMemoryLimit applied
// for zero fields — the ceilings an Engine built from c would report.
func (c Config) WithDefaults() Config {
	if c.Timeout <= 0 {
		c.Timeout = DefaultTimeout
	}
	if c.MemoryLimit <= 0 {
		c.MemoryLimit = DefaultMemoryLimit
	}
	return c
}

// New creates an Engine. Close it when done to stop its epoch ticker.
func New(cfg Config) (*Engine, error) {
	cfg = cfg.WithDefaults()

	wc := wasmtime.NewConfig()
	wc.SetEpochInterruption(true)
	if cfg.Fuel {
		wc.SetConsumeFuel(true)
	}
	// Native unwind info (.eh_frame) only serves host-side profilers such as
	// perf; wasmtime's own unwinder produces wasm traps and backtraces
	// without it. Leaving it out makes artifacts about 5% smaller and, on
	// macOS, turns freeing a large module from seconds (each frame entry is
	// deregistered with the system unwinder under a global lock) into a
	// millisecond.
	wc.SetNativeUnwindInfo(false)
	engine := wasmtime.NewEngineWithConfig(wc)

	linker := wasmtime.NewLinker(engine)
	if err := linker.DefineWasi(); err != nil {
		return nil, fmt.Errorf("cannot define WASI imports: %w", err)
	}
	if err := linker.FuncWrap(HostModule, HostLog, hostLog); err != nil {
		return nil, fmt.Errorf("cannot define %s.%s import: %w", HostModule, HostLog, err)
	}
	if err := linker.FuncWrap(HostModule, HostHTTP, hostHTTP); err != nil {
		return nil, fmt.Errorf("cannot define %s.%s import: %w", HostModule, HostHTTP, err)
	}

	e := &Engine{cfg: cfg, engine: engine, linker: linker, wake: make(chan struct{}, 1), stop: make(chan struct{}), done: make(chan struct{})}
	if cfg.MaxConcurrentRuns > 0 {
		e.runs = make(chan struct{}, cfg.MaxConcurrentRuns)
	}
	if cfg.MaxTotalRunMemory > 0 {
		e.mem = newMemPool(cfg.MaxTotalRunMemory)
	}
	go e.tick()
	return e, nil
}

// tick advances the engine epoch so stores with a deadline get interrupted.
// The ticker only runs while a Run is in flight: an idle runtime costs no
// wakeups, and a deadline is measured from the moment it is set, so a
// pause between runs never counts against one.
func (e *Engine) tick() {
	defer close(e.done)
	for {
		select {
		case <-e.wake:
		case <-e.stop:
			return
		}
		t := time.NewTicker(epochTick)
		for e.active.Load() > 0 {
			select {
			case <-t.C:
				e.engine.IncrementEpoch()
			case <-e.stop:
				t.Stop()
				return
			}
		}
		t.Stop()
	}
}

// running marks a Run in flight for the epoch ticker and the in-flight
// gauge; the returned func marks it done.
func (e *Engine) running() func() {
	e.active.Add(1)
	metrics.RunsInFlight.Inc()
	select {
	case e.wake <- struct{}{}:
	default:
	}
	return func() {
		e.active.Add(-1)
		metrics.RunsInFlight.Dec()
	}
}

// slot waits for a run slot when the Engine bounds concurrent Runs and
// returns the func that gives it back. The wait is bounded by ctx alone — a
// request that cannot run before its deadline has nothing to gain from a
// slot afterwards — and a Run that never got one has consumed nothing.
func (e *Engine) slot(ctx context.Context) (release func(), err error) {
	if e.runs == nil {
		return func() {}, nil
	}
	select {
	case e.runs <- struct{}{}:
		return func() { <-e.runs }, nil
	case <-ctx.Done():
		return nil, fmt.Errorf("waiting for a run slot: %w", ctx.Err())
	}
}

// Config returns the engine's ceilings with defaults applied — what a Run
// gets without RunOptions and the most it can get with them.
func (e *Engine) Config() Config {
	return e.cfg
}

// Close stops the epoch ticker; calling it again is harmless. Compiled
// modules and running stores stay valid until garbage collected.
func (e *Engine) Close() {
	e.closeOnce.Do(func() { close(e.stop) })
	<-e.done
}

// Module is a compiled, ABI-checked guest module. It is safe for concurrent
// Runs. A Module is reference-counted: whoever obtains one — from Compile,
// Deserialize or a Cache — holds one lease and returns it with Release; the
// last Release frees wasmtime's code memory at once instead of when the
// garbage collector eventually notices a tiny Go wrapper.
type Module struct {
	module *wasmtime.Module
	leases atomic.Int64
}

// acquire adds a lease.
func (m *Module) acquire() { m.leases.Add(1) }

// Release returns a lease; the last one frees the module. Releasing more
// often than acquired is a bug that would free a module in use, so it is
// refused: the count never goes below zero.
func (m *Module) Release() {
	for {
		n := m.leases.Load()
		if n <= 0 {
			return
		}
		if m.leases.CompareAndSwap(n, n-1) {
			if n == 1 {
				m.module.Close()
			}
			return
		}
	}
}

// newModule wraps a checked wasmtime module with the caller's lease.
func newModule(m *wasmtime.Module) *Module {
	out := &Module{module: m}
	out.leases.Store(1)
	return out
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
		m.Close()
		return nil, err
	}
	return newModule(m), nil
}

// Serialize returns wasmtime's compiled artifact for m: machine code that
// this engine — same wasmtime version, same host — can load again with
// Deserialize instead of recompiling.
func (e *Engine) Serialize(m *Module) ([]byte, error) {
	b, err := m.module.Serialize()
	if err != nil {
		return nil, fmt.Errorf("cannot serialize module: %w", err)
	}
	return b, nil
}

// Deserialize loads an artifact Serialize produced. wasmtime refuses
// artifacts from another version or host, and the ABI is checked again, so a
// stale or foreign artifact is an error the caller treats as a cache miss.
func (e *Engine) Deserialize(artifact []byte) (*Module, error) {
	m, err := wasmtime.NewModuleDeserialize(e.engine, artifact)
	if err != nil {
		return nil, fmt.Errorf("cannot load compiled module: %s", firstLine(err.Error()))
	}
	return e.checked(m)
}

// DeserializeFile is Deserialize over an artifact file: wasmtime maps the
// file, so the code stays file-backed — shared with the page cache and
// between processes — instead of being copied into the Go heap first. That
// is milliseconds and no allocation for the largest Go guest.
func (e *Engine) DeserializeFile(path string) (*Module, error) {
	m, err := wasmtime.NewModuleDeserializeFile(e.engine, path)
	if err != nil {
		return nil, fmt.Errorf("cannot load compiled module: %s", firstLine(err.Error()))
	}
	return e.checked(m)
}

func (e *Engine) checked(m *wasmtime.Module) (*Module, error) {
	if err := checkABI(m); err != nil {
		m.Close()
		return nil, err
	}
	return newModule(m), nil
}

// Version identifies the wasmtime-go release and the host that compiled
// artifacts are only valid for, e.g. "v47.0.0-linux-arm64" — the compiled
// cache is namespaced by it. The version comes from the build's module
// information (falling back to the major in the import path), so a bump
// changes the namespace without anyone remembering to; wasmtime itself
// refuses artifacts it did not produce, this just keeps them apart.
//
// When fuel is true the suffix "-fuel" is appended: fuel changes wasmtime's
// code generation (it inserts instruction-counting bookkeeping), so
// artifacts compiled with fuel are not interchangeable with those without.
func Version(fuel bool) string {
	pkg := reflect.TypeOf(wasmtime.Engine{}).PkgPath()
	version := path.Base(pkg)
	if info, ok := debug.ReadBuildInfo(); ok {
		for _, dep := range info.Deps {
			if dep.Path == pkg {
				version = dep.Version
			}
		}
	}
	v := version + "-" + runtime.GOOS + "-" + runtime.GOARCH
	if fuel {
		v += "-fuel"
	}
	return v
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
			if err := checkImport(im, HostLog, []wasmtime.ValKind{wasmtime.KindI32, wasmtime.KindI32, wasmtime.KindI32}, nil); err != nil {
				return err
			}
		case im.Module() == HostModule && name == HostHTTP:
			if err := checkImport(im, HostHTTP, []wasmtime.ValKind{wasmtime.KindI32, wasmtime.KindI32}, []wasmtime.ValKind{wasmtime.KindI64}); err != nil {
				return err
			}
		default:
			return fmt.Errorf("module imports %s.%s, which the host does not provide", im.Module(), name)
		}
	}
	return nil
}

// checkImport verifies a host import's type at load, so a mismatch is one
// line once rather than wasmtime's causes at every instantiate.
func checkImport(im *wasmtime.ImportType, name string, params, results []wasmtime.ValKind) error {
	ft := im.Type().FuncType()
	if ft == nil || !kindsEqual(ft.Params(), params) || !kindsEqual(ft.Results(), results) {
		return fmt.Errorf("module imports %s.%s with the wrong type, ABI v1 requires %s", HostModule, name, signatureKinds(params, results))
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
	names := make([]string, len(ks))
	for i, k := range ks {
		names[i] = k.String()
	}
	return strings.Join(names, ", ")
}

func firstLine(s string) string {
	if i := strings.IndexByte(s, '\n'); i >= 0 {
		return strings.TrimSpace(s[:i])
	}
	return strings.TrimSpace(s)
}

// effective returns the budget of one Run: the engine's ceilings narrowed
// by opts where opts asks for less.
func (e *Engine) effective(opts RunOptions) Config {
	cfg := e.cfg
	if opts.Timeout > 0 && opts.Timeout < cfg.Timeout {
		cfg.Timeout = opts.Timeout
	}
	if opts.MemoryLimit > 0 && opts.MemoryLimit < cfg.MemoryLimit {
		cfg.MemoryLimit = opts.MemoryLimit
	}
	if cfg.InstructionLimit > 0 && opts.Instructions > 0 && opts.Instructions < cfg.InstructionLimit {
		cfg.InstructionLimit = opts.Instructions
	} else if opts.Instructions > 0 && cfg.InstructionLimit == 0 {
		cfg.InstructionLimit = opts.Instructions
	}
	return cfg
}

// deadlineTicks converts the effective budget of a Run — the smaller of
// timeout and the context deadline — into epoch ticks.
func deadlineTicks(ctx context.Context, timeout time.Duration) (uint64, time.Duration) {
	budget := timeout
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
