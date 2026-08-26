//! The metrics of the runtime - the same series as the Go runtime's
//! internal/metrics (names, labels, buckets and help strings), registered
//! with prometheus-client, the OpenMetrics-native client: /metrics serves
//! OpenMetrics 1.0 as its main format straight from the encoder, and the
//! classic Prometheus text format is derived from it for scrapers that ask.
//! The Labeled* adapters keep the label-values call shape the prometheus
//! crate had, so the migration moved no call site.
//!
//! None of the metrics carries a module identity label: a Function serves an
//! unbounded set of modules and digests, and per-module series would grow
//! without bound. Logs carry the digest.

use std::fmt::Write as _;
use std::sync::atomic::AtomicU64;
use std::sync::{LazyLock, RwLock};

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::{Family, MetricConstructor};
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::{Metric, Registry};

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

/// The registry every series lives in - prometheus-client has no default
/// registry, so the runtime's is this one.
static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(|| RwLock::new(Registry::default()));

/// Registers a metric into the runtime's registry. A counter's name is
/// given without its `_total` suffix (the encoder appends it, per
/// OpenMetrics), and the help's trailing period is stripped because
/// prometheus-client appends one - so the served text stays byte-identical
/// to the Go runtime's help strings.
pub fn register(name: &str, help: &str, metric: impl Metric) {
    let help = help.strip_suffix('.').unwrap_or(help);
    REGISTRY
        .write()
        .expect("poisoned")
        .register(name, help, metric);
}

type Labels = Vec<(String, String)>;

/// A labeled counter with the prometheus crate's with_label_values call
/// shape.
pub struct LabeledCounter {
    keys: &'static [&'static str],
    family: Family<Labels, Counter>,
}

impl LabeledCounter {
    pub fn new(name: &str, help: &str, keys: &'static [&'static str]) -> Self {
        let family = Family::<Labels, Counter>::default();
        register(name, help, family.clone());
        LabeledCounter { keys, family }
    }

    pub fn with_label_values(&self, values: &[&str]) -> impl std::ops::Deref<Target = Counter> {
        self.family.get_or_create(&pair(self.keys, values))
    }
}

/// A labeled gauge with the prometheus crate's with_label_values call shape.
pub struct LabeledGauge {
    keys: &'static [&'static str],
    family: Family<Labels, Gauge<f64, AtomicU64>>,
}

impl LabeledGauge {
    pub fn new(name: &str, help: &str, keys: &'static [&'static str]) -> Self {
        let family = Family::<Labels, Gauge<f64, AtomicU64>>::default();
        register(name, help, family.clone());
        LabeledGauge { keys, family }
    }

    pub fn with_label_values(
        &self,
        values: &[&str],
    ) -> impl std::ops::Deref<Target = Gauge<f64, AtomicU64>> {
        self.family.get_or_create(&pair(self.keys, values))
    }
}

/// Builds a family's histograms with the series' fixed buckets.
#[derive(Clone)]
struct Buckets(&'static [f64]);

impl MetricConstructor<Histogram> for Buckets {
    fn new_metric(&self) -> Histogram {
        Histogram::new(self.0.iter().copied())
    }
}

/// A labeled histogram with the prometheus crate's with_label_values call
/// shape.
pub struct LabeledHistogram {
    keys: &'static [&'static str],
    family: Family<Labels, Histogram, Buckets>,
}

impl LabeledHistogram {
    fn new(name: &str, help: &str, keys: &'static [&'static str], buckets: &'static [f64]) -> Self {
        let family = Family::new_with_constructor(Buckets(buckets));
        register(name, help, family.clone());
        LabeledHistogram { keys, family }
    }

    pub fn with_label_values(&self, values: &[&str]) -> impl std::ops::Deref<Target = Histogram> {
        self.family.get_or_create(&pair(self.keys, values))
    }
}

fn pair(keys: &[&str], values: &[&str]) -> Labels {
    debug_assert_eq!(keys.len(), values.len());
    keys.iter()
        .zip(values)
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn plain_histogram(name: &str, help: &str, buckets: &'static [f64]) -> Histogram {
    let h = Histogram::new(buckets.iter().copied());
    register(name, help, h.clone());
    h
}

/// How long compiling a module took, on cache misses.
pub static COMPILE_DURATION: LazyLock<Histogram> = LazyLock::new(|| {
    plain_histogram(
        "function_wasm_module_compile_duration_seconds",
        "Time spent compiling a module with wasmtime (compiled-module cache misses only).",
        &[0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0],
    )
});

/// How long fetching module bytes took, by source kind.
pub static FETCH_DURATION: LazyLock<LabeledHistogram> = LazyLock::new(|| {
    LabeledHistogram::new(
        "function_wasm_module_fetch_duration_seconds",
        "Time spent fetching and verifying module bytes, by source (oci, http, path); blob-cache hits included.",
        &["source"],
        &[0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0],
    )
});

/// How long one guest run took, by outcome.
pub static RUN_DURATION: LazyLock<LabeledHistogram> = LazyLock::new(|| {
    LabeledHistogram::new(
        "function_wasm_module_run_duration_seconds",
        "Time spent instantiating and running a module for one request, by outcome (ok, error, timeout).",
        &["outcome"],
        &[
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
        ],
    )
});

/// How much of a run was spent inside host imports. Additive to the Go
/// runtime's series (which had no such split); run_duration minus this is
/// guest compute.
pub static HOSTCALL_DURATION: LazyLock<Histogram> = LazyLock::new(|| {
    plain_histogram(
        "function_wasm_module_hostcall_duration_seconds",
        "Time one run spent inside host imports (wasmfn.log, wasmfn.http and WASI); the rest of run_duration_seconds is guest compute.",
        &[
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
        ],
    )
});

/// How many guest runs are executing right now - pinned at
/// --max-concurrent-runs, it says the bound is what requests wait on.
pub static RUNS_IN_FLIGHT: LazyLock<Gauge<f64, AtomicU64>> = LazyLock::new(|| {
    let g = Gauge::<f64, AtomicU64>::default();
    register(
        "function_wasm_module_runs_in_flight",
        "Module runs executing at this moment (a run holding a slot when --max-concurrent-runs bounds them).",
        g.clone(),
    );
    g
});

/// Hits and misses of the compiled-module and blob caches.
pub static CACHE_EVENTS: LazyLock<LabeledCounter> = LazyLock::new(|| {
    LabeledCounter::new(
        "function_wasm_module_cache_events",
        "Cache lookups by cache (compiled = in-memory modules, compiled-disk = wasmtime artifacts on disk, blob = fetched modules on disk) and event (hit, miss, stale).",
        &["cache", "event"],
    )
});

/// The HTTP requests modules made through the host.
pub static HTTP_REQUESTS: LazyLock<LabeledCounter> = LazyLock::new(|| {
    LabeledCounter::new(
        "function_wasm_module_http_requests",
        "HTTP requests modules made through the host (wasmfn.http), by outcome (ok = the server answered, refused = outside the grant or the egress policy, budget = over a per-run budget or the request timeout, error = the request failed).",
        &["outcome"],
    )
});

/// Memory growths the run limiter denied.
pub static MEMORY_DENIALS: LazyLock<LabeledCounter> = LazyLock::new(|| {
    LabeledCounter::new(
        "function_wasm_module_memory_denials",
        "Guest memory growths denied, by reason (limit = the run's memory ceiling, pool = --max-total-run-memory exhausted before the run's deadline). The guest sees memory.grow fail.",
        &["reason"],
    )
});

/// Requests by outcome.
pub static REQUESTS: LazyLock<LabeledCounter> = LazyLock::new(|| {
    LabeledCounter::new(
        "function_wasm_module_requests",
        "Requests by outcome: ok, refused (declined before the module ran), error (load or run failed).",
        &["outcome"],
    )
});

/// The size of each on-disk store, as of the last sweep.
pub static CACHE_BYTES: LazyLock<LabeledGauge> = LazyLock::new(|| {
    LabeledGauge::new(
        "function_wasm_module_cache_bytes",
        "Bytes held by each on-disk store (compiled-disk = wasmtime artifacts, blob = fetched modules), measured by the periodic sweep.",
        &["cache"],
    )
});

/// The content type of render_openmetrics.
pub const OPENMETRICS_CONTENT_TYPE: &str =
    "application/openmetrics-text; version=1.0.0; charset=utf-8";

/// The registry rendered as OpenMetrics 1.0 text - the /metrics endpoint's
/// main format, straight from prometheus-client's own encoder.
pub fn render_openmetrics() -> String {
    let mut out = String::new();
    let _ =
        prometheus_client::encoding::text::encode(&mut out, &REGISTRY.read().expect("poisoned"));
    out
}

/// The registry rendered in the classic Prometheus text format
/// (`text/plain; version=0.0.4`), derived from the OpenMetrics rendering:
/// the two differ only in a counter family being named with its `_total`
/// suffix in the classic HELP and TYPE lines, and in the terminating
/// `# EOF` - the samples themselves are identical.
pub fn render() -> String {
    classic_from_openmetrics(&render_openmetrics())
}

fn classic_from_openmetrics(om: &str) -> String {
    let counters: std::collections::HashSet<&str> = om
        .lines()
        .filter_map(|l| l.strip_prefix("# TYPE ")?.strip_suffix(" counter"))
        .collect();
    let mut out = String::new();
    for line in om.lines() {
        if line == "# EOF" {
            continue;
        }
        if let Some(name) = line
            .strip_prefix("# TYPE ")
            .and_then(|rest| rest.strip_suffix(" counter"))
        {
            let _ = writeln!(out, "# TYPE {name}_total counter");
            continue;
        }
        if let Some((name, help)) = line
            .strip_prefix("# HELP ")
            .and_then(|rest| rest.split_once(' '))
            && counters.contains(name)
        {
            let _ = writeln!(out, "# HELP {name}_total {help}");
            continue;
        }
        let _ = writeln!(out, "{line}");
    }
    out
}

/// Reads one series from the rendered registry: the counter or gauge value
/// or the histogram sample count for name with exactly the given labels. It
/// exists for tests, which assert the wiring rather than the values.
pub fn sample(name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    let text = render_openmetrics();
    let histogram_count = format!("{name}_count");
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let Some(end) = line.find(['{', ' ']) else {
            continue;
        };
        let series = &line[..end];
        if series != name && series != histogram_count {
            continue;
        }
        let rest = &line[end..];
        let (rendered, value) = match rest.strip_prefix('{') {
            Some(labeled) => match labeled.split_once("} ") {
                Some(split) => split,
                None => continue,
            },
            None => ("", rest.trim_start_matches(' ')),
        };
        let pairs: Vec<(&str, &str)> = rendered
            .split(',')
            .filter(|p| !p.is_empty())
            .filter_map(|p| {
                let (k, v) = p.split_once("=\"")?;
                Some((k, v.strip_suffix('"')?))
            })
            .collect();
        if pairs.len() != labels.len()
            || !labels
                .iter()
                .all(|(k, v)| pairs.iter().any(|(pk, pv)| pk == k && pv == v))
        {
            continue;
        }
        return value.trim().parse().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The OpenMetrics rendering is prometheus-client's own; the classic
    /// rendering is derived from it and differs exactly where the formats
    /// do: counter family naming and the terminating EOF.
    #[test]
    fn openmetrics_rendering_matches_the_spec_shape() {
        // Touch one of each type so the families exist.
        CACHE_EVENTS
            .with_label_values(&[CACHE_BLOB, EVENT_HIT])
            .inc();
        RUNS_IN_FLIGHT.set(0.0);
        COMPILE_DURATION.observe(0.1);

        let om = render_openmetrics();
        assert!(om.ends_with("# EOF\n"), "OpenMetrics must end with EOF");
        assert!(
            om.contains("# TYPE function_wasm_module_cache_events counter"),
            "counter family named without _total:\n{om}"
        );
        assert!(
            om.contains("function_wasm_module_cache_events_total{cache=\"blob\",event=\"hit\"}"),
            "counter samples keep _total:\n{om}"
        );
        assert!(om.contains("# TYPE function_wasm_module_runs_in_flight gauge"));
        assert!(om.contains("# TYPE function_wasm_module_compile_duration_seconds histogram"));
        assert!(om.contains("function_wasm_module_compile_duration_seconds_bucket{le=\"+Inf\"}"));
        // The help strings are the Go runtime's, single trailing period
        // included (prometheus-client appends one; register strips ours).
        assert!(om.contains("bounds them)."), "help text:\n{om}");
        assert!(!om.contains("bounds them).."), "double period:\n{om}");

        let classic = render();
        assert!(classic.contains("# TYPE function_wasm_module_cache_events_total counter"));
        assert!(classic.contains("# HELP function_wasm_module_cache_events_total "));
        assert!(!classic.contains("# EOF"));
        assert!(
            classic
                .contains("function_wasm_module_cache_events_total{cache=\"blob\",event=\"hit\"}"),
            "samples identical in both renderings:\n{classic}"
        );
    }

    #[test]
    fn sample_reads_counters_gauges_and_histograms() {
        CACHE_EVENTS
            .with_label_values(&[CACHE_COMPILED, EVENT_MISS])
            .inc();
        let got = sample(
            "function_wasm_module_cache_events_total",
            &[("cache", CACHE_COMPILED), ("event", EVENT_MISS)],
        )
        .expect("counter");
        assert!(got >= 1.0);
        COMPILE_DURATION.observe(0.2);
        let count =
            sample("function_wasm_module_compile_duration_seconds", &[]).expect("histogram");
        assert!(count >= 1.0);
        assert_eq!(
            sample(
                "function_wasm_module_cache_events_total",
                &[("cache", "no")]
            ),
            None
        );
    }
}
