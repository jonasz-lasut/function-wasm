//! The Rust runtime of function-wasm: a Crossplane composition function that
//! runs a user-supplied WebAssembly module in a wasmtime sandbox. This is
//! the initial implementation (docs/one-pager-abi-v2.md, phase 1); the flags
//! it carries are the subset of the Go runtime's it serves, with the same
//! names, defaults and units. Serving is the default; `function validate`
//! runs the same admission over Compositions offline.

mod admission;
mod authz;
mod cache;
mod egress_rules;
mod from;
mod input;
mod location;
mod manifest;
mod quantity;
mod resolver;
mod runner;
mod sandboxenv;
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

    if args.cosign_key.is_some() {
        eprintln!("--cosign-key is not implemented yet in the Rust runtime");
        std::process::exit(1);
    }
    // The operator policy compiles once at startup and is immutable for the
    // process; a malformed policy or SSRF CIDR rule stops the runtime here
    // rather than compiling into a table that means less than written.
    let policy = match &args.sandbox_policy_file {
        None => None,
        Some(path) => match authz::OperatorPolicy::load(path).and_then(|p| {
            p.compile_ip_rules()?;
            Ok(p)
        }) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
    };
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

    let engine = Engine::new(Config {
        timeout: args.module_timeout,
        memory_limit: args.module_memory_limit << 20,
    })
    .expect("cannot create the wasmtime engine");
    let engine = Arc::new(engine);

    let function = runner::WasmFunction {
        engine: Arc::clone(&engine),
        cache: cache::ModuleCache::new(Arc::clone(&engine)),
        resolver: Arc::new(resolver::Resolver::new(
            args.module_dir,
            args.max_module_size << 20,
        )),
        ttl: function_sdk_rust::response::DEFAULT_TTL,
        policy,
    };
    serve(function, &args.sdk).await
}
