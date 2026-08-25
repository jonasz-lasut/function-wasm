//! The in-memory tier of the module cache: compiled modules by content
//! digest, loads single-flighted so concurrent first requests compile once.
//! The Go runtime's disk tiers (fetched blobs, serialized artifacts), idle
//! TTL and LRU bound are not ported yet; entries live for the process.

use std::collections::HashMap;
use std::sync::Arc;

use function_wasm_engine::{Engine, Module};
use tokio::sync::{Mutex, OnceCell};

pub struct ModuleCache {
    engine: Arc<Engine>,
    cells: Mutex<HashMap<String, Arc<OnceCell<Module>>>>,
}

impl ModuleCache {
    pub fn new(engine: Arc<Engine>) -> Self {
        ModuleCache {
            engine,
            cells: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the compiled module for digest, fetching and compiling it on
    /// the first request. Concurrent misses share one compile; a failed load
    /// is not cached, so the next request retries.
    pub async fn get<F>(&self, digest: &str, fetch: F) -> Result<Module, String>
    where
        F: FnOnce() -> Result<Vec<u8>, String> + Send + 'static,
    {
        let cell = {
            let mut cells = self.cells.lock().await;
            Arc::clone(cells.entry(digest.to_string()).or_default())
        };
        let engine = Arc::clone(&self.engine);
        cell.get_or_try_init(|| async move {
            // Fetch and compile are blocking (file reads, wasmtime codegen);
            // they run off the async executor.
            tokio::task::spawn_blocking(move || {
                let wasm = fetch()?;
                engine.compile(&wasm).map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| format!("internal error while loading the module: {e}"))?
        })
        .await
        .cloned()
    }
}
