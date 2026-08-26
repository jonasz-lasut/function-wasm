//! Runs function-wasm guest modules with wasmtime.
//!
//! This is the Rust port of the Go runtime's `internal/engine`: the host half
//! of ABI v1 (docs/abi.md). A guest is a wasip1 module exporting `memory`,
//! `wasmfn_alloc(size u32) -> u32` and `wasmfn_run(ptr u32, len u32) -> u64`,
//! exchanging protobuf-encoded RunFunctionRequest / RunFunctionResponse
//! messages through its linear memory. Every run gets a fresh store and
//! instance; the Engine, its linker and the compiled modules are shared.
//!
//! The engine works on request and response bytes: encoding and decoding the
//! protobuf messages is the caller's, so this crate depends on wasmtime and
//! nothing protocol-specific.

mod abi;
pub mod concurrency;
pub mod duration;
mod hosthttp;
mod hostlog;
pub mod metrics;
mod run;
mod sandbox;
pub mod wire;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use wasmtime::Linker;
use wasmtime_wasi::p1::WasiP1Ctx;

pub use hosthttp::HttpRequester;

/// Export names of ABI v1.
pub const EXPORT_MEMORY: &str = "memory";
pub const EXPORT_INITIALIZE: &str = "_initialize";
pub const EXPORT_ALLOC: &str = "wasmfn_alloc";
pub const EXPORT_RUN: &str = "wasmfn_run";

/// The import module name of the host functions a guest may use.
pub const HOST_MODULE: &str = "wasmfn";
/// The structured logging import: log(level u32, ptr u32, len u32).
pub const HOST_LOG: &str = "log";
/// The egress import: http(req_ptr u32, req_len u32) -> u64, answered within
/// the run's sandbox.egress grant (hosthttp.rs).
pub const HOST_HTTP: &str = "http";

/// The WASI preview 1 import module the host provides in full.
pub const WASI_MODULE: &str = "wasi_snapshot_preview1";

// What a guest sees as os.Args[0]. WASI guests written in Go (via klog's
// init) index os.Args[0], so an empty argv traps at _initialize.
const ARGV0: &str = "function";

// How often the engine's epoch counter advances; a run's deadline is
// expressed in ticks, so it is also the timeout granularity.
const EPOCH_TICK: Duration = Duration::from_millis(10);

/// Defaults applied for unset Config fields.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MEMORY_LIMIT: u64 = 512 << 20;
/// wasmtime's own default wasm stack ceiling, kept as ours.
pub const DEFAULT_STACK_LIMIT: u64 = 512 << 10;

/// An engine failure, formatted exactly as the Go runtime formats it: the
/// message is the contract (it ends up in an XR condition), not a type.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub(crate) String);

/// Config bounds what a single run may consume, and how many may run at
/// once.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// The wall-clock budget of one run.
    pub timeout: Duration,
    /// The cap on a guest's linear memory in bytes.
    pub memory_limit: u64,
    /// The cap on a guest's call stack in bytes; engine-wide, there is no
    /// Input field to narrow it per run.
    pub stack_limit: u64,
    /// Resolve trap backtraces to file and line through the module's DWARF
    /// (the runtime's --debug); costs DWARF parsing at compile time.
    pub backtrace_details: bool,
    /// Bounds how many runs execute at once on the whole engine, served
    /// round-robin by module key; 0 leaves concurrency to the caller.
    pub max_concurrent_runs: usize,
    /// Bounds the aggregate linear-memory reservation of all running
    /// modules in bytes; a run reserves its module's initial linear memory
    /// before it starts and each growth beyond it as the guest grows, so
    /// only memory a guest actually claims counts against the pool. 0 means
    /// no bound.
    pub max_total_run_memory: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            timeout: DEFAULT_TIMEOUT,
            memory_limit: DEFAULT_MEMORY_LIMIT,
            stack_limit: DEFAULT_STACK_LIMIT,
            backtrace_details: false,
            max_concurrent_runs: 0,
            max_total_run_memory: 0,
        }
    }
}

/// RunOptions narrow one run's budget below the Engine's Config - what a
/// Composition asks for through the Input's limits - and carry the sandbox
/// grants the run gets. An unset budget field means the Config's value; a
/// larger one is capped to it, so the Config stays the ceiling whatever a
/// caller passes.
#[derive(Default)]
pub struct RunOptions {
    /// This run's wall-clock budget.
    pub timeout: Option<Duration>,
    /// The request's own deadline (its gRPC timeout), when it carries one:
    /// it bounds the waits for slots and memory and caps the run budget,
    /// so a run never outlives the caller that asked for it.
    pub deadline: Option<Instant>,
    /// The cap on this run's linear memory in bytes.
    pub memory_limit: Option<u64>,

    /// Gives the guest a fresh, empty, writable /tmp for this run alone: a
    /// directory created under the host's temp dir before the instance
    /// exists and removed after it is gone, whatever the outcome.
    pub private_tmp: bool,
    /// The guest's environment variables (WASI environ); sorted by key.
    pub env: BTreeMap<String, String>,

    /// What answers the wasmfn.http import for this run. None is no grant:
    /// every call gets a refusal, never a trap.
    pub http: Option<Arc<dyn HttpRequester>>,

    /// The module's description and digest, attached to guest log lines.
    pub module: String,
    pub digest: String,

    /// Writes a Firefox-profiler JSON profile of this run into the
    /// directory (the runtime's --profile-guests, gated on --debug): the
    /// guest sampled every epoch tick, host imports marked. None runs
    /// without any profiling cost.
    pub profile_dir: Option<std::path::PathBuf>,
}

/// The per-store data: the WASI context, the memory limiter and the state
/// host functions reach through the store.
pub(crate) struct Ctx {
    wasi: WasiP1Ctx,
    limits: RunLimiter,
    call: CallState,
}

/// A run's reservation from the shared memory pool; dropping it - with the
/// store, whatever the run's outcome - releases the whole reservation.
pub(crate) struct PoolHold {
    pool: Arc<concurrency::MemPool>,
    n: u64,
}

impl PoolHold {
    /// A reservation of n bytes already taken from pool.
    pub(crate) fn new(pool: Arc<concurrency::MemPool>, n: u64) -> Self {
        PoolHold { pool, n }
    }

    fn grow(&mut self, delta: u64, deadline: Instant) -> Result<(), String> {
        self.pool.reserve(delta, deadline)?;
        self.n += delta;
        Ok(())
    }
}

impl Drop for PoolHold {
    fn drop(&mut self) {
        if self.n > 0 {
            self.pool.release(self.n);
        }
    }
}

/// The per-run memory limiter: enforces the run's ceiling (limits.memory or
/// the engine's memory_limit) per memory, and reserves growth beyond the
/// pre-reserved initial memory from the shared pool incrementally - so a
/// run's pool footprint is what its guest actually claimed, not the
/// worst-case ceiling. A growth the pool cannot serve before the run's
/// deadline is denied: the guest sees memory.grow fail, exactly as it does
/// at the ceiling.
pub(crate) struct RunLimiter {
    limit: usize,
    hold: Option<PoolHold>,
    /// Total bytes the store's memories have claimed (the sum of grow
    /// deltas, initial sizes included).
    charged: u64,
    deadline: Instant,
}

impl RunLimiter {
    pub(crate) fn new(limit: u64, hold: Option<PoolHold>, deadline: Instant) -> Self {
        RunLimiter {
            limit: limit as usize,
            hold,
            charged: 0,
            deadline,
        }
    }
}

impl wasmtime::ResourceLimiter for RunLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.limit {
            metrics::MEMORY_DENIALS.with_label_values(&["limit"]).inc();
            return Ok(false);
        }
        let delta = (desired - current) as u64;
        self.charged += delta;
        if let Some(hold) = &mut self.hold
            && self.charged > hold.n
        {
            let need = self.charged - hold.n;
            if let Err(e) = hold.grow(need, self.deadline) {
                self.charged -= delta;
                metrics::MEMORY_DENIALS.with_label_values(&["pool"]).inc();
                tracing::info!(error = %e, "Denied a memory growth");
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // Tables were never bounded (StoreLimits bounded memory_size only);
        // parity kept.
        Ok(true)
    }
}

/// The per-run state host functions reach through the store data.
pub(crate) struct CallState {
    module: String,
    digest: String,
    http: Option<Arc<dyn HttpRequester>>,
    deadline: Instant,
    // Throttles the audit line of a guest that calls wasmfn.http without a
    // grant to one info line per run.
    no_grant_logged: bool,
    timer: HostTimer,
    /// Time this run spent waiting on wasmfn.http answers - credited back to
    /// the epoch deadline so limits.timeout means guest compute. Only http
    /// time is creditable: each request is capped by the run's deadline, so
    /// the credit is self-limiting where crediting arbitrary host time
    /// (a wasmfn.log loop) would not be.
    http_host: Duration,
}

/// Splits a run's wall clock between guest code and host imports: every
/// call_hook transition charges the elapsed slice to whichever side the
/// innermost frame was on, so time a host import spends re-entered in the
/// guest (wasmfn.http calling wasmfn_alloc) counts as guest time.
pub(crate) struct HostTimer {
    /// One entry per live host<->wasm frame; true is a host frame.
    stack: Vec<bool>,
    last: Instant,
    host_total: Duration,
}

impl HostTimer {
    fn new() -> Self {
        HostTimer {
            stack: Vec::with_capacity(8),
            last: Instant::now(),
            host_total: Duration::ZERO,
        }
    }

    pub(crate) fn transition(&mut self, hook: wasmtime::CallHook) {
        let now = Instant::now();
        if self.stack.last() == Some(&true) {
            self.host_total += now - self.last;
        }
        self.last = now;
        match hook {
            wasmtime::CallHook::CallingWasm => self.stack.push(false),
            wasmtime::CallHook::CallingHost => self.stack.push(true),
            _ => {
                self.stack.pop();
            }
        }
    }

    pub(crate) fn host_total(&self) -> Duration {
        self.host_total
    }
}

/// A compiled, ABI-checked guest module with its imports resolved once into
/// an InstancePre, so a run only instantiates. It is safe for concurrent
/// runs and cheap to clone; wasmtime frees the code memory when the last
/// clone drops.
#[derive(Clone)]
pub struct Module {
    pub(crate) inner: wasmtime::Module,
    pub(crate) pre: wasmtime::InstancePre<Ctx>,
}

impl std::fmt::Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Module").field(&self.inner).finish()
    }
}

impl Module {
    /// The initial size in bytes of the module's exported memory - what
    /// instantiation claims before the guest runs, and so what a run
    /// reserves from the shared memory pool up front.
    pub(crate) fn initial_memory_bytes(&self) -> u64 {
        self.inner
            .exports()
            .find_map(|e| match e.ty() {
                wasmtime::ExternType::Memory(mt) if e.name() == EXPORT_MEMORY => {
                    Some(mt.minimum() << 16)
                }
                _ => None,
            })
            .unwrap_or(0)
    }
}

/// What Inspect reads from a module: its wasmfn host imports and, when the
/// module does not implement ABI v1, checkABI's refusal.
#[derive(Debug)]
pub struct Inspection {
    pub host_imports: Vec<String>,
    pub abi_error: Option<String>,
    /// Exports in declaration order.
    pub exports: Vec<Extern>,
    /// Imports in declaration order.
    pub imports: Vec<Extern>,
    /// Memories the module defines or imports.
    pub memories: Vec<MemoryLimits>,
}

/// One export or import, as a listing shows it.
#[derive(Debug, Clone)]
pub struct Extern {
    /// An import's module; empty for an export.
    pub module: String,
    pub name: String,
    /// func, memory, table or global.
    pub kind: String,
    /// A function's signature, "(i32, i32) -> (i64)"; empty otherwise.
    pub ty: String,
}

/// A memory's limits in 64 KiB pages.
#[derive(Debug, Clone)]
pub struct MemoryLimits {
    pub min: u64,
    /// None when unbounded.
    pub max: Option<u64>,
    pub shared: bool,
    pub memory64: bool,
}

fn extern_kind(ty: &wasmtime::ExternType) -> (String, String) {
    match ty {
        wasmtime::ExternType::Func(ft) => ("func".to_string(), abi::signature_of(ft)),
        wasmtime::ExternType::Memory(_) => ("memory".to_string(), String::new()),
        wasmtime::ExternType::Table(_) => ("table".to_string(), String::new()),
        wasmtime::ExternType::Global(_) => ("global".to_string(), String::new()),
        _ => ("?".to_string(), String::new()),
    }
}

fn memory_limits(mt: &wasmtime::MemoryType) -> MemoryLimits {
    MemoryLimits {
        min: mt.minimum(),
        max: mt.maximum(),
        shared: mt.is_shared(),
        memory64: mt.is_64(),
    }
}

/// Engine compiles and runs guest modules. It is safe for concurrent use.
pub struct Engine {
    config: Config,
    pub(crate) inner: wasmtime::Engine,
    pub(crate) linker: Linker<Ctx>,
    pub(crate) scheduler: Option<concurrency::FairScheduler>,
    pub(crate) mem: Option<Arc<concurrency::MemPool>>,
    active: Arc<AtomicI64>,
    stop: Arc<AtomicBool>,
    ticker: Option<thread::JoinHandle<()>>,
}

impl Engine {
    /// Creates an Engine; dropping it stops its epoch ticker.
    pub fn new(config: Config) -> Result<Self, Error> {
        // A pool smaller than the per-run ceiling could never admit a
        // full-limit run - caught at startup rather than as a fleet of
        // timing-out requests.
        if config.max_total_run_memory > 0 && config.max_total_run_memory < config.memory_limit {
            return Err(Error(format!(
                "--max-total-run-memory {} is smaller than the per-run ceiling {} (--module-memory-limit): no full-limit run could ever reserve its memory",
                concurrency::format_bytes(config.max_total_run_memory),
                concurrency::format_bytes(config.memory_limit)
            )));
        }
        if config.stack_limit == 0 {
            return Err(Error(
                "--module-stack-limit must be positive: a guest cannot run on an empty stack"
                    .to_string(),
            ));
        }
        let mut wc = wasmtime::Config::new();
        wc.epoch_interruption(true);
        // Native unwind info only serves host-side profilers; wasmtime's own
        // unwinder produces wasm traps and backtraces without it.
        wc.native_unwind_info(false);
        wc.max_wasm_stack(config.stack_limit as usize);
        // Explicit in both directions: the decision is the runtime's --debug,
        // never the WASMTIME_BACKTRACE_DETAILS environment wasmtime would
        // otherwise read.
        wc.wasm_backtrace_details(if config.backtrace_details {
            wasmtime::WasmBacktraceDetails::Enable
        } else {
            wasmtime::WasmBacktraceDetails::Disable
        });
        let inner =
            wasmtime::Engine::new(&wc).map_err(|e| Error(format!("cannot create engine: {e}")))?;

        let mut linker: Linker<Ctx> = Linker::new(&inner);
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |c: &mut Ctx| &mut c.wasi)
            .map_err(|e| Error(format!("cannot define WASI imports: {e}")))?;
        linker
            .func_wrap(HOST_MODULE, HOST_LOG, hostlog::host_log)
            .map_err(|e| {
                Error(format!(
                    "cannot define {HOST_MODULE}.{HOST_LOG} import: {e}"
                ))
            })?;
        linker
            .func_wrap(HOST_MODULE, HOST_HTTP, hosthttp::host_http)
            .map_err(|e| {
                Error(format!(
                    "cannot define {HOST_MODULE}.{HOST_HTTP} import: {e}"
                ))
            })?;

        let active = Arc::new(AtomicI64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        // The epoch only has to advance while a run is in flight; a deadline
        // is relative to the epoch at the moment it is set, so ticks between
        // runs never count against one. (The Go engine parks the ticker when
        // idle; a skipped increment every 10ms buys the same and stays simple.)
        let ticker = {
            let engine = inner.clone();
            let active = Arc::clone(&active);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    if active.load(Ordering::Relaxed) > 0 {
                        engine.increment_epoch();
                    }
                    thread::sleep(EPOCH_TICK);
                }
            })
        };

        Ok(Engine {
            config,
            inner,
            linker,
            scheduler: (config.max_concurrent_runs > 0)
                .then(|| concurrency::FairScheduler::new(config.max_concurrent_runs)),
            mem: (config.max_total_run_memory > 0)
                .then(|| Arc::new(concurrency::MemPool::new(config.max_total_run_memory))),
            active,
            stop,
            ticker: Some(ticker),
        })
    }

    /// The engine's ceilings: what a run gets without RunOptions and the most
    /// it can get with them.
    pub fn config(&self) -> Config {
        self.config
    }

    /// Compiles wasm bytes and verifies they export ABI v1.
    pub fn compile(&self, wasm: &[u8]) -> Result<Module, Error> {
        let start = std::time::Instant::now();
        let m = self.compiled(wasm)?;
        metrics::COMPILE_DURATION.observe(start.elapsed().as_secs_f64());
        abi::check_abi(&m)?;
        self.pre(m)
    }

    /// Resolves the module's imports against the linker once, so every run
    /// skips that work. After checkABI the only way this fails is a WASI
    /// import wasmtime-wasi does not define - refused here, at load, with
    /// the wording a run-time instantiation failure carried before.
    fn pre(&self, m: wasmtime::Module) -> Result<Module, Error> {
        let pre = self.linker.instantiate_pre(&m).map_err(|e| {
            Error(format!(
                "cannot instantiate module: {}",
                first_line(&e.to_string())
            ))
        })?;
        Ok(Module { inner: m, pre })
    }

    /// Compiles wasm bytes and reports what the runtime sees in them: the
    /// host imports and checkABI's verdict - what `function validate
    /// --resolve` shows. The compiled code is dropped.
    pub fn inspect(&self, wasm: &[u8]) -> Result<Inspection, Error> {
        let m = self.compiled(wasm)?;
        let host_imports = m
            .imports()
            .filter(|i| i.module() == HOST_MODULE)
            .map(|i| format!("{HOST_MODULE}.{}", i.name()))
            .collect();
        let abi_error = abi::check_abi(&m).err().map(|e| e.to_string());
        let mut exports = Vec::new();
        let mut imports = Vec::new();
        let mut memories = Vec::new();
        for ex in m.exports() {
            let (kind, ty) = extern_kind(&ex.ty());
            if let wasmtime::ExternType::Memory(mt) = ex.ty() {
                memories.push(memory_limits(&mt));
            }
            exports.push(Extern {
                module: String::new(),
                name: ex.name().to_string(),
                kind,
                ty,
            });
        }
        for im in m.imports() {
            let (kind, ty) = extern_kind(&im.ty());
            if let wasmtime::ExternType::Memory(mt) = im.ty() {
                // An imported memory precedes defined ones in the index
                // space.
                memories.insert(0, memory_limits(&mt));
            }
            imports.push(Extern {
                module: im.module().to_string(),
                name: im.name().to_string(),
                kind,
                ty,
            });
        }
        Ok(Inspection {
            host_imports,
            abi_error,
            exports,
            imports,
            memories,
        })
    }

    /// Returns wasmtime's compiled artifact for m: machine code that this
    /// engine - same wasmtime version, same host - can load again with
    /// deserialize_file instead of recompiling.
    pub fn serialize(&self, m: &Module) -> Result<Vec<u8>, Error> {
        m.inner
            .serialize()
            .map_err(|e| Error(format!("cannot serialize module: {e}")))
    }

    /// Loads an artifact serialize produced, mapping the file so the code
    /// stays file-backed instead of being copied to the heap. wasmtime
    /// refuses artifacts from another version or host, and the ABI is
    /// checked again, so a stale or foreign artifact is an error the caller
    /// treats as a cache miss.
    pub fn deserialize_file(&self, path: &std::path::Path) -> Result<Module, Error> {
        // SAFETY: the artifact comes from the runtime's own cache directory,
        // written by serialize; wasmtime validates its header and version.
        let m = unsafe { wasmtime::Module::deserialize_file(&self.inner, path) }.map_err(|e| {
            Error(format!(
                "cannot load compiled module: {}",
                first_line(&e.to_string())
            ))
        })?;
        abi::check_abi(&m)?;
        self.pre(m)
    }

    /// Only the binary format is accepted, as in the Go runtime: a module is
    /// what a toolchain produced, never text. A wasmtime compiled artifact
    /// (what serialize writes, a .cwasm) is named for what it is rather than
    /// failing as malformed wasm: artifacts are host- and version-specific
    /// cache entries, never a module source.
    fn compiled(&self, wasm: &[u8]) -> Result<wasmtime::Module, Error> {
        if wasmtime::Engine::detect_precompiled(wasm).is_some() {
            return Err(Error(
                "module is a wasmtime compiled artifact (.cwasm), not a wasm module".to_string(),
            ));
        }
        wasmtime::Module::from_binary(&self.inner, wasm).map_err(|e| {
            Error(format!(
                "cannot compile module: {}",
                first_line(&e.to_string())
            ))
        })
    }

    /// Instantiates the module and hands it the request bytes, within the
    /// engine's ceilings narrowed by opts. The returned bytes are whatever the
    /// guest produced; an error means the guest could not be run to completion
    /// (instantiation failure, trap, exit, deadline, memory limit or an ABI
    /// violation) and carries no response. Blocking: run it off the async
    /// executor.
    pub fn run(&self, m: &Module, request: &[u8], opts: RunOptions) -> Result<Vec<u8>, Error> {
        run::run(self, m, request, opts)
    }

    /// The budget of one run: the engine's ceilings narrowed by opts where
    /// opts asks for less.
    pub(crate) fn effective(&self, opts: &RunOptions) -> Config {
        let mut cfg = self.config;
        if let Some(t) = opts.timeout
            && t < cfg.timeout
        {
            cfg.timeout = t;
        }
        if let Some(m) = opts.memory_limit
            && m < cfg.memory_limit
        {
            cfg.memory_limit = m;
        }
        cfg
    }

    /// Marks a run in flight for the epoch ticker; the guard marks it done.
    pub(crate) fn running(&self) -> RunningGuard<'_> {
        self.active.fetch_add(1, Ordering::Relaxed);
        metrics::RUNS_IN_FLIGHT.inc();
        RunningGuard(&self.active)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.ticker.take() {
            let _ = t.join();
        }
    }
}

pub(crate) struct RunningGuard<'a>(&'a AtomicI64);

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
        metrics::RUNS_IN_FLIGHT.dec();
    }
}

/// Identifies the wasmtime release and host that compiled artifacts are only
/// valid for - the compiled cache is namespaced by it, so a bump changes the
/// namespace without anyone remembering to. Distinct from the Go runtime's
/// namespace on purpose: the two engines' artifacts are never assumed
/// interchangeable, even at the same wasmtime version.
pub fn version() -> String {
    format!(
        "rust-v{}-{}-{}",
        env!("FUNCTION_WASM_WASMTIME_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// The first line of a multi-line wasmtime message: the finding, without the
/// cause chain that does not belong in an XR condition.
pub(crate) fn first_line(s: &str) -> &str {
    match s.find('\n') {
        Some(i) => s[..i].trim(),
        None => s.trim(),
    }
}
