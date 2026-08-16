// Package metrics declares the Prometheus metrics of the runtime. They are
// registered with the default registry, which function-sdk-go serves on its
// metrics endpoint (:8080/metrics by default) next to the gRPC metrics.
//
// None of the metrics carries a module identity label: a Function serves an
// unbounded set of modules and digests, and per-module series would grow
// without bound. Logs carry the digest.
package metrics

import (
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promauto"
)

const (
	namespace = "function_wasm"
	subsystem = "module"
)

// Label values.
const (
	// CacheCompiled is the in-memory tier of compiled modules.
	CacheCompiled = "compiled"
	// CacheCompiledDisk is the on-disk store of wasmtime artifacts.
	CacheCompiledDisk = "compiled-disk"
	// CacheBlob is the on-disk store of fetched modules.
	CacheBlob = "blob"
	EventHit  = "hit"
	EventMiss = "miss"
	// EventStale is a compiled-disk lookup that found an artifact wasmtime
	// refused (another version, a corrupt file) — a miss that cost a read.
	EventStale = "stale"

	OutcomeOK      = "ok"
	OutcomeError   = "error"
	OutcomeTimeout = "timeout"
	// OutcomeRefused is a wasmfn.http request outside the module's grant or
	// the egress policy; OutcomeBudget one that hit a per-run budget
	// (requests, response bytes, redirects, the request timeout).
	OutcomeRefused = "refused"
	OutcomeBudget  = "budget"
)

var (
	// CompileDuration is how long compiling a module took, on cache misses.
	CompileDuration = promauto.NewHistogram(prometheus.HistogramOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "compile_duration_seconds",
		Help:      "Time spent compiling a module with wasmtime (compiled-module cache misses only).",
		Buckets:   []float64{0.05, 0.1, 0.25, 0.5, 1, 2, 5, 10, 30},
	})

	// FetchDuration is how long fetching module bytes took, by source kind.
	FetchDuration = promauto.NewHistogramVec(prometheus.HistogramOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "fetch_duration_seconds",
		Help:      "Time spent fetching and verifying module bytes, by source (oci, http, path); blob-cache hits included.",
		Buckets:   []float64{0.01, 0.05, 0.1, 0.25, 0.5, 1, 2, 5, 10, 30, 60},
	}, []string{"source"})

	// RunDuration is how long one guest run took, by outcome.
	RunDuration = promauto.NewHistogramVec(prometheus.HistogramOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "run_duration_seconds",
		Help:      "Time spent instantiating and running a module for one request, by outcome (ok, error, timeout).",
		Buckets:   []float64{0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30},
	}, []string{"outcome"})

	// RunsInFlight is how many guest runs are executing right now — pinned
	// at --max-concurrent-runs, it says the bound is what requests wait on.
	RunsInFlight = promauto.NewGauge(prometheus.GaugeOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "runs_in_flight",
		Help:      "Module runs executing at this moment (a run holding a slot when --max-concurrent-runs bounds them).",
	})

	// CacheEvents counts hits and misses of the compiled-module and blob caches.
	CacheEvents = promauto.NewCounterVec(prometheus.CounterOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "cache_events_total",
		Help:      "Cache lookups by cache (compiled = in-memory modules, compiled-disk = wasmtime artifacts on disk, blob = fetched modules on disk) and event (hit, miss, stale).",
	}, []string{"cache", "event"})

	// HTTPRequests counts the HTTP requests modules made through the host
	// (the wasmfn.http import), by outcome. No host label: the set of hosts a
	// Function's modules reach is unbounded; the audit log line names it.
	HTTPRequests = promauto.NewCounterVec(prometheus.CounterOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "http_requests_total",
		Help:      "HTTP requests modules made through the host (wasmfn.http), by outcome (ok = the server answered, refused = outside the grant or the egress policy, budget = over a per-run budget or the request timeout, error = the request failed).",
	}, []string{"outcome"})

	// Requests counts requests by outcome: ok (the module ran and answered),
	// refused (the runtime declined before running it — input, policy,
	// grants, limits, resolution, verification), error (the load or the run
	// failed). Together with the run histogram it tells a shared runtime's
	// operator how many requests never reach a module and why.
	Requests = promauto.NewCounterVec(prometheus.CounterOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "requests_total",
		Help:      "Requests by outcome: ok, refused (declined before the module ran), error (load or run failed).",
	}, []string{"outcome"})

	// CacheBytes is the size of each on-disk store, as of the last sweep.
	CacheBytes = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "cache_bytes",
		Help:      "Bytes held by each on-disk store (compiled-disk = wasmtime artifacts, blob = fetched modules), measured by the periodic sweep.",
	}, []string{"cache"})
)
