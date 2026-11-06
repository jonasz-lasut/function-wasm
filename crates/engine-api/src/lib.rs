//! What every engine of function-wasm shares, and what the runtime holds
//! an engine to: the `Engine` trait (compile, inspect, run, version), the
//! ceilings (`Config`) and one run's narrowing and grants (`RunOptions`),
//! the inspection a tool reads, the egress seam (`wire::HttpRequester`),
//! the run slots and the memory pool (`concurrency`), the Go-style
//! durations and the Prometheus series (`metrics`). Nothing here knows
//! WebAssembly: `function-wasm-engine` implements the trait over Wasmtime,
//! and the OCI runtime (docs/one-pager-oci-runtime.md) over a microVM.

pub mod concurrency;
pub mod duration;
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

/// An engine failure, whose message wording is pinned: the
/// message is the contract (it ends up in an XR condition), not a type.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub String);

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
pub trait Engine: Send + Sync + 'static {
    /// A compiled module: what `compile` makes and `run` takes, cheap to
    /// clone and shared between runs.
    type Module: Clone + Send + Sync + std::fmt::Debug + 'static;

    /// The module type this engine serves, as the manifest's `type` names
    /// it: `wasm` or `oci`. The runtime fetches, admits and describes
    /// modules by it, and refuses the other type's by name.
    const KIND: &'static str;

    /// The contract the engine implements, as a tool prints it beside a
    /// module's size: `ABI v2` for the component world, `process contract
    /// v1` for an image.
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

    /// Runs one request through a module under the run's options and
    /// returns the response bytes; every failure is an `Error` whose
    /// message is the fatal result's.
    fn run(&self, m: &Self::Module, request: &[u8], opts: RunOptions) -> Result<Vec<u8>, Error>;

    /// What namespaces the compiled store on disk: an artifact is loaded
    /// only by the engine version that wrote it.
    fn version() -> String
    where
        Self: Sized;
}
