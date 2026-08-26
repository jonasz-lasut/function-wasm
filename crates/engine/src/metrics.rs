//! The Prometheus metrics of the runtime - the Rust port of Go's
//! internal/metrics: same names, labels, buckets and help strings, on the
//! crate's default registry, served by the runtime's /metrics endpoint.
//!
//! None of the metrics carries a module identity label: a Function serves an
//! unbounded set of modules and digests, and per-module series would grow
//! without bound. Logs carry the digest.

use std::sync::LazyLock;

use prometheus::{
    CounterVec, Gauge, GaugeVec, Histogram, HistogramOpts, HistogramVec, Opts,
    register_counter_vec, register_gauge, register_gauge_vec, register_histogram,
    register_histogram_vec,
};

const NAMESPACE: &str = "function_wasm";
const SUBSYSTEM: &str = "module";

/// The in-memory tier of compiled modules.
pub const CACHE_COMPILED: &str = "compiled";
/// The on-disk store of wasmtime artifacts.
pub const CACHE_COMPILED_DISK: &str = "compiled-disk";
/// The on-disk store of fetched modules.
pub const CACHE_BLOB: &str = "blob";
pub const EVENT_HIT: &str = "hit";
pub const EVENT_MISS: &str = "miss";
/// A compiled-disk lookup that found an artifact wasmtime refused (another
/// version, a corrupt file) - a miss that cost a read.
pub const EVENT_STALE: &str = "stale";

pub const OUTCOME_OK: &str = "ok";
pub const OUTCOME_ERROR: &str = "error";
pub const OUTCOME_TIMEOUT: &str = "timeout";

/// How long compiling a module took, on cache misses.
pub static COMPILE_DURATION: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        HistogramOpts::new(
            "compile_duration_seconds",
            "Time spent compiling a module with wasmtime (compiled-module cache misses only).",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM)
        .buckets(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0])
    )
    .expect("register")
});

/// How long fetching module bytes took, by source kind.
pub static FETCH_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        HistogramOpts::new(
            "fetch_duration_seconds",
            "Time spent fetching and verifying module bytes, by source (oci, http, path); blob-cache hits included.",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM)
        .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]),
        &["source"]
    )
    .expect("register")
});

/// How long one guest run took, by outcome.
pub static RUN_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        HistogramOpts::new(
            "run_duration_seconds",
            "Time spent instantiating and running a module for one request, by outcome (ok, error, timeout).",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM)
        .buckets(vec![
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0
        ]),
        &["outcome"]
    )
    .expect("register")
});

/// How much of a run was spent inside host imports. Additive to the Go
/// runtime's series (which had no such split); run_duration minus this is
/// guest compute.
pub static HOSTCALL_DURATION: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        HistogramOpts::new(
            "hostcall_duration_seconds",
            "Time one run spent inside host imports (wasmfn.log, wasmfn.http and WASI); the rest of run_duration_seconds is guest compute.",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM)
        .buckets(vec![
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0
        ])
    )
    .expect("register")
});

/// How many guest runs are executing right now - pinned at
/// --max-concurrent-runs, it says the bound is what requests wait on.
pub static RUNS_IN_FLIGHT: LazyLock<Gauge> = LazyLock::new(|| {
    register_gauge!(
        Opts::new(
            "runs_in_flight",
            "Module runs executing at this moment (a run holding a slot when --max-concurrent-runs bounds them).",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM)
    )
    .expect("register")
});

/// Hits and misses of the compiled-module and blob caches.
pub static CACHE_EVENTS: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        Opts::new(
            "cache_events_total",
            "Cache lookups by cache (compiled = in-memory modules, compiled-disk = wasmtime artifacts on disk, blob = fetched modules on disk) and event (hit, miss, stale).",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM),
        &["cache", "event"]
    )
    .expect("register")
});

/// The HTTP requests modules made through the host.
pub static HTTP_REQUESTS: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        Opts::new(
            "http_requests_total",
            "HTTP requests modules made through the host (wasmfn.http), by outcome (ok = the server answered, refused = outside the grant or the egress policy, budget = over a per-run budget or the request timeout, error = the request failed).",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM),
        &["outcome"]
    )
    .expect("register")
});

/// Memory growths the run limiter denied.
pub static MEMORY_DENIALS: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        Opts::new(
            "memory_denials_total",
            "Guest memory growths denied, by reason (limit = the run's memory ceiling, pool = --max-total-run-memory exhausted before the run's deadline). The guest sees memory.grow fail.",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM),
        &["reason"]
    )
    .expect("register")
});

/// Requests by outcome.
pub static REQUESTS: LazyLock<CounterVec> = LazyLock::new(|| {
    register_counter_vec!(
        Opts::new(
            "requests_total",
            "Requests by outcome: ok, refused (declined before the module ran), error (load or run failed).",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM),
        &["outcome"]
    )
    .expect("register")
});

/// The size of each on-disk store, as of the last sweep.
pub static CACHE_BYTES: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        Opts::new(
            "cache_bytes",
            "Bytes held by each on-disk store (compiled-disk = wasmtime artifacts, blob = fetched modules), measured by the periodic sweep.",
        )
        .namespace(NAMESPACE)
        .subsystem(SUBSYSTEM),
        &["cache"]
    )
    .expect("register")
});

/// The default registry rendered in Prometheus text exposition format - what
/// the /metrics endpoint serves.
pub fn render() -> String {
    let encoder = prometheus::TextEncoder::new();
    encoder
        .encode_to_string(&prometheus::gather())
        .unwrap_or_default()
}

/// Reads one series from the default registry: the counter or gauge value or
/// the histogram sample count for name with exactly the given labels. It
/// exists for tests, which assert the wiring rather than the values.
pub fn sample(name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    for family in prometheus::gather() {
        if family.name() != name {
            continue;
        }
        for m in family.get_metric() {
            let got = m.get_label();
            if got.len() != labels.len() {
                continue;
            }
            if !labels
                .iter()
                .all(|(k, v)| got.iter().any(|l| l.name() == *k && l.value() == *v))
            {
                continue;
            }
            let histogram = m.get_histogram();
            if histogram.has_sample_count() {
                return Some(histogram.get_sample_count() as f64);
            }
            let counter = m.get_counter();
            if counter.has_value() {
                return Some(counter.value());
            }
            let gauge = m.get_gauge();
            if gauge.has_value() {
                return Some(gauge.value());
            }
        }
    }
    None
}
