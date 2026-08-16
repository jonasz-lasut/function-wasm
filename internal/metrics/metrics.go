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
	CacheCompiled = "compiled"
	CacheBlob     = "blob"
	EventHit      = "hit"
	EventMiss     = "miss"

	OutcomeOK      = "ok"
	OutcomeError   = "error"
	OutcomeTimeout = "timeout"
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

	// CacheEvents counts hits and misses of the compiled-module and blob caches.
	CacheEvents = promauto.NewCounterVec(prometheus.CounterOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "cache_events_total",
		Help:      "Cache lookups by cache (compiled, blob) and event (hit, miss).",
	}, []string{"cache", "event"})
)
