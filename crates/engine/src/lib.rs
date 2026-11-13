//! What the engines of guestfn share, behind one `Engine` trait: what the
//! runtime holds an engine to (compile, inspect, run, version), the
//! ceilings (`Config`), one run's narrowing and grants (`RunOptions`), the
//! inspection a tool reads, the egress seam (`wire::HttpRequester`), the
//! run slots and the memory pool (`concurrency`), the Go-style durations,
//! the Prometheus series (`metrics`), the pass-through gRPC codec (`grpc`)
//! and the writing half of the OCI image layout (`layout`). An engine is a
//! crate of its own beside this one: guestfn-engine-wasm, the Wasmtime
//! engine, and guestfn-engine-oci, the OCI engine of
//! docs/one-pager-oci-runtime.md (a Firecracker microVM per request).

pub mod concurrency;
pub mod duration;
pub mod grpc;
pub mod layout;
pub mod metrics;
pub mod wire;

pub use wire::HttpRequester;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Defaults applied for unset Config fields.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MEMORY_LIMIT: u64 = 512 << 20;
/// wasmtime's own default wasm stack ceiling, kept as ours.
pub const DEFAULT_STACK_LIMIT: u64 = 512 << 10;
/// The runtime's --module-size-limit default.
pub const DEFAULT_MODULE_SIZE_LIMIT: u64 = 128 << 20;

/// An engine failure, whose message wording is pinned: the
/// message is the contract (it ends up in an XR condition), not a type.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub String);

/// Why a run produced no response: what the engines and their callers
/// tell apart, so that none of them reads it out of the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFailure {
    /// The wait for a run slot or for run memory was cut short by the
    /// deadline: the module never ran, and the run metric does not count
    /// it.
    Waiting,
    /// The module outlived its budget, whichever clock ended it.
    Deadline,
    /// The module was ended at its memory limit.
    MemoryLimit,
    /// The module answered with an error of its own: the world's
    /// `err(string)`, a gRPC status.
    Returned,
    /// The module stopped without answering: a trap, an exit, a signal, a
    /// failed instantiation.
    Stopped,
    /// The host could not start the run, or lost it.
    Host,
}

/// A failed run: its kind for the code, and the message for the reader,
/// whose wording is pinned as `Error`'s is (it ends up in an XR
/// condition). `Display` is the message alone.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct RunError {
    kind: RunFailure,
    message: String,
}

impl RunError {
    pub fn new(kind: RunFailure, message: impl Into<String>) -> Self {
        RunError {
            kind,
            message: message.into(),
        }
    }

    /// A wait the deadline cut short, in the words the slot or the pool
    /// gave it.
    pub fn waiting(message: String) -> Self {
        Self::new(RunFailure::Waiting, message)
    }

    /// A run that outlived its budget: one sentence for both engines,
    /// after what the engine was doing.
    pub fn deadline(what: &str, budget: Duration) -> Self {
        Self::new(
            RunFailure::Deadline,
            format!(
                "{what}: module exceeded its execution deadline ({})",
                duration::format(budget)
            ),
        )
    }

    pub fn kind(&self) -> RunFailure {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A failure of the engine's own on the way to a run is the host's.
impl From<Error> for RunError {
    fn from(e: Error) -> Self {
        Self::new(RunFailure::Host, e.0)
    }
}

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
    /// The cap on a module's bytes as the resolver fetches them (the
    /// runtime's --module-size-limit). The resolver enforces it; an engine
    /// that expands what it is handed (the OCI engine unpacking an image's
    /// layers) bounds the expansion by it.
    pub module_size_limit: u64,
}

impl Config {
    /// The budget of one run: these ceilings narrowed by opts where opts
    /// asks for less.
    pub fn narrowed_by(&self, opts: &RunOptions) -> Config {
        let mut cfg = *self;
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

    /// A pool smaller than the per-run ceiling could never admit a
    /// full-limit run: refused at startup rather than met as a fleet of
    /// timing-out requests.
    pub fn check_run_memory(&self) -> Result<(), Error> {
        if self.max_total_run_memory > 0 && self.max_total_run_memory < self.memory_limit {
            return Err(Error(format!(
                "--max-total-run-memory {} is smaller than the per-run ceiling {} (--module-memory-limit): no full-limit run could ever reserve its memory",
                concurrency::format_bytes(self.max_total_run_memory),
                concurrency::format_bytes(self.memory_limit)
            )));
        }
        Ok(())
    }

    /// The run slots `max_concurrent_runs` bounds, when it bounds any.
    pub fn scheduler(&self) -> Option<concurrency::FairScheduler> {
        (self.max_concurrent_runs > 0)
            .then(|| concurrency::FairScheduler::new(self.max_concurrent_runs))
    }

    /// The memory pool `max_total_run_memory` bounds, when it bounds one.
    pub fn memory_pool(&self) -> Option<Arc<concurrency::MemPool>> {
        (self.max_total_run_memory > 0)
            .then(|| Arc::new(concurrency::MemPool::new(self.max_total_run_memory)))
    }
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
            module_size_limit: DEFAULT_MODULE_SIZE_LIMIT,
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

    /// The arguments an OCI module's manifest gives its binary, replacing
    /// the image's command. An engine with no command line ignores them.
    pub args: Vec<String>,

    /// The module's description and digest, attached to guest log lines.
    pub module: String,
    pub digest: String,
}

impl RunOptions {
    /// When a wait under this budget ends: the budget from now, or the
    /// request's own deadline when that is sooner.
    pub fn deadline_within(&self, budget: Duration) -> Instant {
        let within = Instant::now() + budget;
        match self.deadline {
            Some(d) if d < within => d,
            _ => within,
        }
    }
}

/// What Inspect reads from a module without running it: for a component,
/// its top-level imports and exports with the world's host imports among
/// them; for an OCI image, its config as `details`; for either, the
/// contract check's refusal when the module does not implement it.
#[derive(Debug, Default)]
pub struct Inspection {
    /// The world's host imports the component uses (its `log`).
    pub host_imports: Vec<String>,
    /// The contract verdict: None when the module implements the engine's
    /// contract, else the sentence the load prints.
    pub abi_error: Option<String>,
    /// Exports in declaration order.
    pub exports: Vec<Extern>,
    /// Imports in declaration order.
    pub imports: Vec<Extern>,
    /// What else the engine read, as `name: value` lines a tool prints in
    /// order (an image's entrypoint, user and platform); empty for a
    /// component, whose shape is the lists above.
    pub details: Vec<(String, String)>,
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

/// What the runtime holds an engine to. `runner.rs` compiles a module once
/// per digest (`cache.rs` keeps the result in memory and as a serialized
/// artifact on disk, under `version()`), inspects it for `guestfn` and
/// `function validate --resolve`, and runs it once per request under the
/// `RunOptions` admission settled. An engine is safe for concurrent use.
/// Files an engine derives from a compiled module at run time and keeps as
/// the compiled artifacts are kept: the OCI engine's VM snapshots. The
/// runtime's compiled store implements it, so the store's sweep bounds
/// these files, its size gauge counts them and its per-process key signs
/// them. A name is the engine's own and has no path separator. A file is
/// written in place, since a VM's memory is too large to hand over as
/// bytes, and served only once sealed.
pub trait ArtifactStore: Send + Sync + 'static {
    /// Where to write a new file for name. Nothing serves it before `seal`.
    fn begin(&self, name: &str) -> Result<std::path::PathBuf, String>;

    /// Signs what was written at `begin`'s path and publishes it under
    /// name; returns the entry's path.
    fn seal(&self, name: &str) -> Result<std::path::PathBuf, String>;

    /// The entry under name when this store sealed it and it still
    /// verifies; None otherwise, with a refused entry removed.
    fn sealed(&self, name: &str) -> Option<std::path::PathBuf>;

    /// Marks an entry used, for a sweep that removes the least recently
    /// used first.
    fn used(&self, path: &std::path::Path);

    /// Keeps the entry at path out of the sweep until `release`. An engine
    /// that opens a stored file by its path at every run (the OCI engine's
    /// root filesystem, which each VM mounts) holds it for as long as the
    /// module is loaded: a module in memory whose file went would fail
    /// every run until it left memory. Holds count, so an entry loaded
    /// twice is released twice. A hold on a path that is no entry keeps
    /// nothing: the caller checks the file after taking the hold.
    fn hold(&self, path: &std::path::Path);

    /// Gives back one `hold` on path.
    fn release(&self, path: &std::path::Path);
}

pub trait Engine: Send + Sync + 'static {
    /// A compiled module: what `compile` makes and `run` takes, cheap to
    /// clone and shared between runs.
    type Module: Clone + Send + Sync + std::fmt::Debug + 'static;

    /// The module type this engine serves, as the manifest's `type` names
    /// it: `wasm` or `oci`. The runtime fetches, admits and describes
    /// modules by it, and refuses the other type's by name.
    const KIND: &'static str;

    /// The contract the engine implements, as the manifest names it:
    /// `wasm@1.0` for the component world, `oci@0.1` for the process
    /// contract.
    const CONTRACT: &'static str;

    /// The ceilings this engine was created with.
    fn config(&self) -> Config;

    /// Compiles a module from its bytes, checking its contract.
    fn compile(&self, bytes: &[u8]) -> Result<Self::Module, Error>;

    /// Reads a module's shape and the contract verdict without running it.
    fn inspect(&self, bytes: &[u8]) -> Result<Inspection, Error>;

    /// The compiled module as bytes the same engine version loads again
    /// with `deserialize_file`.
    fn serialize(&self, m: &Self::Module) -> Result<Vec<u8>, Error>;

    /// Loads a module `serialize` wrote.
    fn deserialize_file(&self, path: &std::path::Path) -> Result<Self::Module, Error>;

    /// The module once the runtime has stored its serialized artifact at
    /// path. An engine whose module is a file on disk (the OCI engine's
    /// root filesystem) continues from the stored one, so what it derives
    /// from that file outlives this load; the default keeps the module as
    /// it is.
    fn stored(&self, m: Self::Module, _path: &std::path::Path) -> Self::Module {
        m
    }

    /// Makes ready, at warm-up, what the module's first run would
    /// otherwise pay for. The OCI engine takes the image's VM snapshot for
    /// the machine a step with no limits of its own gets; the default does
    /// nothing.
    fn warm(&self, _m: &Self::Module) -> Result<(), Error> {
        Ok(())
    }

    /// Hands the engine the runtime's compiled store, for what it derives
    /// from a stored module at run time: the OCI engine keeps an image's
    /// VM snapshots there, under the store's key, bound and gauge. The
    /// default derives nothing and ignores it.
    fn keep_in(&self, _store: Arc<dyn ArtifactStore>) {}

    /// Runs one request through a module under the run's options and
    /// returns the response bytes; every failure is a `RunError`, whose
    /// message is the fatal result's and whose kind says how the run
    /// ended.
    fn run(&self, m: &Self::Module, request: &[u8], opts: RunOptions) -> Result<Vec<u8>, RunError>;

    /// What namespaces the compiled store on disk: an artifact is loaded
    /// only by the engine version that wrote it.
    fn version() -> String
    where
        Self: Sized;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_is_narrowed_never_widened() {
        let config = Config {
            timeout: Duration::from_secs(30),
            memory_limit: 512 << 20,
            ..Default::default()
        };
        let narrowed = config.narrowed_by(&RunOptions {
            timeout: Some(Duration::from_secs(5)),
            memory_limit: Some(64 << 20),
            ..Default::default()
        });
        assert_eq!(
            (narrowed.timeout, narrowed.memory_limit),
            (Duration::from_secs(5), 64 << 20)
        );
        let widened = config.narrowed_by(&RunOptions {
            timeout: Some(Duration::from_secs(60)),
            memory_limit: Some(1 << 30),
            ..Default::default()
        });
        assert_eq!(
            (widened.timeout, widened.memory_limit),
            (config.timeout, config.memory_limit)
        );
    }

    #[test]
    fn a_pool_below_the_per_run_ceiling_is_refused() {
        let config = Config {
            memory_limit: 512 << 20,
            max_total_run_memory: 256 << 20,
            ..Default::default()
        };
        assert_eq!(
            config.check_run_memory().unwrap_err().0,
            "--max-total-run-memory 256Mi is smaller than the per-run ceiling 512Mi (--module-memory-limit): no full-limit run could ever reserve its memory"
        );
        assert!(Config::default().check_run_memory().is_ok());
    }

    #[test]
    fn a_wait_ends_at_the_nearer_of_budget_and_deadline() {
        let near = Instant::now() + Duration::from_millis(10);
        let opts = RunOptions {
            deadline: Some(near),
            ..Default::default()
        };
        assert_eq!(opts.deadline_within(Duration::from_secs(30)), near);
        let far = RunOptions {
            deadline: Some(Instant::now() + Duration::from_secs(3600)),
            ..Default::default()
        };
        assert!(far.deadline_within(Duration::from_secs(1)) < far.deadline.unwrap());
    }

    #[test]
    fn the_outcome_follows_the_kind_not_the_words() {
        let deadline: Result<(), RunError> =
            Err(RunError::deadline("run failed", Duration::from_secs(30)));
        assert_eq!(
            deadline.as_ref().unwrap_err().to_string(),
            "run failed: module exceeded its execution deadline (30s)"
        );
        assert_eq!(metrics::run_outcome(&deadline), metrics::OUTCOME_TIMEOUT);
        let said: Result<(), RunError> = Err(RunError::new(
            RunFailure::Returned,
            "run returned an error: module exceeded its execution deadline (30s)",
        ));
        assert_eq!(metrics::run_outcome(&said), metrics::OUTCOME_ERROR);
        assert_eq!(
            metrics::run_outcome(&Ok::<(), RunError>(())),
            metrics::OUTCOME_OK
        );
        let host = RunError::from(Error("cannot create the private /tmp: denied".into()));
        assert_eq!(
            (host.kind(), host.message()),
            (RunFailure::Host, "cannot create the private /tmp: denied")
        );
    }
}
