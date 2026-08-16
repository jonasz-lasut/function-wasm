# Runtime Resource Governance

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.0

One runtime serves many Compositions, many teams and many modules. This
document is the model of what a module may consume, what bounds the runtime
as a whole, and how an operator sizes and probes it — the limits that keep
one team's module from taking every other team's reconcile down with it.
Everything below is implemented; the extensions it points to live in the
module-source-schema one-pager, which is a draft.

## What was unbounded

Before this revision the sandbox bounded one run — a wall-clock deadline
(`--module-timeout`) and a cap on the guest's linear memory
(`--module-memory-limit`) — and nothing else. Measured on linux/arm64
(review, 2026-08-16): a first request for a 75 MB Go guest costs 23–28
CPU-seconds and 0.7–1.1 GB of peak memory to compile, and N such requests
ran N compiles at once (four concurrent: 100 CPU-seconds, +2.9 GB); every
distinct module used inside ten minutes stayed resident, 145 MB each for a
Go guest, with no cap; a dropped module was freed only when the garbage
collector eventually noticed a small Go wrapper; every module version ever
served left ~230 MB on disk. A pod with 1–2 cores and a few hundred MB of
memory serving twenty Go modules from a cold cache would not converge: it
was OOM-killed, restarted with an empty `/tmp`, and started over.

## The model

Every limit is a runtime flag set by the operator. Compositions ask for
nothing yet (see *What comes next*); the same ceilings apply to every module.

| dimension | bound | flag | default | what happens at the bound |
|---|---|---|---|---|
| one run, wall clock | epoch deadline; the request context's deadline if shorter | `--module-timeout` | 30 s | the run is interrupted; fatal result naming the module and the budget |
| one run, guest memory | wasmtime store limiter on linear memory | `--module-memory-limit` | 512 MB | the guest's `memory.grow` fails; a Go guest panics → fatal result |
| resident compiled modules | LRU over the memory tier, on top of the 10-minute idle TTL | `--max-cached-modules` | 0 (TTL only) | the least recently used module is dropped and freed when its last run ends |
| memory tier at all | `--enable-memory-cache=false` maps the artifact per request and releases it | `--enable-memory-cache` | true | 6–8 ms per request for the largest Go guest, nothing resident |
| concurrent compiles | semaphore around Compile + Serialize | `--max-concurrent-compiles` | 1 | further first requests (and the requests waiting on them) queue; each waits under its own context |
| one load (fetch + compile) | the cache's own context and timeout, detached from the requester's | `engine.DefaultLoadTimeout` | 10 min | the load fails for everyone waiting; nothing is cached; the next request retries |
| disk | LRU sweep across both stores, a read counting as a use | `--max-cache-size` | 0 (unbounded) | least recently used entries removed down to 90 % of the bound, at startup and every ten minutes |
| module size | cap on fetched bytes (and on tar extraction, ×8) | `--max-module-size` | 128 MB | fetch fails; fatal result |
| concurrent runs | *none* — see below | | | |

Compiled modules are reference-counted (`engine.Module.Release`): a
module dropped from the tier — by TTL, LRU or because the tier is off —
frees wasmtime's code memory the moment its last run returns, not at some
later garbage collection of a Go wrapper. Artifacts are mapped from their
files, so a resident Go module costs ~90 MB of file-backed, reclaimable
memory rather than 145 MB of heap.

Why the compile semaphore defaults to one: wasmtime compiles in parallel
across every core it is given, so two compiles at once finish no sooner
than one after the other and only add a second gigabyte of peak memory. Two
or more make sense on large nodes serving many small (Rust, TinyGo)
modules, where a compile is milliseconds and the queue, not the CPU, would
be the bottleneck.

Why there is no bound on concurrent runs: a run holds 11–16 MB for the
largest guest and its linear memory is capped per run, so concurrency is
bounded by the caller — Crossplane's reconcile workers — at a cost the
operator can compute (`concurrent runs × --module-memory-limit` in the
worst case, `× 16 MB` in practice). A `--max-concurrent-runs` semaphore
would add head-of-line blocking without a demonstrated need; it is the
first thing to add if a deployment shows otherwise.

## Sizing

Per module class (linux/arm64, review measurements — the README carries the
full table):

- **Memory** ≈ base + Σ resident modules (Go ~90 MB, raw-proto Go ~40 MB,
  TinyGo 3.5 MB, Rust 0.7 MB) + `--max-concurrent-compiles` × 1 GB +
  concurrent runs × 16 MB.
- **CPU** ≈ requests/s × per-request cost (Go 8–11 ms, raw-proto Go 1.2 ms,
  TinyGo 0.4 ms, Rust 0.05 ms) + compiles (Go 25 CPU-s each, once per module
  version per cache).
- **Cold start** ≈ Σ compile CPU / cores; a warm volume under
  `/tmp/function-wasm-cache` replaces that with a few milliseconds per
  module.
- **Disk** ≈ 230 MB per Go module version (module + artifact) until
  `--max-cache-size` bounds it.

The dominant per-request cost is the guest's own runtime initialisation
(91 % of a Go guest's request), not the host; the toolchain choice is the
sizing decision.

## Readiness and warmth

The runtime serves the gRPC health service on the function port and reports
Serving once the caches are open and the engine is up, so a Kubernetes gRPC
readiness probe has a target (README shows the `DeploymentRuntimeConfig`).
Ready is not warm: the first request for each module on a pod pays a map
(warm volume) or a compile (cold). Warming — compiling a pinned list of
digests before reporting ready — is not implemented; the volume-backed cache
covers restarts and the compile semaphore keeps a cold fleet from thrashing.

## Failure containment

A run that traps, exits, times out or exhausts its memory is a fatal result;
a host-side panic on the request path is recovered into a fatal result with
the stack logged; a load that fails is not cached and the next request
retries; a load that panics leaves nothing wedged. The process itself is
never the unit of failure for one request — that is the property every
bound above protects.

## What comes next

The ceilings are global: a trusted internal policy module and a tenant's
labelling module get the same 30 s / 512 MB, and the operator cannot give
one more room without giving it to all. A Composition-owned `limits`
(`timeout`, `memory`, each at most the runtime's ceiling — the operator sets
the ceiling, the Composition asks for less) is designed in the
module-source-schema one-pager, together with the Input shape it needs;
nothing of it is implemented, and this document describes only what is.
Likewise warming a pod (compiling a pinned list of digests before it reports
ready) and a `--max-concurrent-runs` semaphore are noted above as the next
knobs if a deployment shows the need.

## Observability

`function_wasm_module_run_duration_seconds{outcome}` (timeouts are their
own outcome), `function_wasm_module_compile_duration_seconds`,
`function_wasm_module_cache_events_total{cache,event}` (memory tier hits
and misses, artifacts mapped or found stale, blob hits and misses),
`function_wasm_module_cache_bytes{cache}` from the sweep, and function-sdk-go's
gRPC metrics. Logs carry the module reference and digest, the panic stack
if any, and every sweep that freed something. No metric carries a module
label — the set of digests is unbounded.
