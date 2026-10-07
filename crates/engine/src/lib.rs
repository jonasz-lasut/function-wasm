//! Runs function-wasm guest modules with wasmtime.
//!
//! The host half of the guest ABI (docs/abi-v2.md): a guest is a
//! WebAssembly component implementing the `wasmfn:function` world
//! (wit/wasmfn-function.wit), whose `run` export takes protobuf-encoded
//! RunFunctionRequest bytes and returns RunFunctionResponse bytes. Every
//! run gets a fresh store and instance; the Engine, its linker and the
//! compiled components are shared. A core module (ABI v1, removed in
//! function-wasm 1.0.0) is refused at load with one sentence.
//!
//! The engine works on request and response bytes: encoding and decoding the
//! protobuf messages is the caller's, so this crate depends on wasmtime and
//! nothing protocol-specific.

mod component;
pub mod componentize;
pub mod concurrency;
pub mod duration;
pub mod metrics;
mod run;
mod sandbox;
mod wasihttp;
pub mod wire;

pub use component::ABI_V2_WORLD;
pub use wire::HttpRequester;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// What every tool says of a core module - the runtime's load refusal,
/// `function validate --resolve`'s, `guestfn build`'s and `guestfn
/// inspect`'s: one sentence, so the conformance goldens pin one string.
/// ABI v1 (wasip1 core modules exporting `wasmfn_run`) was deprecated in
/// v0.6.0 and removed in 1.0.0 (issues #114 and #129).
pub const CORE_MODULE_REFUSAL: &str = "module is a core module, which function-wasm 1.0.0 no longer runs (ABI v1 was removed); build it as an ABI v2 component (docs/abi-v2.md)";

// What a guest sees as os.Args[0]. WASI guests written in Go (via klog's
// init) index os.Args[0], so an empty argv traps at instantiation.
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
    /// modules in bytes: a component exports no top-level memory, so a run
    /// reserves nothing up front and each growth is reserved as the guest
    /// claims it, so only memory a guest actually claims counts against
    /// the pool. 0 means no bound.
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

    /// What answers the guest's wasi:http sends for this run. None is no
    /// grant: every send gets a refusal, never a trap.
    pub http: Option<Arc<dyn HttpRequester>>,

    /// The module's description and digest, attached to guest log lines.
    pub module: String,
    pub digest: String,
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
/// the engine's memory_limit) per memory, and reserves every growth - the
/// initial memory included, since a component reserves nothing before it
/// is instantiated - from the shared pool incrementally, so a run's pool
/// footprint is what its guest actually claimed, not the worst-case
/// ceiling. A growth the pool cannot serve before the run's deadline is
/// denied: the guest sees memory.grow fail, exactly as it does at the
/// ceiling.
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
    timer: HostTimer,
}

/// Splits a run's wall clock between guest code and host imports: every
/// call_hook transition charges the elapsed slice to whichever side the
/// innermost frame was on, so time a host import spends re-entered in the
/// guest (the canonical ABI calling the guest's realloc) counts as guest
/// time.
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

/// A compiled, world-checked guest with its imports resolved once (the
/// bindgen's FunctionPre over an InstancePre), so a run only instantiates.
/// It is safe for concurrent runs and cheap to clone; wasmtime frees the
/// code memory when the last clone drops.
#[derive(Clone)]
pub struct Module {
    pub(crate) inner: wasmtime::component::Component,
    pub(crate) pre: component::FunctionPre<component::Ctx>,
}

impl std::fmt::Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Module").finish_non_exhaustive()
    }
}

/// What Inspect reads from a component: its top-level imports and exports,
/// the world's host imports among them, and, when the component does not
/// implement the world, the load check's refusal.
#[derive(Debug)]
pub struct Inspection {
    /// The world's host imports the component uses (its `log`).
    pub host_imports: Vec<String>,
    pub abi_error: Option<String>,
    /// Exports in declaration order.
    pub exports: Vec<Extern>,
    /// Imports in declaration order.
    pub imports: Vec<Extern>,
}

/// One top-level export or import of a component, as a listing shows it.
#[derive(Debug, Clone)]
pub struct Extern {
    /// The world's name for it: `run`, `log`, or an interface such as
    /// `wasi:cli/environment@0.2.6`.
    pub name: String,
    /// func, instance, type, resource, module, component or core func.
    pub kind: String,
}

/// Engine compiles and runs guest modules. It is safe for concurrent use.
pub struct Engine {
    config: Config,
    pub(crate) inner: wasmtime::Engine,
    pub(crate) linker: wasmtime::component::Linker<component::Ctx>,
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
        let linker = component::linker(&inner)?;

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

    /// Compiles component bytes and verifies they implement the world (the
    /// typecheck is the ABI check, once, at load).
    pub fn compile(&self, wasm: &[u8]) -> Result<Module, Error> {
        let start = std::time::Instant::now();
        let c = self.compiled(wasm)?;
        metrics::COMPILE_DURATION.observe(start.elapsed().as_secs_f64());
        self.pre(c)
    }

    /// Resolves the component's imports against the linker and typechecks
    /// it against the wasmfn:function world, once, so every run skips that
    /// work.
    fn pre(&self, c: wasmtime::component::Component) -> Result<Module, Error> {
        let pre = self
            .linker
            .instantiate_pre(&c)
            .and_then(component::FunctionPre::new)
            .map_err(|e| {
                Error(format!(
                    "component does not implement the {ABI_V2_WORLD} world: {}",
                    first_line(&e.to_string())
                ))
            })?;
        Ok(Module { inner: c, pre })
    }

    /// Compiles component bytes and reports what the runtime sees in them:
    /// the top-level items and the world typecheck's verdict - what
    /// `function validate --resolve` shows. The compiled code is dropped.
    pub fn inspect(&self, wasm: &[u8]) -> Result<Inspection, Error> {
        let c = self.compiled(wasm)?;
        let abi_error = self.pre(c.clone()).err().map(|e| e.to_string());
        let ty = c.component_type();
        let item_kind = |item: &wasmtime::component::types::ComponentItem| match item {
            wasmtime::component::types::ComponentItem::ComponentFunc(_) => "func",
            wasmtime::component::types::ComponentItem::CoreFunc(_) => "core func",
            wasmtime::component::types::ComponentItem::Module(_) => "module",
            wasmtime::component::types::ComponentItem::Component(_) => "component",
            wasmtime::component::types::ComponentItem::ComponentInstance(_) => "instance",
            wasmtime::component::types::ComponentItem::Type(_) => "type",
            wasmtime::component::types::ComponentItem::Resource(_) => "resource",
        };
        let imports: Vec<Extern> = ty
            .imports(&self.inner)
            .map(|(name, item)| Extern {
                name: name.to_string(),
                kind: item_kind(&item.ty).to_string(),
            })
            .collect();
        let exports = ty
            .exports(&self.inner)
            .map(|(name, item)| Extern {
                name: name.to_string(),
                kind: item_kind(&item.ty).to_string(),
            })
            .collect();
        let host_imports = imports
            .iter()
            .filter(|i| i.name == component::LOG_IMPORT)
            .map(|i| i.name.clone())
            .collect();
        Ok(Inspection {
            host_imports,
            abi_error,
            exports,
            imports,
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
    /// refuses artifacts from another version or host, and the world is
    /// checked again, so a stale or foreign artifact is an error the caller
    /// treats as a cache miss.
    pub fn deserialize_file(&self, path: &std::path::Path) -> Result<Module, Error> {
        // The artifact says what it is; one a pre-1.0 runtime compiled from
        // a core module is refused like the module itself would be.
        let kind = wasmtime::Engine::detect_precompiled_file(path).map_err(|e| {
            Error(format!(
                "cannot load compiled module: {}",
                first_line(&e.to_string())
            ))
        })?;
        if !matches!(kind, Some(wasmtime::Precompiled::Component)) {
            return Err(Error(CORE_MODULE_REFUSAL.to_string()));
        }
        // SAFETY: the artifact comes from the runtime's own cache directory,
        // written by serialize; wasmtime validates its header and version.
        let c = unsafe { wasmtime::component::Component::deserialize_file(&self.inner, path) }
            .map_err(|e| {
                Error(format!(
                    "cannot load compiled module: {}",
                    first_line(&e.to_string())
                ))
            })?;
        self.pre(c)
    }

    /// Only the binary format is accepted, as in the Go runtime: a module is
    /// what a toolchain produced, never text. A wasmtime compiled artifact
    /// (what serialize writes, a .cwasm) is named for what it is rather than
    /// failing as malformed wasm: artifacts are host- and version-specific
    /// cache entries, never a module source. A core module is named for
    /// what it is too: the ABI it implements is gone, and "does not export
    /// run" would send its author to the wrong fix.
    fn compiled(&self, wasm: &[u8]) -> Result<wasmtime::component::Component, Error> {
        if wasmtime::Engine::detect_precompiled(wasm).is_some() {
            return Err(Error(
                "module is a wasmtime compiled artifact (.cwasm), not a wasm module".to_string(),
            ));
        }
        if component::is_core_module_binary(wasm) {
            return Err(Error(CORE_MODULE_REFUSAL.to_string()));
        }
        wasmtime::component::Component::from_binary(&self.inner, wasm).map_err(|e| {
            Error(format!(
                "cannot compile module: {}",
                first_line(&e.to_string())
            ))
        })
    }

    /// Instantiates the module and hands it the request bytes, within the
    /// engine's ceilings narrowed by opts. The returned bytes are whatever the
    /// guest produced; an error means the guest could not be run to completion
    /// (instantiation failure, trap, exit, deadline, memory limit or the
    /// guest's own error string) and carries no response. Blocking: run it
    /// off the async executor.
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
