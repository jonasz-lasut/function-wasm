//! The module cache - the Rust port of the Go engine's Cache: compiled
//! modules by content digest from three tiers. Memory holds hot modules
//! (dropped after an idle TTL, past a max-entries bound, or disabled), the
//! on-disk store of wasmtime artifacts survives restarts and maps in
//! milliseconds, and fetch + compile (seconds for a large guest) writes its
//! result back to disk. A load for one digest runs once even under
//! concurrent requests, and at most max_concurrent_compiles compile at a
//! time; a failed load is not cached, so the next request retries.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use function_wasm_engine::{Engine, Module};
use tokio::sync::{Mutex as AsyncMutex, OnceCell, Semaphore};

use crate::store::Store;

/// How long a compiled module stays in memory after its last use before the
/// next request for it goes back to the on-disk artifact.
pub const DEFAULT_IDLE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Default)]
pub struct CacheOptions {
    /// The store of wasmtime artifacts; None keeps compiled modules in
    /// memory only (tests).
    pub disk: Option<Arc<Store>>,
    /// How long a compiled module stays in memory after its last use;
    /// None means DEFAULT_IDLE_TTL.
    pub idle_ttl: Option<Duration>,
    /// Disables the memory tier: every request maps the artifact from disk
    /// (milliseconds) and releases it afterwards.
    pub no_memory: bool,
    /// Most compiled modules kept in memory at once; the least recently
    /// used is dropped beyond it. 0 leaves it to the idle TTL alone.
    pub max_entries: usize,
    /// Most modules compiling at once - a compile uses every core and
    /// about a gigabyte for a large guest. 0 means 1.
    pub max_concurrent_compiles: usize,
}

pub struct ModuleCache {
    engine: Arc<Engine>,
    disk: Option<Arc<Store>>,
    ttl: Duration,
    no_memory: bool,
    max_entries: usize,
    compiles: Semaphore,
    entries: Mutex<HashMap<String, Entry>>,
    loading: AsyncMutex<HashMap<String, Arc<OnceCell<Module>>>>,
}

struct Entry {
    module: Module,
    last_used: Instant,
}

impl ModuleCache {
    pub fn new(engine: Arc<Engine>, o: CacheOptions) -> Self {
        ModuleCache {
            engine,
            disk: o.disk,
            ttl: o.idle_ttl.unwrap_or(DEFAULT_IDLE_TTL),
            no_memory: o.no_memory,
            max_entries: o.max_entries,
            compiles: Semaphore::new(o.max_concurrent_compiles.max(1)),
            entries: Mutex::new(HashMap::new()),
            loading: AsyncMutex::new(HashMap::new()),
        }
    }

    /// Returns the compiled module for digest, calling fetch for its bytes
    /// only when neither memory nor disk has it.
    pub async fn get<F>(&self, digest: &str, fetch: F) -> Result<Module, String>
    where
        F: FnOnce() -> Result<Vec<u8>, String> + Send + 'static,
    {
        use function_wasm_engine::metrics::{self, CACHE_EVENTS};
        if let Some(m) = self.memory_get(digest) {
            CACHE_EVENTS
                .with_label_values(&[metrics::CACHE_COMPILED, metrics::EVENT_HIT])
                .inc();
            return Ok(m);
        }
        CACHE_EVENTS
            .with_label_values(&[metrics::CACHE_COMPILED, metrics::EVENT_MISS])
            .inc();
        let cell = {
            let mut loading = self.loading.lock().await;
            Arc::clone(loading.entry(digest.to_string()).or_default())
        };
        let result = cell
            .get_or_try_init(|| self.load(digest, fetch))
            .await
            .cloned();
        // The load is settled either way: drop the flight so a failure is
        // retried by the next request, and promote a success to memory.
        self.loading.lock().await.remove(digest);
        if let Ok(m) = &result {
            self.memory_put(digest, m.clone());
        }
        result
    }

    async fn load<F>(&self, digest: &str, fetch: F) -> Result<Module, String>
    where
        F: FnOnce() -> Result<Vec<u8>, String> + Send + 'static,
    {
        // The artifact on disk: mapped, not compiled. A stale or foreign
        // artifact is a miss - wasmtime refuses it and the module recompiles.
        use function_wasm_engine::metrics::{self, CACHE_EVENTS};
        if let Some(disk) = &self.disk {
            if let Some(path) = disk.path(digest) {
                let engine = Arc::clone(&self.engine);
                let loaded =
                    tokio::task::spawn_blocking(move || engine.deserialize_file(&path)).await;
                if let Ok(Ok(m)) = loaded {
                    CACHE_EVENTS
                        .with_label_values(&[metrics::CACHE_COMPILED_DISK, metrics::EVENT_HIT])
                        .inc();
                    return Ok(m);
                }
                // The artifact was there but wasmtime refused it: a miss
                // that cost a read.
                CACHE_EVENTS
                    .with_label_values(&[metrics::CACHE_COMPILED_DISK, metrics::EVENT_STALE])
                    .inc();
            } else {
                CACHE_EVENTS
                    .with_label_values(&[metrics::CACHE_COMPILED_DISK, metrics::EVENT_MISS])
                    .inc();
            }
        }
        // Fetch and compile, at most max_concurrent_compiles at a time;
        // further loads wait their turn, and their requesters with them.
        let _permit = self
            .compiles
            .acquire()
            .await
            .expect("the semaphore is never closed");
        let engine = Arc::clone(&self.engine);
        let disk = self.disk.clone();
        let digest = digest.to_string();
        tokio::task::spawn_blocking(move || {
            let wasm = fetch()?;
            let m = engine.compile(&wasm).map_err(|e| e.to_string())?;
            if let Some(disk) = disk {
                // Best effort: a full or read-only store only costs the next
                // process the compile.
                if let Ok(artifact) = engine.serialize(&m) {
                    let _ = disk.put(&digest, &artifact);
                }
            }
            Ok(m)
        })
        .await
        .map_err(|e| format!("internal error while loading the module: {e}"))?
    }

    fn memory_get(&self, digest: &str) -> Option<Module> {
        if self.no_memory {
            return None;
        }
        let mut entries = self.entries.lock().expect("poisoned");
        let now = Instant::now();
        entries.retain(|_, e| now.duration_since(e.last_used) < self.ttl);
        let e = entries.get_mut(digest)?;
        e.last_used = now;
        Some(e.module.clone())
    }

    fn memory_put(&self, digest: &str, module: Module) {
        if self.no_memory {
            return;
        }
        let mut entries = self.entries.lock().expect("poisoned");
        entries.insert(
            digest.to_string(),
            Entry {
                module,
                last_used: Instant::now(),
            },
        );
        if self.max_entries > 0 {
            while entries.len() > self.max_entries {
                let oldest = entries
                    .iter()
                    .min_by_key(|(_, e)| e.last_used)
                    .map(|(k, _)| k.clone())
                    .expect("non-empty");
                entries.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use function_wasm_engine::Config;

    const WAT: &str = r#"(module (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#;

    fn engine() -> Arc<Engine> {
        Arc::new(Engine::new(Config::default()).expect("engine"))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn serves_from_the_artifact_store_across_caches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = Arc::new(Store::open_dir(dir.path(), false).expect("store"));
        let engine = engine();
        let wasm = wat::parse_str(WAT).expect("wat");

        let first = ModuleCache::new(
            Arc::clone(&engine),
            CacheOptions {
                disk: Some(Arc::clone(&disk)),
                ..Default::default()
            },
        );
        first.get("sha256:m", move || Ok(wasm)).await.expect("load");

        // A fresh cache over the same store never fetches: the artifact is
        // mapped from disk.
        let second = ModuleCache::new(
            Arc::clone(&engine),
            CacheOptions {
                disk: Some(disk),
                ..Default::default()
            },
        );
        second
            .get("sha256:m", || {
                panic!("the artifact tier should have served this")
            })
            .await
            .expect("load from artifact");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_expired_memory_entry_goes_back_to_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = Arc::new(Store::open_dir(dir.path(), false).expect("store"));
        let cache = ModuleCache::new(
            engine(),
            CacheOptions {
                disk: Some(disk),
                idle_ttl: Some(Duration::ZERO),
                ..Default::default()
            },
        );
        let wasm = wat::parse_str(WAT).expect("wat");
        cache.get("sha256:m", move || Ok(wasm)).await.expect("load");
        cache
            .get("sha256:m", || {
                panic!("the artifact tier should have served this")
            })
            .await
            .expect("reload");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_load_is_not_cached() {
        let cache = ModuleCache::new(engine(), CacheOptions::default());
        let err = cache
            .get("sha256:m", || Err("cannot fetch module: boom".to_string()))
            .await
            .expect_err("fail");
        assert_eq!(err, "cannot fetch module: boom");
        let wasm = wat::parse_str(WAT).expect("wat");
        cache
            .get("sha256:m", move || Ok(wasm))
            .await
            .expect("retry succeeds");
    }
}
