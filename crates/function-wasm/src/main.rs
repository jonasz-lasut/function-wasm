//! The function-wasm package's binary: the guestfn runtime over the
//! Wasmtime engine.

use std::process::ExitCode;

use guestfn_engine_wasm::Wasmtime;

/// The Wasmtime engine takes nothing beyond the ceilings.
#[derive(clap::Args, Debug)]
struct EngineArgs {}

fn main() -> ExitCode {
    guestfn_runtime::cli::run(
        "A Crossplane composition function that runs WebAssembly modules.",
        |config, _: &EngineArgs| Wasmtime::new(config),
    )
}
