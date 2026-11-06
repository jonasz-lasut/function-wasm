//! The engine behind the runtime, behind the `Engine` trait of
//! function-wasm-engine-api: what `runner.rs` compiles, inspects and runs
//! modules through, what `cache.rs` keeps and what `validate.rs` reads. A
//! Cargo feature selects the one engine a package serves with (`wasm`:
//! Wasmtime, in `function-wasm-engine`; the OCI runtime of
//! docs/one-pager-oci-runtime.md adds `oci`), and the rest of the crate
//! names it only as `Selected`, so a second engine is a second feature and
//! no other change here.

pub use function_wasm_engine_api::{Config, Engine, Error, Inspection, RunOptions};

#[cfg(feature = "wasm")]
pub use function_wasm_engine::Wasmtime as Selected;

#[cfg(not(feature = "wasm"))]
compile_error!("function-wasm serves with one engine: build with the `wasm` feature");

/// A compiled module of the selected engine.
pub type Module = <Selected as Engine>::Module;
