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
pub mod duration;
mod hosthttp;
mod hostlog;
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

/// An engine failure, formatted exactly as the Go runtime formats it: the
/// message is the contract (it ends up in an XR condition), not a type.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub(crate) String);

/// Config bounds what a single run may consume.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// The wall-clock budget of one run.
    pub timeout: Duration,
    /// The cap on a guest's linear memory in bytes.
    pub memory_limit: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            timeout: DEFAULT_TIMEOUT,
            memory_limit: DEFAULT_MEMORY_LIMIT,
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
}

/// The per-store data: the WASI context, the memory limiter and the state
/// host functions reach through the store.
pub(crate) struct Ctx {
    wasi: WasiP1Ctx,
    limits: wasmtime::StoreLimits,
    call: CallState,
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
}

/// A compiled, ABI-checked guest module. It is safe for concurrent runs and
/// cheap to clone; wasmtime frees the code memory when the last clone drops.
#[derive(Clone)]
pub struct Module(pub(crate) wasmtime::Module);

/// What Inspect reads from a module: its wasmfn host imports and, when the
/// module does not implement ABI v1, checkABI's refusal.
#[derive(Debug)]
pub struct Inspection {
    pub host_imports: Vec<String>,
    pub abi_error: Option<String>,
}

/// Engine compiles and runs guest modules. It is safe for concurrent use.
pub struct Engine {
    config: Config,
    pub(crate) inner: wasmtime::Engine,
    pub(crate) linker: Linker<Ctx>,
    active: Arc<AtomicI64>,
    stop: Arc<AtomicBool>,
    ticker: Option<thread::JoinHandle<()>>,
}

impl Engine {
    /// Creates an Engine; dropping it stops its epoch ticker.
    pub fn new(config: Config) -> Result<Self, Error> {
        let mut wc = wasmtime::Config::new();
        wc.epoch_interruption(true);
        // Native unwind info only serves host-side profilers; wasmtime's own
        // unwinder produces wasm traps and backtraces without it.
        wc.native_unwind_info(false);
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
        let m = self.compiled(wasm)?;
        abi::check_abi(&m)?;
        Ok(Module(m))
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
        Ok(Inspection {
            host_imports,
            abi_error,
        })
    }

    /// Only the binary format is accepted, as in the Go runtime: a module is
    /// what a toolchain produced, never text.
    fn compiled(&self, wasm: &[u8]) -> Result<wasmtime::Module, Error> {
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
    }
}

/// The first line of a multi-line wasmtime message: the finding, without the
/// cause chain that does not belong in an XR condition.
pub(crate) fn first_line(s: &str) -> &str {
    match s.find('\n') {
        Some(i) => s[..i].trim(),
        None => s.trim(),
    }
}
