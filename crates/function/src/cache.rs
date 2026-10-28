//! The module cache: compiled
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
    /// Wall-clock bound on a cold load, the fetch and compile of a module
    /// no tier holds; None leaves it unbounded. Past it the request fails
    /// while the compile runs to its end on its thread, keeps its compile
    /// slot until then and stores its artifact for the next request.
    pub compile_timeout: Option<Duration>,
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
    compiles: Arc<Semaphore>,
    compile_timeout: Option<Duration>,
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
            compiles: Arc::new(Semaphore::new(o.max_concurrent_compiles.max(1))),
            compile_timeout: o.compile_timeout,
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
        // artifact is a miss - the store refuses one whose MAC isn't this
        // process's, wasmtime refuses one it can't load - and the module
        // recompiles.
        use function_wasm_engine::metrics::{self, CACHE_EVENTS};
        if let Some(disk) = &self.disk {
            match disk.path(digest) {
                crate::store::Lookup::Hit(path) => {
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
                }
                crate::store::Lookup::Stale => {
                    CACHE_EVENTS
                        .with_label_values(&[metrics::CACHE_COMPILED_DISK, metrics::EVENT_STALE])
                        .inc();
                }
                crate::store::Lookup::Miss => {
                    CACHE_EVENTS
                        .with_label_values(&[metrics::CACHE_COMPILED_DISK, metrics::EVENT_MISS])
                        .inc();
                }
            }
        }
        // Fetch and compile, at most max_concurrent_compiles at a time;
        // further loads wait their turn, and their requesters with them.
        // The permit travels into the blocking closure: a load cut short by
        // the bound keeps its slot until the compile ends, so the bound
        // never admits a second runaway compile beside the first.
        let permit = Arc::clone(&self.compiles)
            .acquire_owned()
            .await
            .expect("the semaphore is never closed");
        let engine = Arc::clone(&self.engine);
        let disk = self.disk.clone();
        let key = digest.to_string();
        let load = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let wasm = fetch()?;
            let m = engine.compile(&wasm).map_err(|e| e.to_string())?;
            if let Some(disk) = disk {
                // Best effort: a full or read-only store only costs the next
                // process the compile.
                if let Ok(artifact) = engine.serialize(&m) {
                    let _ = disk.put(&key, &artifact);
                }
            }
            Ok::<Module, String>(m)
        });
        let joined = match self.compile_timeout {
            // Wasmtime cannot cancel a compile in progress: past the bound the
            // request fails and the thread finishes on its own, writing its
            // artifact for the next request (the 1.0 audit's F-4).
            Some(bound) => match tokio::time::timeout(bound, load).await {
                Ok(joined) => joined,
                Err(_) => {
                    CACHE_EVENTS
                        .with_label_values(&[metrics::CACHE_COMPILED, metrics::EVENT_TIMEOUT])
                        .inc();
                    return Err(format!(
                        "compile exceeded --module-compile-timeout {}",
                        function_wasm_engine::duration::format(bound)
                    ));
                }
            },
            None => load.await,
        };
        let m = joined.map_err(|e| format!("internal error while loading the module: {e}"))??;
        Ok(m)
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

    fn engine() -> Arc<Engine> {
        Arc::new(Engine::new(Config::default()).expect("engine"))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn serves_from_the_artifact_store_across_caches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = Arc::new(Store::open_dir(dir.path(), false).expect("store"));
        let engine = engine();
        let wasm = crate::testing::guests::tiny_component();

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
    async fn a_planted_artifact_is_refused_by_a_keyed_store() {
        // The 1.0 audit's F-2 reproduction, made permanent: an artifact
        // placed under a digest by something other than this process is a
        // stale miss and a compile, and the compile's artifact then serves
        // a second cache under the same key without a fetch.
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine();
        let planted = engine
            .serialize(
                &engine
                    .compile(&crate::testing::guests::tiny_component())
                    .expect("compile"),
            )
            .expect("serialize");
        std::fs::write(dir.path().join("sha256-m"), &planted).expect("plant");
        let key = [9u8; 32];
        let keyed = || {
            Arc::new(
                Store::open_dir(dir.path(), false)
                    .expect("store")
                    .with_key(key),
            )
        };
        let cache = ModuleCache::new(
            Arc::clone(&engine),
            CacheOptions {
                disk: Some(keyed()),
                ..Default::default()
            },
        );
        let fetched = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&fetched);
        let wasm = crate::testing::guests::tiny_component();
        cache
            .get("sha256:m", move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(wasm)
            })
            .await
            .expect("load");
        assert!(
            fetched.load(std::sync::atomic::Ordering::SeqCst),
            "the planted artifact must not be served: the module is fetched and compiled"
        );
        let stale = function_wasm_engine::metrics::sample(
            "function_wasm_module_cache_events_total",
            &[("cache", "compiled-disk"), ("event", "stale")],
        )
        .unwrap_or(0.0);
        assert!(stale >= 1.0, "the refusal should be counted as stale");

        let second = ModuleCache::new(
            engine,
            CacheOptions {
                disk: Some(keyed()),
                ..Default::default()
            },
        );
        second
            .get("sha256:m", || {
                panic!("the artifact this key wrote should be served")
            })
            .await
            .expect("load from the keyed artifact");
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
        let wasm = crate::testing::guests::tiny_component();
        cache.get("sha256:m", move || Ok(wasm)).await.expect("load");
        cache
            .get("sha256:m", || {
                panic!("the artifact tier should have served this")
            })
            .await
            .expect("reload");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_load_is_cut_at_the_compile_timeout() {
        // The fetch stands in for a slow compile: both run on the load's
        // thread under the bound. The late load keeps the one compile slot
        // until it ends, so the retry queues behind it and then succeeds.
        let cache = ModuleCache::new(
            engine(),
            CacheOptions {
                compile_timeout: Some(Duration::from_millis(50)),
                ..Default::default()
            },
        );
        let wasm = crate::testing::guests::tiny_component();
        let slow = wasm.clone();
        let err = cache
            .get("sha256:m", move || {
                std::thread::sleep(Duration::from_millis(300));
                Ok(slow)
            })
            .await
            .expect_err("cut short");
        assert_eq!(err, "compile exceeded --module-compile-timeout 50ms");
        let timeouts = function_wasm_engine::metrics::sample(
            "function_wasm_module_cache_events_total",
            &[("cache", "compiled"), ("event", "timeout")],
        )
        .unwrap_or(0.0);
        assert!(timeouts >= 1.0, "the timeout should be counted");
        cache
            .get("sha256:m", move || Ok(wasm))
            .await
            .expect("a retry after the slow load succeeds");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_load_is_not_cached() {
        let cache = ModuleCache::new(engine(), CacheOptions::default());
        let err = cache
            .get("sha256:m", || Err("cannot fetch module: boom".to_string()))
            .await
            .expect_err("fail");
        assert_eq!(err, "cannot fetch module: boom");
        let wasm = crate::testing::guests::tiny_component();
        cache
            .get("sha256:m", move || Ok(wasm))
            .await
            .expect("retry succeeds");
    }
}
