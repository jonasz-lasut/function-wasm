//! The Wasmtime engine of function-wasm, behind guestfn-engine's `Engine`
//! trait: `Wasmtime` compiles a component, checks it against the world and
//! runs it in a fresh store per request. The only crate in the workspace
//! that imports wasmtime. `componentize` is `guestfn build`'s wrap of a
//! core module into a component, kept here so the adapter it links is the
//! one this crate's wasmtime-wasi was released with.

mod component;
pub mod componentize;
mod run;
mod sandbox;
#[cfg(any(test, feature = "testutil"))]
pub mod testing;
mod wasihttp;
mod wasm;

pub use wasm::{ABI_V2_WORLD, CORE_MODULE_REFUSAL, Module, Wasmtime, version};
pub(crate) use wasm::{ARGV0, CallState, EPOCH_TICK, HostTimer, PoolHold, RunLimiter, first_line};

pub(crate) use guestfn_engine::{
    Config, Engine, Error, Extern, Inspection, RunOptions, concurrency, duration, metrics, wire,
};
