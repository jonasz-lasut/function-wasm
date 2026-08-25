//! The Rust runtime of function-wasm: a Crossplane composition function that
//! runs a user-supplied WebAssembly module in a wasmtime sandbox. This is
//! the initial implementation (docs/one-pager-abi-v2.md, phase 1); the flags
//! it carries are the subset of the Go runtime's it serves, with the same
//! names, defaults and units. Serving is the default; `function validate`
//! runs the same admission over Compositions offline.

mod admission;
mod authz;
mod cache;
mod cosign;
mod egress;
mod egress_rules;
mod from;
mod input;
mod location;
mod manifest;
mod oci;
mod ops;
mod quantity;
mod resolver;
mod runner;
mod sandboxenv;
mod store;
mod validate;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use function_sdk_rust::{Args, logging, serve};
use function_wasm_engine::{Config, Engine, duration};

#[derive(Parser, Debug)]
#[command(
    name = "function",
    version,
    about = "A Crossplane composition function that runs WebAssembly modules (Rust runtime)"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Validate the function-wasm Inputs of Compositions against these
    /// flags, offline: the checks a request passes before its module is
    /// resolved, in the runtime's own words.
    Validate(validate::ValidateArgs),
}

#[derive(clap::Args, Debug)]
struct ServeArgs {
    #[command(flatten)]
    sdk: Args,

    /// Directory served for 'path' module sources; unset refuses them.
    #[arg(long, env = "MODULE_DIR")]
    module_dir: Option<PathBuf>,

    /// Maximum size of a module in MB.
    #[arg(long, default_value_t = 128)]
    max_module_size: u64,

    /// Maximum wall-clock time one module run may take.
    #[arg(long, default_value = "30s", value_parser = duration::parse)]
    module_timeout: Duration,

    /// Maximum linear memory of a running module in MB.
    #[arg(long, default_value_t = 512)]
    module_memory_limit: u64,

    /// Cedar document with the operator's grant policy - the sole authority
    /// that enables a sandbox capability, evaluated default-deny. Unset, no
    /// sandbox capability is grantable and the runtime offers only the
    /// default sandbox.
    #[arg(long, env = "SANDBOX_POLICY_FILE")]
    sandbox_policy_file: Option<PathBuf>,

    /// PEM file with one or more cosign public keys.
    #[arg(long, env = "COSIGN_KEY")]
    cosign_key: Option<PathBuf>,

    /// Keep compiled modules in memory between requests. Off, every request
    /// maps the module's compiled artifact from disk and releases it after.
    #[arg(long, default_value_t = true, env = "ENABLE_MEMORY_CACHE", action = clap::ArgAction::Set)]
    enable_memory_cache: bool,

    /// Most compiled modules kept in memory at once; the least recently
    /// used is dropped beyond it. 0 leaves it to the idle timeout.
    #[arg(long, default_value_t = 0, env = "MAX_CACHED_MODULES")]
    max_cached_modules: usize,

    /// Most modules compiled at once; further first requests wait their
    /// turn.
    #[arg(long, default_value_t = 1, env = "MAX_CONCURRENT_COMPILES")]
    max_concurrent_compiles: usize,

    /// Sustained egress requests per minute per module digest. 0 leaves
    /// egress unrated.
    #[arg(
        long,
        default_value_t = 0.0,
        env = "EGRESS_RATE_LIMIT_PER_MINUTE",
        allow_negative_numbers = true
    )]
    egress_rate_limit_per_minute: f64,

    /// Burst tokens for --egress-rate-limit-per-minute; 0 derives
    /// max(1, requestsPerMinute).
    #[arg(
        long,
        default_value_t = 0,
        env = "EGRESS_RATE_LIMIT_BURST",
        allow_negative_numbers = true
    )]
    egress_rate_limit_burst: i64,

    /// Most module runs executing at once; a further request waits for a
    /// slot until its deadline, then fails with a fatal result. 0 leaves
    /// concurrency to the caller.
    #[arg(long, default_value_t = 0, env = "MAX_CONCURRENT_RUNS")]
    max_concurrent_runs: usize,

    /// Total linear-memory budget in MB across all running modules; a run
    /// reserves its effective limit before it starts. 0 means no bound.
    #[arg(long, default_value_t = 0, env = "MAX_TOTAL_RUN_MEMORY")]
    max_total_run_memory: u64,

    /// Address of the plain-HTTP health endpoints /livez and /readyz (ready
    /// once the caches are open and --warm-modules are loaded); empty
    /// disables them.
    #[arg(long, default_value = ":8081", env = "HEALTH_ADDRESS")]
    health_address: String,

    /// Modules loaded before /readyz reports ready: OCI references pinned
    /// to their manifest digest and, with --module-dir, path:<file>
    /// entries. Repeatable or comma-separated. One that fails to load is
    /// logged and loaded on its first request instead.
    #[arg(long, env = "WARM_MODULES", value_delimiter = ',')]
    warm_modules: Vec<String>,

    /// Time to live for function responses the runtime itself produces
    /// (fatal results); a module sets the TTL of its own responses.
    #[arg(long, value_parser = duration::parse)]
    ttl: Option<Duration>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Validate(args)) => validate::run(&args),
        None => match serve_main(cli.serve) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
    }
}

#[tokio::main]
async fn serve_main(args: ServeArgs) -> Result<(), function_sdk_rust::Error> {
    logging::configure(args.sdk.debug);

    let verifier = match &args.cosign_key {
        None => None,
        Some(path) => match cosign::Verifier::load(path) {
            Ok(v) => Some(Arc::new(v)),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
    };
    if args.egress_rate_limit_per_minute < 0.0 || args.egress_rate_limit_burst < 0 {
        eprintln!(
            "--egress-rate-limit-per-minute and --egress-rate-limit-burst must not be negative"
        );
        std::process::exit(1);
    }
    // The operator policy compiles once at startup and is immutable for the
    // process; a malformed policy or SSRF CIDR rule stops the runtime here
    // rather than compiling into a table that means less than written.
    let mut ip_rules = authz::IpRules::default();
    let policy = match &args.sandbox_policy_file {
        None => None,
        Some(path) => match authz::OperatorPolicy::load(path).and_then(|p| {
            ip_rules = p.compile_ip_rules()?;
            Ok(p)
        }) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
    };
    let egress = Arc::new(egress::Egress::new(
        ip_rules,
        args.egress_rate_limit_per_minute,
        args.egress_rate_limit_burst,
    ));
    // A key without a policy is the all-or-nothing regime; a key beside a
    // policy with no requireSignature rule verifies nothing - warn loudly
    // rather than let --cosign-key lapse silently.
    if verifier.is_some()
        && let Some(p) = &policy
        && !p.has_signature_rules()
    {
        tracing::warn!(
            "--cosign-key is set but the operator policy has no requireSignature rule: no module will be signature-verified"
        );
    }
    // The $TMPDIR probe runs once before serving, only when the policy can
    // grant a private /tmp: a misconfigured $TMPDIR stops the runtime here,
    // not every request.
    if policy
        .as_ref()
        .is_some_and(authz::OperatorPolicy::has_private_tmp_rules)
    {
        match tempfile::Builder::new()
            .prefix("function-wasm-private-tmp-")
            .tempdir()
        {
            Ok(probe) => drop(probe),
            Err(e) => {
                eprintln!(
                    "the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot create one under {}: {e}",
                    std::env::temp_dir().display()
                );
                std::process::exit(1);
            }
        }
    }

    let engine = match Engine::new(Config {
        timeout: args.module_timeout,
        memory_limit: args.module_memory_limit << 20,
        max_concurrent_runs: args.max_concurrent_runs,
        max_total_run_memory: args.max_total_run_memory << 20,
    }) {
        Ok(engine) => engine,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let engine = Arc::new(engine);

    // The on-disk caches under the fixed directory: fetched blobs (for the
    // OCI/HTTP sources when they land), the compiled artifacts namespaced
    // by the engine version, and module manifests; a filesystem that cannot
    // hold them stops the runtime here (mount an emptyDir on a read-only
    // root filesystem).
    let cache_dir = std::path::Path::new(store::DEFAULT_DIR);
    let compiled_parent = cache_dir.join(store::COMPILED_DIR);
    let version = function_wasm_engine::version();
    store::remove_stale_versions(&compiled_parent, &version);
    let open = |dir: std::path::PathBuf, verify: bool| match store::Store::open_dir(dir, verify) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let compiled = open(compiled_parent.join(&version), false);
    let _modules = open(cache_dir.join(store::MODULES_DIR), true);
    let _manifests = open(cache_dir.join(store::MANIFESTS_DIR), false);
    let blobs = Arc::clone(&_modules);

    let resolver = Arc::new(resolver::Resolver::new(
        args.module_dir,
        args.max_module_size << 20,
        Some(blobs),
    ));
    let module_cache = Arc::new(cache::ModuleCache::new(
        Arc::clone(&engine),
        cache::CacheOptions {
            disk: Some(compiled),
            no_memory: !args.enable_memory_cache,
            max_entries: args.max_cached_modules,
            max_concurrent_compiles: args.max_concurrent_compiles,
            ..Default::default()
        },
    ));
    let step_slots = Arc::new(function_wasm_engine::concurrency::StepSlots::new());
    let function = runner::WasmFunction {
        engine: Arc::clone(&engine),
        cache: Arc::clone(&module_cache),
        resolver: Arc::clone(&resolver),
        ttl: args.ttl.unwrap_or(function_sdk_rust::response::DEFAULT_TTL),
        policy,
        egress,
        step_slots: Arc::clone(&step_slots),
        verifier,
    };
    {
        // The periodic sweep: idle per-step slot entries every ten minutes.
        let step_slots = Arc::clone(&step_slots);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(10 * 60));
            loop {
                tick.tick().await;
                step_slots.sweep_idle();
            }
        });
    }

    // The health endpoints answer while warm-up runs: a probe reads
    // not-ready rather than a refused connection, and an early request is
    // simply served cold or joins the load in flight.
    let readiness = ops::Readiness::default();
    if !args.health_address.is_empty() {
        let address = args.health_address.clone();
        let r = readiness.clone();
        tokio::spawn(async move {
            if let Err(e) = ops::serve_health(&address, r).await {
                tracing::error!(error = %e, "cannot serve health endpoints");
            }
        });
    }
    {
        // Warm-up shares the request path's cache, so a warmed module is
        // exactly what a request would have loaded; failures never hold
        // readiness back.
        let entries = args.warm_modules.clone();
        let readiness = readiness.clone();
        let resolver = Arc::clone(&resolver);
        let module_cache = Arc::clone(&module_cache);
        tokio::spawn(async move {
            ops::warm(&entries, &resolver, &module_cache).await;
            readiness.ready();
        });
    }
    serve(function, &args.sdk).await
}
