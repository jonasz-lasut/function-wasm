//! The Prometheus metrics of the runtime - the Rust port of Go's
//! internal/metrics: same names, labels, buckets and help strings, on the
//! crate's default registry, served by the runtime's /metrics endpoint.
//!
//! None of the metrics carries a module identity label: a Function serves an
//! unbounded set of modules and digests, and per-module series would grow
//! without bound. Logs carry the digest.

use std::fmt::Write as _;
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

/// The default registry rendered in Prometheus text exposition format
/// (`text/plain; version=0.0.4`) - what /metrics serves to a scraper that
/// asks for the classic format.
pub fn render() -> String {
    let encoder = prometheus::TextEncoder::new();
    encoder
        .encode_to_string(&prometheus::gather())
        .unwrap_or_default()
}

/// The content type of render_openmetrics.
pub const OPENMETRICS_CONTENT_TYPE: &str =
    "application/openmetrics-text; version=1.0.0; charset=utf-8";

/// The default registry rendered as OpenMetrics 1.0 text - the /metrics
/// endpoint's main format, readable beyond Prometheus. Hand-encoded over
/// the same gathered families as render() because the prometheus crate
/// only speaks the classic format: the runtime's three metric types
/// (counter, gauge, histogram) are covered, and the differences from
/// classic are exactly a counter family named without its `_total` suffix,
/// the bucket-count-sum order, and the terminating `# EOF`. Series names,
/// labels and values are identical in both renderings.
pub fn render_openmetrics() -> String {
    let mut out = String::new();
    for family in prometheus::gather() {
        let name = family.name();
        let help = escape_help(family.help());
        match family.get_field_type() {
            prometheus::proto::MetricType::COUNTER => {
                // An OpenMetrics counter family is named without the
                // `_total` its samples carry.
                let base = name.strip_suffix("_total").unwrap_or(name);
                let _ = writeln!(out, "# HELP {base} {help}");
                let _ = writeln!(out, "# TYPE {base} counter");
                for m in family.get_metric() {
                    let labels = render_labels(m.get_label());
                    let value = fmt_f64(m.get_counter().value());
                    let _ = writeln!(out, "{base}_total{labels} {value}");
                }
            }
            prometheus::proto::MetricType::GAUGE => {
                let _ = writeln!(out, "# HELP {name} {help}");
                let _ = writeln!(out, "# TYPE {name} gauge");
                for m in family.get_metric() {
                    let labels = render_labels(m.get_label());
                    let value = fmt_f64(m.get_gauge().value());
                    let _ = writeln!(out, "{name}{labels} {value}");
                }
            }
            prometheus::proto::MetricType::HISTOGRAM => {
                let _ = writeln!(out, "# HELP {name} {help}");
                let _ = writeln!(out, "# TYPE {name} histogram");
                for m in family.get_metric() {
                    let h = m.get_histogram();
                    let mut saw_inf = false;
                    for b in h.get_bucket() {
                        saw_inf = saw_inf || b.upper_bound().is_infinite();
                        let labels =
                            render_labels_with(m.get_label(), "le", fmt_f64(b.upper_bound()));
                        let _ = writeln!(out, "{name}_bucket{labels} {}", b.cumulative_count());
                    }
                    if !saw_inf {
                        let labels = render_labels_with(m.get_label(), "le", "+Inf".to_string());
                        let _ = writeln!(out, "{name}_bucket{labels} {}", h.get_sample_count());
                    }
                    let labels = render_labels(m.get_label());
                    let _ = writeln!(out, "{name}_count{labels} {}", h.get_sample_count());
                    let _ = writeln!(out, "{name}_sum{labels} {}", fmt_f64(h.get_sample_sum()));
                }
            }
            // The runtime registers no other type; a family from a future
            // dependency is skipped rather than mis-rendered.
            _ => {}
        }
    }
    out.push_str("# EOF\n");
    out
}

fn render_labels(labels: &[prometheus::proto::LabelPair]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = labels
        .iter()
        .map(|l| format!("{}=\"{}\"", l.name(), escape_label(l.value())))
        .collect();
    format!("{{{}}}", inner.join(","))
}

/// The metric's labels plus one extra (a histogram bucket's le), in label
/// order with the extra last, as the classic encoder renders it.
fn render_labels_with(
    labels: &[prometheus::proto::LabelPair],
    name: &str,
    value: String,
) -> String {
    let mut inner: Vec<String> = labels
        .iter()
        .map(|l| format!("{}=\"{}\"", l.name(), escape_label(l.value())))
        .collect();
    inner.push(format!("{name}=\"{value}\""));
    format!("{{{}}}", inner.join(","))
}

fn escape_label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn escape_help(v: &str) -> String {
    v.replace('\\', "\\\\").replace('\n', "\\n")
}

fn fmt_f64(v: f64) -> String {
    if v == f64::INFINITY {
        return "+Inf".to_string();
    }
    if v == f64::NEG_INFINITY {
        return "-Inf".to_string();
    }
    if v.is_nan() {
        return "NaN".to_string();
    }
    format!("{v}")
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The OpenMetrics rendering differs from classic exactly where the
    /// spec does: counter families named without _total, the terminating
    /// EOF, and bucket-count-sum order - over the same series and values.
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
        let count = om
            .find("function_wasm_module_compile_duration_seconds_count")
            .expect("count sample");
        let sum = om
            .find("function_wasm_module_compile_duration_seconds_sum")
            .expect("sum sample");
        assert!(count < sum, "OpenMetrics orders histogram count before sum");

        // The classic rendering carries the same series for the same
        // scrape, classic-formatted.
        let classic = render();
        assert!(classic.contains("# TYPE function_wasm_module_cache_events_total counter"));
        assert!(!classic.contains("# EOF"));
    }
}
