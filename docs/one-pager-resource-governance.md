# Runtime Resource Governance

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.2

One runtime serves many Compositions, many teams and many modules. This
document is the model of what a module may consume, what bounds the runtime
as a whole, and how an operator sizes and probes it — the limits that keep
one team's module from taking every other team's reconcile down with it.
Revision 1.1 adds the per-Composition `limits` (the Input shape is the
module-source-schema one-pager's); revision 1.2 the bound on concurrent
runs (`--max-concurrent-runs`), the one knob 1.1 left to the caller, what
bounds the sandbox's private `/tmp`, and the per-run HTTP egress budgets
(sandbox one-pager, revision 1.0). Everything below is implemented.


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

Every ceiling is a runtime flag set by the operator; a Composition may ask
for less than a ceiling through the Input's `limits`, never more.

| dimension | bound | flag / Input field | default | what happens at the bound |
|---|---|---|---|---|
| one run, wall clock | epoch deadline; the request context's deadline if shorter | `--module-timeout` | 30 s | the run is interrupted; fatal result naming the module and the budget |
| one run, wall clock, per Composition | the same deadline, narrowed to `limits.timeout` (≤ `--module-timeout`) | `limits.timeout` | the flag | as above, the fatal result naming the narrower budget; a value above the flag is a fatal result naming both (`limits.timeout 1m0s exceeds the runtime's --module-timeout of 30s`) before the module runs |
| one run, guest memory | wasmtime store limiter on linear memory | `--module-memory-limit` | 512 MB | the guest's `memory.grow` fails; a Go guest panics → fatal result |
| one run, guest memory, per Composition | the same limiter, narrowed to `limits.memory` (≤ `--module-memory-limit`) | `limits.memory` | the flag | as above; a value above the flag is a fatal result naming both (`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`) |
| one run, instructions (fuel) | wasmtime fuel: deterministic instruction counter, opt-in | `--enable-fuel`, `--module-instruction-limit` | off, 0 (unbounded) | the run traps; fatal result `module exceeded its instruction budget (N instructions)` with outcome `fuel`; the `run_instructions` histogram observes every run when on |
| one run, instructions, per Composition | the same fuel, narrowed to `limits.instructions` (≤ `--module-instruction-limit`) | `limits.instructions` | the flag | as above; a value above the flag is a fatal result naming both |
| resident compiled modules | LRU over the memory tier, on top of the 10-minute idle TTL | `--max-cached-modules` | 0 (TTL only) | the least recently used module is dropped and freed when its last run ends |
| memory tier at all | `--enable-memory-cache=false` maps the artifact per request and releases it | `--enable-memory-cache` | true | 6–8 ms per request for the largest Go guest, nothing resident |
| concurrent compiles | semaphore around Compile + Serialize | `--max-concurrent-compiles` | 1 | further first requests (and the requests waiting on them) queue; each waits under its own context |
| one load (fetch + compile) | the cache's own context and timeout, detached from the requester's | `engine.DefaultLoadTimeout` | 10 min | the load fails for everyone waiting; nothing is cached; the next request retries |
| disk | LRU sweep across both stores, a read counting as a use | `--max-cache-size` | 0 (unbounded) | least recently used entries removed down to 90 % of the bound, at startup and every ten minutes |
| module size | cap on fetched bytes (and on tar extraction, ×8) | `--max-module-size` | 128 MB | fetch fails; fatal result |
| concurrent runs | semaphore around one run (instantiate to response), taken after the module is loaded | `--max-concurrent-runs` | 0 (unbounded — see below) | a further request waits for a slot under its own deadline; if that passes first it is a fatal result (`waiting for a run slot: context deadline exceeded`) that consumed nothing and is not counted as a run |
| concurrent runs, per step | per-module-digest semaphore, taken before the global slot so one step does not fill every slot | `limits.concurrency` | 0 (the global bound only) | a further request waits for one of this step's slots; capped to `--max-concurrent-runs` when set |
| total run memory | a Run reserves its effective memory limit from a global pool before it starts and releases it after; a Run that cannot fit waits under its context | `--max-total-run-memory` | 0 (unbounded) | a further request waits; if its deadline passes first it is a fatal result (`waiting for 512Mi of run memory (--max-total-run-memory 2Gi): context deadline exceeded`) |
| one run, private `/tmp` bytes | *none in the runtime* — the directory lives under the runtime's `$TMPDIR` and is removed when the run ends; the filesystem behind it is the bound | `TMPDIR` → a tmpfs `emptyDir` with `sizeLimit` (`--enable-sandbox-private-tmp`) | the pod's `/tmp` | the guest's write fails (ENOSPC), the run continues or ends as the guest decides; the runtime is unaffected |
| one run, HTTP egress (`sandbox.egress`) | per-run budgets of the operator's egress policy: request count, response bytes, redirects, per-request timeout; a request is also cut at the run's deadline | `--sandbox-egress-policy` (`maxRequests`, `maxResponseBytes`, `maxRedirects`, `timeout`) | 16, 4 MiB, 5, 10 s | the request is answered with an error the guest sees; the run goes on |
| HTTP egress rate, per module | process-wide token bucket per module digest: a burst of concurrent requests and a sustained rate; keyed by the module's content digest so one noisy module cannot exhaust the rate for others | `--sandbox-egress-policy` (`rateLimit.requestsPerMinute`, `rateLimit.burst`) | off (no rate limit) | the request is answered with a budget error the guest sees; the run goes on; idle entries are swept every ten minutes |


`limits` are read from the Input only — never through `module.from`, so an
XR author cannot widen a budget — checked against the flags before anything
is resolved or fetched, and applied to that run's store through
`engine.RunOptions`, which the engine caps at its `Config` whatever the
caller passes. A Composition author who sets them tightly gets fatal
results the operator did not cause; the benefit is a shared runtime where
the loudest module cannot spend the whole ceiling. Raising a ceiling stays
operator-only.

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

Why the bound on concurrent runs is off by default: a run holds 11–16 MB
for the largest guest and its linear memory is capped per run, so
concurrency is bounded by the caller — Crossplane's reconcile workers — at
a cost the operator can compute (`concurrent runs × --module-memory-limit`
in the worst case, `× 16 MB` in practice), and a semaphore adds
head-of-line blocking: with it set, one slow module's requests queue every
other module's behind them. It exists for the deployments where the
caller's concurrency is not the number to size for — many Crossplane
instances or a large worker count sharing one runtime, a memory-tight pod
where `runs × --module-memory-limit` must not be reachable — and it fails
closed: a waiter is bounded by its own request deadline, so the queue can
never grow past what the callers themselves are still waiting on, and a
request that gives up has run nothing. The slot is taken after the module
is loaded (a wait for a compile slot is the cache's, separate) and around
exactly one run — instantiate to response — so the run-duration histogram
keeps measuring runs, not queues; `function_wasm_module_runs_in_flight`
pinned at the bound is the sign the queue is the bottleneck.

## Sizing

Per module class (linux/arm64, review measurements — the README carries the
full table):

- **Memory** ≈ base + Σ resident modules (Go ~90 MB, raw-proto Go ~40 MB,
  TinyGo 3.5 MB, Rust 0.7 MB) + `--max-concurrent-compiles` × 1 GB +
  concurrent runs × 16 MB — `--max-concurrent-runs`, when set, is the
  ceiling of that last factor (`× --module-memory-limit` in the worst
  case), otherwise the callers' concurrency is.
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
Ready is not warm by default: the first request for each module on a pod
pays a map (warm volume) or a compile (cold); the volume-backed cache covers
restarts and the compile semaphore keeps a cold fleet from thrashing.
Warming modules before Serving (`--warm-modules`: what it loads, its
ordering with the health service and the compile semaphore, what a failure
costs) is a cache optimisation and the cache one-pager's subject, not a
bound of this document.

## Failure containment

A run that traps, exits, times out or exhausts its memory is a fatal result;
a host-side panic on the request path is recovered into a fatal result with
the stack logged; a load that fails is not cached and the next request
retries; a load that panics leaves nothing wedged. The process itself is
never the unit of failure for one request — that is the property every
bound above protects.

## What comes next

The sandbox one-pager's grants are all in and each carries its bound: the
private `/tmp` is bounded by the filesystem behind `$TMPDIR` (row above) —
no runtime-side byte quota, since WASI pre-opens offer none — and HTTP
egress by the per-run budgets in the table (policy file only, not lowerable
per Input yet). Host directories are not mountable, so nothing else on the
pod's filesystem is a module's to read.


## Observability

`function_wasm_module_run_duration_seconds{outcome}` (timeouts are their
own outcome; a wait for a run slot is outside it and a request that never
got one is not counted), `function_wasm_module_runs_in_flight` (runs
executing now — at `--max-concurrent-runs`, requests are queueing),
`function_wasm_module_compile_duration_seconds`,
`function_wasm_module_cache_events_total{cache,event}` (memory tier hits
and misses, artifacts mapped or found stale, blob hits and misses),
`function_wasm_module_cache_bytes{cache}` from the sweep, and function-sdk-go's
gRPC metrics. Logs carry the module reference and digest, the panic stack
if any, and every sweep that freed something. No metric carries a module
label — the set of digests is unbounded.
