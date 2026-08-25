//! The Rust runtime of function-wasm: a Crossplane composition function that
//! runs a user-supplied WebAssembly module in a wasmtime sandbox. This is
//! the initial implementation (docs/one-pager-abi-v2.md, phase 1); the flags
//! it carries are the subset of the Go runtime's it serves, with the same
//! names, defaults and units.

mod admission;
mod cache;
mod input;
mod quantity;
mod resolver;
mod runner;

use std::path::PathBuf;
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

#[tokio::main]
async fn main() -> Result<(), function_sdk_rust::Error> {
    let cli = Cli::parse();
    logging::configure(cli.sdk.debug);

    let engine = Engine::new(Config {
        timeout: cli.module_timeout,
        memory_limit: cli.module_memory_limit << 20,
    })
    .expect("cannot create the wasmtime engine");
    let engine = Arc::new(engine);

    let function = runner::WasmFunction {
        engine: Arc::clone(&engine),
        cache: cache::ModuleCache::new(Arc::clone(&engine)),
        resolver: Arc::new(resolver::Resolver::new(
            cli.module_dir,
            cli.max_module_size << 20,
        )),
        ttl: function_sdk_rust::response::DEFAULT_TTL,
    };
    serve(function, &cli.sdk).await
}
