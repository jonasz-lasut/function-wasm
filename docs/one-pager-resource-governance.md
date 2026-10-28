# Runtime resource governance

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.6 (a cold load bounded by `--module-compile-timeout`, the 1.0 audit's F-4; table growth charged to the ceiling and the pool like memory growth, the 1.0 audit's F-1; rewritten in the present tense over the Rust workspace; merges in the governance-perf record, revision 0.2: the egress rate limit and the fair run slots are part of the model, and its dropped fuel phase is under Alternatives)

One runtime serves many Compositions, many teams and many modules. This
document is the model of what a module may consume, what bounds the runtime as
a whole, how the bounds divide the runtime between modules, and how an operator
sizes and probes it. The limits exist so that one team's module can't take
every other team's reconcile down with it.

Every ceiling is a runtime flag the operator sets. A Composition may lower a
ceiling through the Input's `limits` and can't raise it (the
[module source schema](one-pager-module-source-schema.md) defines the fields).
The runtime reads `limits` from the Input only, never through `module.from`, so
an XR author can't widen a budget. A value over its ceiling is a fatal result
that states both, checked before anything is resolved or fetched:
`limits.timeout 1m0s exceeds the runtime's --module-timeout of 30s`,
`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`. The
exception is `limits.concurrency`, which has no ceiling of its own and is
capped to `--max-concurrent-runs` without a message.

## Background

A first request for a large guest costs seconds of CPU and about a gigabyte of
peak memory to compile ([sizing](operators/sizing.md) has the measurements),
and the run itself costs far less. Without bounds, N cold requests would run N
compiles at once and every module used within ten minutes would stay resident.
Every version ever served would also stay on disk. A pod with a core or two serving
twenty large modules from a cold cache wouldn't converge: it would be killed
for memory, restart with an empty cache and begin again.

## The model

| dimension | bound | flag or Input field | default | at the bound |
|---|---|---|---|---|
| one run, wall clock | epoch deadline, or the request's gRPC deadline when that's sooner | `--module-timeout`; `limits.timeout` narrows it | 30 s | the run is interrupted and the fatal result states the module and the budget |
| one run, guest memory | Wasmtime store limiter on each linear memory and each table, a host pointer per element | `--module-memory-limit`; `limits.memory` narrows it | 512 MB | `memory.grow` or `table.grow` fails and the guest decides; counted in `memory_denials_total{reason="limit"}` |
| one run, call stack | Wasmtime's stack ceiling, engine-wide | `--module-stack-limit` | 512 KB | the guest traps: a fatal result |
| resident compiled modules | LRU over the memory tier, on top of its 10-minute idle TTL | `--max-cached-modules` | 0 (TTL only) | the least recently used module is dropped |
| the memory tier itself | off maps the artifact per request and releases it | `--enable-memory-cache` | true | nothing stays resident |
| concurrent compiles | semaphore around fetch and compile | `--max-concurrent-compiles` | 1 | further first requests, and those waiting on the same load, queue |
| one cold load, wall clock | `tokio::time::timeout` around the load's thread | `--module-compile-timeout` | 0 (unbounded) | the request fails with a fatal result that states the bound; the compile finishes on its thread, keeps its slot until then and stores its artifact; counted in `cache_events_total{cache="compiled",event="timeout"}` |
| disk | LRU sweep over the stores, a read counting as a use | `--max-cache-size` | 0 (unbounded) | entries removed down to nine tenths of the bound, at startup and every ten minutes |
| module size | cap on fetched bytes, and on tar extraction at eight times the cap | `--max-module-size` | 128 MB | the fetch fails: a fatal result |
| concurrent runs | fair run slots, taken after the module is loaded | `--max-concurrent-runs` | 0 (the caller's concurrency) | a further request waits under its own deadline |
| concurrent runs of one step | per-digest semaphore, taken before the global slot | `limits.concurrency` | 0 (the global bound only) | a further request of this step waits under its own deadline |
| total run memory | pool charged as guests grow | `--max-total-run-memory` | 0 (unbounded) | a growth the pool can't serve before the run's deadline is denied |
| private `/tmp` | none in the runtime: the filesystem behind `$TMPDIR` | `TMPDIR` | the pod's `/tmp` | the guest's write fails (`ENOSPC`); the runtime is unaffected |
| one run, HTTP egress | per-run budgets, fixed at 16 requests, 4 MiB of response bytes, 5 redirects and a 10 s timeout per request | none | as stated | the request is answered with an error the guest reads; the run goes on |
| HTTP egress rate | token bucket per module digest | `--egress-rate-limit-per-minute`, `--egress-rate-limit-burst` | off | a budget error the guest reads; the run goes on |

A run's wall clock meters guest compute. Time blocked in `wasi:http` is
credited back to the epoch deadline, so a slow upstream doesn't spend
`limits.timeout`. The request's own gRPC deadline stays the hard cap. Without
one the credit is still bounded: every egress request is cut at the run's
deadline, and a run stretches to about twice its budget at most. Only
`wasi:http` time is creditable. Crediting arbitrary host time would let a guest
stretch its wall clock through a host import with no such bound.

### Compiles and the memory tier

The compile semaphore defaults to one because Wasmtime compiles in parallel
across every core it's given: two compiles at once finish no sooner than one
after the other and add a second gigabyte of peak memory. A larger value suits
large nodes serving many small modules, where a compile takes milliseconds and
the queue, not the CPU, is the bottleneck. A failed load isn't cached, and
the next request retries. [The cache record](one-pager-cache.md)
describes the caches.

### Run slots and fairness

The caller (Crossplane's reconcile workers) bounds concurrency unless the
operator sets `--max-concurrent-runs`. The flag is off by default because a
run's memory is capped per run and a semaphore adds head-of-line blocking. It
exists for deployments where the caller's concurrency isn't the number to size
for: many Crossplane instances or a large worker count sharing one runtime, or
a memory-tight pod where concurrent runs times `--module-memory-limit` must not
be reachable.

With the flag set, the engine's `FairScheduler` (`crates/engine/src/concurrency.rs`)
queues waiters per module digest and admits them round-robin. A freed slot goes
to the next digest in rotation, first in first out within one digest. One hot
module takes at most its share of the slots, and a request's wait
scales with the modules queued ahead of it, not the requests. With one
digest waiting the scheduler is a plain FIFO.

`limits.concurrency` adds a bound per step. `StepSlots` keeps a semaphore per
module digest, so two Compositions naming the same module share it. A run takes
its step slot before the global one, so a step that has used its share doesn't
occupy a global slot. A digest's slot count is set by the first step to use it
and applies until the entry is swept after ten idle minutes.

The run's budget or the request's gRPC deadline, whichever comes first, caps
each wait. A queue never outgrows what its callers still wait for, and a request
that gives up took no slot and isn't counted as a run. The refusals are fatal
results:

- `waiting for a run slot: deadline exceeded`
- `waiting for one of this step's 2 run slots (limits.concurrency): deadline exceeded`

A run takes its slot after the module has loaded, around exactly one run, so
`function_wasm_module_run_duration_seconds` measures runs rather than queues.
`function_wasm_module_runs_in_flight` pinned at `--max-concurrent-runs` means
the queue is the bottleneck.

### Run memory pool

`--max-total-run-memory` bounds the total memory of all running guests, their
linear memories and tables together.
A component has no top-level memory to reserve ahead of the run. Each run
charges the pool as its guest grows, the initial memory included, and releases
everything when its store drops. The pool reflects what runs use, and a small
guest doesn't wait on a worst-case ceiling it never claims. A growth the pool
can't serve before the run's deadline is denied. The guest sees `memory.grow`
or `table.grow` fail, as at the per-run ceiling, the runtime logs `Denied a
memory growth` or `Denied a table growth` and the denial counts in
`memory_denials_total{reason="pool"}`. The runtime refuses
at startup a pool smaller than `--module-memory-limit`, since no full-limit run
could ever fit.

The acquisition order is the step slot, the global slot, then memory as the run
grows. A run waiting for the pool keeps its run slots, and runs with partial
reservations can wait on each other. The run's deadline bounds each wait and
denies the growth when it passes. A cycle lasts no longer than a deadline.

### Egress rate

A module called once per XR against one endpoint makes a request per reconcile
per XR. `RateLimiters` in `crates/function/src/egress.rs` keeps a token bucket
per module digest, process-wide, consulted after the per-run request count. The
digest, not a host or a Composition, is the key: it's the unit of the audit
line, idle buckets expire so the map follows the modules in use, and one
module's traffic can't exhaust another's rate. A bucket starts full.
`--egress-rate-limit-burst` sets its size, and `0` derives
`max(1, requests per minute)`. Over the limit, the guest reads
`sandbox.egress: the module's request rate exceeds the egress policy's rateLimit`
and `http_requests_total{outcome="budget"}` counts the request. The check
returns at once, and the module retries on its next reconcile. The ten-minute sweep drops
idle buckets. The operator sets the rate, and a Composition can't change it.

## Sizing

The [sizing page](operators/sizing.md) has the measurements. The terms are:

- **Memory** is the resident compiled modules, plus `--max-concurrent-compiles`
  times the peak of compiling the largest module, plus concurrent runs times the
  per-run figure. `--max-concurrent-runs`, when set, caps the last factor (at
  `--module-memory-limit` per run in the worst case), and `--max-total-run-memory`
  caps the sum of what runs claim.
- **CPU** is requests per second times the per-request cost, plus the compiles,
  one per module version per cache.
- **Cold start** is the compile CPU of every module over the cores. A volume
  under `/tmp/function-wasm-cache` replaces compiles with loading the stored
  artifact, and `--warm-modules` moves either ahead of readiness.
- **Disk** is a module and its artifact per version served, until
  `--max-cache-size` bounds it.

The dominant per-request cost is the guest's own initialisation, not the host,
so the toolchain choice is the sizing decision.

## Readiness and warmth

The runtime reports the gRPC health service on the function port and plain-HTTP
`/readyz` on `--health-address`, ready once the caches are open and the
`--warm-modules` entries are loaded. Ready isn't warm by default: the first
request for each module on a pod pays a map from a warm volume or a compile from
cold. The volume-backed cache covers restarts, and the compile semaphore keeps a
cold fleet from thrashing. [Readiness and warm-up](operators/readiness.md) is the
manual page, and the cache record covers what warm-up loads.

## Failure containment

A run that traps, exits, times out or exhausts its memory is a fatal result. A
panic in the host on the run path becomes a fatal result too, and a load that
fails isn't cached. The process is never the unit of failure for one request,
which is the property every bound here protects.

The sandbox's other grants have bounds in the table. The filesystem behind
`$TMPDIR` bounds the private `/tmp`, because WASI pre-opens don't support a
quota and the runtime adds none. The per-run budgets bound egress, and an
operator can't configure them. Host directories aren't mountable, so nothing
else on the pod's filesystem is a module's to read.

## Observability

The series are [documented](operators/metrics.md). Those that govern resources
are `function_wasm_module_run_duration_seconds{outcome}` (timeouts are an
outcome of their own, and a wait for a run slot is outside it),
`function_wasm_module_runs_in_flight`,
`function_wasm_module_compile_duration_seconds`,
`function_wasm_module_cache_events_total{cache,event}`,
`function_wasm_module_cache_bytes{cache}` from the sweep, and
`function_wasm_module_memory_denials_total{reason}`. No series has a module,
digest or host label: the set is unbounded.

## Alternatives

**Fuel.** Wasmtime's instruction counting (a fuel budget per run) would have
bounded CPU rather than wall clock. The project dropped it while wazero was the
fallback engine, because it couldn't build a feature tied to one WebAssembly
engine. It also changes the generated code, which needs a separate compiled
cache namespace. The epoch deadline, a wall clock that credits
`wasi:http` time back, remains the run bound.

**A response cache for egress.** Caching a module's HTTP responses would cut
the thousand identical requests per sweep that the rate limit now bounds. The
invalidation semantics (TTL, `Cache-Control`, key correctness across headers)
add more complexity than the benefit repays when modules already run in
milliseconds. The limit refuses instead.

**A metric label from the Input's name.** An Input is a fragment embedded in a
Composition, so its `metadata.name` is unset in practice and, when set, is
neither unique nor informative across the many Compositions one runtime handles.
The label's signal was unreliable.

**One FIFO for run slots.** The first form of `--max-concurrent-runs` was one
queue, which put a slow module's requests ahead of every other module's. The
per-digest round-robin replaced it.

**Reserving the whole memory limit up front.** A run reserved its effective
limit before starting, which capped parallelism at what a worst case would
claim. The pool charges growth as it happens instead.

## Open questions

- Should the egress rate limit also key by destination host? A per-host bucket
  would protect a third party across modules, and needs a bounded key set
  first. It stays per module digest until then.
- Should warm-up load from a bounded "last served" file on the cache volume? It
  would make the warm set a function of history that a fresh volume lacks and an
  operator can't read from the deployment. `--warm-modules` covers cold starts
  and a persistent volume keeps artifacts; revisit if a real fleet shows the
  gap.
