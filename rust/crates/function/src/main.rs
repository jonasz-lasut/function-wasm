//! The Rust runtime of function-wasm: a Crossplane composition function that
//! runs a user-supplied WebAssembly module in a wasmtime sandbox. This is
//! the initial implementation (docs/one-pager-abi-v2.md, phase 1); the flags
//! it carries are the subset of the Go runtime's it serves, with the same
//! names, defaults and units. Serving is the default; `function validate`
//! runs the same admission over Compositions offline.

mod admission;
mod cache;
mod input;
mod quantity;
mod resolver;
mod runner;
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
    };
    serve(function, &args.sdk).await
}
