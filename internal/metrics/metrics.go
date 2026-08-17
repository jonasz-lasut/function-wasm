// Package metrics declares the Prometheus metrics of the runtime. They are
// registered with the default registry, which function-sdk-go serves on its
// metrics endpoint (:8080/metrics by default) next to the gRPC metrics.
//
// None of the metrics carries a module identity label: a Function serves an
// unbounded set of modules and digests, and per-module series would grow
// without bound. Logs carry the digest.
//
// Four metrics (RunDuration, HTTPRequests, Requests, RunInstructions) gain an
// opt-in "input" label (the Input's metadata.name) when Init(true) is called;
// use the ObserveRun / IncHTTPRequests / IncRequests / ObserveInstructions
// helpers rather than the vars directly, so the label count is handled.
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
	// refused (another version, a corrupt file) - a miss that cost a read.
	EventStale = "stale"

	OutcomeOK      = "ok"
	OutcomeError   = "error"
	OutcomeTimeout = "timeout"
	// OutcomeRefused is a wasmfn.http request outside the module's grant or
	// the egress policy; OutcomeBudget one that hit a per-run budget
	// (requests, response bytes, redirects, the request timeout).
	OutcomeRefused = "refused"
	OutcomeBudget  = "budget"
	// OutcomeFuel is a run that exhausted its instruction budget (wasmtime
	// fuel); distinct from timeout so an operator can tell compute-bound
	// from wall-clock-bound runs apart.
	OutcomeFuel = "fuel"
)

// Shared opts for metrics that Init may re-register with an extra label.
var (
	runDurationOpts = prometheus.HistogramOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "run_duration_seconds",
		Help:      "Time spent instantiating and running a module for one request, by outcome (ok, error, timeout).",
		Buckets:   []float64{0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30},
	}
	httpRequestsOpts = prometheus.CounterOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "http_requests_total",
		Help:      "HTTP requests modules made through the host (wasmfn.http), by outcome (ok = the server answered, refused = outside the grant or the egress policy, budget = over a per-run budget or the request timeout, error = the request failed).",
	}
	requestsOpts = prometheus.CounterOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "requests_total",
		Help:      "Requests by outcome: ok, refused (declined before the module ran), error (load or run failed).",
	}
	runInstructionsOpts = prometheus.HistogramOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "run_instructions",
		Help:      "Wasm instructions executed in one module run (wasmtime fuel consumed, observed only when --enable-fuel is on).",
		Buckets:   []float64{1e5, 3e5, 1e6, 3e6, 1e7, 3e7, 1e8, 3e8, 1e9, 3e9, 1e10},
	}
)

// withInput is set by Init(true); the observation helpers check it.
var withInput bool

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

	// RunDuration is how long one guest run took, by outcome. Use
	// ObserveRun instead of .WithLabelValues directly.
	RunDuration = prometheus.NewHistogramVec(runDurationOpts, []string{"outcome"})

	// RunsInFlight is how many guest runs are executing right now - pinned
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

	// HTTPRequests counts the HTTP requests modules made through the host.
	// Use IncHTTPRequests instead of .WithLabelValues directly.
	HTTPRequests = prometheus.NewCounterVec(httpRequestsOpts, []string{"outcome"})

	// Requests counts requests by outcome. Use IncRequests instead of
	// .WithLabelValues directly.
	Requests = prometheus.NewCounterVec(requestsOpts, []string{"outcome"})

	// RunInstructions is the number of wasm instructions one guest run
	// executed; observed only when --enable-fuel is on. Use
	// ObserveInstructions instead of .Observe directly.
	RunInstructions prometheus.Histogram = prometheus.NewHistogram(runInstructionsOpts)

	// runInstructionsVec replaces RunInstructions when the input label is on.
	runInstructionsVec *prometheus.HistogramVec

	// CacheBytes is the size of each on-disk store, as of the last sweep.
	CacheBytes = promauto.NewGaugeVec(prometheus.GaugeOpts{
		Namespace: namespace,
		Subsystem: subsystem,
		Name:      "cache_bytes",
		Help:      "Bytes held by each on-disk store (compiled-disk = wasmtime artifacts, blob = fetched modules), measured by the periodic sweep.",
	}, []string{"cache"})
)

func init() {
	prometheus.MustRegister(RunDuration, HTTPRequests, Requests, RunInstructions)
}

// Init optionally adds an "input" label (the Input's metadata.name) to the
// four request-path metrics: run_duration_seconds, http_requests_total,
// requests_total and run_instructions. Call it once at startup before serving;
// callers must use the observation helpers (ObserveRun, IncHTTPRequests,
// IncRequests, ObserveInstructions) so the label count matches.
func Init(inputLabel bool) {
	if !inputLabel {
		return
	}
	withInput = true

	prometheus.DefaultRegisterer.Unregister(RunDuration)
	RunDuration = prometheus.NewHistogramVec(runDurationOpts, []string{"outcome", "input"})
	prometheus.MustRegister(RunDuration)

	prometheus.DefaultRegisterer.Unregister(HTTPRequests)
	HTTPRequests = prometheus.NewCounterVec(httpRequestsOpts, []string{"outcome", "input"})
	prometheus.MustRegister(HTTPRequests)

	prometheus.DefaultRegisterer.Unregister(Requests)
	Requests = prometheus.NewCounterVec(requestsOpts, []string{"outcome", "input"})
	prometheus.MustRegister(Requests)

	prometheus.DefaultRegisterer.Unregister(RunInstructions)
	runInstructionsVec = prometheus.NewHistogramVec(runInstructionsOpts, []string{"input"})
	prometheus.MustRegister(runInstructionsVec)
}

// ObserveRun records a run's duration and outcome. When the input label is on,
// input is the Input's metadata.name; otherwise it is ignored.
func ObserveRun(outcome, input string, seconds float64) {
	if withInput {
		RunDuration.WithLabelValues(outcome, input).Observe(seconds)
	} else {
		RunDuration.WithLabelValues(outcome).Observe(seconds)
	}
}

// IncHTTPRequests counts one HTTP request by outcome.
func IncHTTPRequests(outcome, input string) {
	if withInput {
		HTTPRequests.WithLabelValues(outcome, input).Inc()
	} else {
		HTTPRequests.WithLabelValues(outcome).Inc()
	}
}

// IncRequests counts one function request by outcome.
func IncRequests(outcome, input string) {
	if withInput {
		Requests.WithLabelValues(outcome, input).Inc()
	} else {
		Requests.WithLabelValues(outcome).Inc()
	}
}

// ObserveInstructions records how many wasm instructions a run consumed.
func ObserveInstructions(count float64, input string) {
	if withInput {
		runInstructionsVec.WithLabelValues(input).Observe(count)
	} else {
		RunInstructions.Observe(count)
	}
}
