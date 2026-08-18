# Governance and Performance Additions

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 0.1

The resource-governance one-pager bounds a run by wall clock and linear
memory, the runtime by compiles, resident modules, disk and (optionally)
concurrent runs; the sandbox one-pager bounds egress per run. This
document adds what the 2026-08-16 capabilities research ranked as cheap and
worth having, as one coherent extension of that model in four independent
phases: an egress economy (per-module rate limits), fairness inside one
runtime (per-Input concurrency, fair queueing, a global run-memory budget,
an opt-in per-Input metric label), distribution for restricted networks (a
registry mirror, an OCI layout on disk, `function precompile`), and a
raw-bytes gRPC codec that skips two of the four protobuf passes. Each
phase is a new optional Input field or a new flag; none changes an existing
one, and every ceiling keeps the rule that the operator sets it and a
Composition may only ask for less. Nothing here gates `v0.1.0`.

Fuel-based instruction counting (wasmtime's `SetConsumeFuel`/`SetFuel`)
was drafted as Phase 1 and dropped: it is not portable to wazero, the
stated fallback engine, and the project cannot build features tied to one
wasm engine.

## Invariants across all phases

- **Ceilings are flags, budgets are Input fields, the Input only narrows.**
  A `limits` value above its ceiling is a fatal result naming both, as
  `runOptions` does today; a capability without its flag is
  `… is refused: the runtime was started without --enable-…`.
- **Digests are stated, not discovered.** A mirror or a layout changes
  where bytes come from, never which bytes; every fetch stays verified
  against the stated digest, and the caches keep their keys.
- **The pull credential stays withheld, the request is forwarded whole.**
  A codec that forwards raw bytes must still strip exactly what
  `withoutCredential` strips.
- **Metric cardinality stays bounded.** New label values are enumerations
  (`outcome=cached`, `outcome=fuel`); the only identity label is the opt-in
  Input name (phase 3), never a digest, ref or host.
- **The guest never sees a trap for a policy decision.** A cache miss, a
  rate limit or a slot wait is an error the guest reads or a fatal result
  the runtime issues — never a trap inside the module.

## ~~Phase 1 — Fuel~~ (dropped)

Wasmtime's fuel-based instruction counting (`SetConsumeFuel`, `SetFuel`,
`GetFuel`, `OutOfFuel` trap, codegen change requiring a separate compiled
cache namespace) is not portable to wazero, the stated fallback engine.
The project cannot build features tied to one wasm engine. The epoch
deadline (wall clock) remains the run bound.

## Phase 2 — Egress economy: per-module rate limits

A module called once per XR against the same endpoint makes one request per
reconcile per XR: a thousand XRs are a thousand identical GETs per sweep.
One addition in `internal/egress`, behind the operator's policy file:

**Per-module rate limits** (policy `rateLimit: {requestsPerMinute, burst}`).
A token bucket per module digest in `Egress` (a bounded, idle-expiring map:
the set of digests in use is the memory tier's size, not a metric label),
consulted in `Client.do` after the per-run `maxRequests` check; over the
limit the guest reads `sandbox.egress: the module's request rate exceeds
the egress policy's rateLimit` - `outcome=budget`, never a trap - and
retries next reconcile. Policy file only, like the other budgets; a
Composition cannot raise it. Idle entries are swept every ten minutes.
Deferred: a per-host bucket (protects a third party across modules - needs
a bounded key set first), a Composition-lowerable `sandbox.egress.rateLimit`.

Response caching was considered and dropped: the invalidation semantics
(TTL, Cache-Control, key correctness across headers) add complexity out of
proportion to the benefit when modules already run in milliseconds.

## Phase 3 — Fairness inside one runtime

`--max-concurrent-runs` is one FIFO: with it set, a slow module's requests
queue everyone's behind them (the resource-governance one-pager says so),
and without it `runs × --module-memory-limit` is the caller's number. Four
small additions, all in `internal/engine` and `cmd/function`, all optional:

- **`limits.concurrency`** (int32, ≥ 1): at most N runs of *this step* at
  once - a semaphore keyed by the module's content digest
  (bounded by the set of served digests, idle-expired every ten minutes),
  taken after the module is loaded and before the global slot, waited under
  the request context:
  `waiting for one of this step's 2 run slots (limits.concurrency): context
  deadline exceeded` - a fatal result that consumed nothing, not a run. No
  ceiling: a Composition can only lower; a value above
  `--max-concurrent-runs` is capped silently. **Implemented.**
- **Fair queueing** when `--max-concurrent-runs` is set: the slot channel
  becomes per-digest queues served round-robin (`engine.fairScheduler`), so
  one hot module takes at most its share of the slots and a request's wait
  is bounded by the number of *modules* waiting, not requests. Off the flag
  nothing changes. `RunOptions.Key` (the module digest) identifies the
  queue. M. **Implemented.**
- **`--max-total-run-memory`** (MB, `MAX_TOTAL_RUN_MEMORY`, 0 = off): a run
  reserves its effective memory limit (`limits.memory` or the ceiling) from
  a global pool before it starts and returns it after; a run that cannot fit
  waits under its context (`waiting for 512Mi of run memory
  (--max-total-run-memory 2Gi): context deadline exceeded`). Reservation,
  not accounting — the store limiter still caps actual use — so a step that
  states a small `limits.memory` gets more parallelism, an incentive to
  state it honestly. Acquisition order is fixed — per-Input slot, global
  slot, memory — so no cycle exists. S. **Implemented.**
- **`--metrics-label-input-name`** (bool, off): adds an `input` label - the
  Input's `metadata.name`, empty when unset - to `requests_total`,
  `run_duration_seconds`, `http_requests_total` and `run_instructions`. It
  is Composition-authored and bounded by the operator's own Compositions,
  never a digest, ref or host; the risk (hundreds of Inputs) is documented
  next to the flag. `internal/metrics` builds its vectors at startup with
  or without the label (`metrics.Init`). S. **Implemented.**

Interactions: the resource-governance table gains a row per addition;
`run_duration_seconds` keeps measuring runs (every wait is outside it);
`runs_in_flight` is unchanged.

## Phase 4 — Restricted networks: mirror, OCI layout, `function precompile`

Compositions state `ghcr.io/…@sha256:…`; an air-gapped or egress-fenced
cluster cannot reach it, and today's answer is a `--module-dir` volume or an
`http` source per cluster — environment-specific Compositions. Three
additions keep them portable:

- **`--registry-mirror ghcr.io=registry.internal/ghcr`** (repeatable,
  `REGISTRY_MIRROR`): `internal/module/oci.go` rewrites the *fetch* location
  — `registry.internal/ghcr/<repository>@sha256:<same digest>` — for the
  manifest, the layer and the cosign `.sig` lookup (a mirror filled with
  `crane copy` must copy `sha256-<hex>.sig` too; the README says so). Policy
  (`repositoryAllowList`), the cache key, `Ref.Description` and the audit
  line see the stated ref, unchanged; a mirror can only fail a fetch
  (`module layer content is … want …`), never substitute bytes. Auth for
  the mirror is the runtime's own keychain (`DOCKER_CONFIG`) — the mirror is
  the operator's; a step credential was declared for the stated registry
  and is not sent to a host the Composition author did not name. S.
  **Implemented.**
- **`--oci-layout-dir /modules`** (`OCI_LAYOUT_DIR`): a directory in OCI
  image-layout form (`index.json`, `blobs/sha256/…`, as `crane pull
  --format=oci`, `oras copy --to-oci-layout` or a `guestfn save` write it),
  consulted by `Fetch` before the network: the stated manifest digest names
  `blobs/sha256/<hex>` regardless of any repository name, the manifest names
  its layer, both are verified as always and the layer lands in the blob
  store like a pulled one. No new `module.type` — `type: OCI` with the same
  ref works online and offline, and the operator's flag decides where bytes
  come from. With `--cosign-key`, the `.sig` manifest is looked up in the
  layout's index by the ref-name annotation the layout tools write
  (`cosign save` keeps signatures next to the image), so verification works
  offline. M. **Implemented.**
- **`function precompile <ref|path:…>…`**: the runtime binary gains kong
  subcommands (`serve` as the default so the bare invocation Crossplane uses
  is unchanged); `precompile` runs the warm path (`cmd/function/warm.go`:
  resolve → `Verify` → `load` → `Release`) with the same engine and
  `engine.Version()`, so an init container or a Job over the shared cache
  volume leaves exactly the artifacts the serving pods will map, honours
  `--registry-mirror`/`--oci-layout-dir`/`--cosign-key`, and — unlike
  warm-up — exits non-zero when an entry failed. The "AOT is cache, never
  source" stance stands: publisher-shipped `.cwasm` remains a non-goal. S.
  **Implemented.**

## Phase 5 — Raw-bytes gRPC codec

A request is decoded by gRPC into Go structs, re-encoded by the host for
the guest, and the guest's response is decoded by the host and re-encoded
by gRPC: four passes, measured at ~20 ms per MB of observed state,
regardless of guest. Two of them are avoidable without field-level wire
surgery. A codec (`internal/codec`, registered as `proto` via
`encoding.RegisterCodec` globally - acceptable in a process with one gRPC
server) decodes the request as before *and* stashes the wire bytes in a
side table keyed by the message pointer (`weak.Pointer` +
`runtime.AddCleanup`); `RunFunction` consumes the stashed bytes before the
decoded request may be modified and hands `engine.RunRaw` the stashed bytes
verbatim - unless a pull credential must be withheld, in which case the
normal path re-marshals the modified request as before. The guest's
response bytes travel back through the codec when the guest set `meta`
(the common case for Go guests): `RunRaw` returns the raw bytes alongside
the decoded response, and `RunFunction` stashes them on the response so
the codec's `Marshal` returns them instead of re-encoding. When `meta` is
absent (a non-Go guest, typically), the host sets it and the codec
marshals normally. Savings: skip request Marshal (pass 2) when no
credential stripping is needed, skip response Marshal (pass 4) when the
guest sets meta - up to 2 of 4 passes for large XRs. The `protowire`
approach (field-level credential strip and meta scan for up to 3 of 4
passes) was considered and dropped as disproportionate complexity. Risks:
the global codec also serves the health service (tiny messages, plain
path - the codec falls through to standard proto for non-RunFunction
messages); when credential stripping is needed the raw path is not taken,
so the pull credential is absent by construction - tested. Effort M.
**Implemented.** Deferred: streaming, avoiding the copy into guest memory
(impossible: the guest owns its memory), `grpc.ForceServerCodec` via an
upstream function-sdk-go PR.

## Trust and threats

Nothing here widens what a module can reach. Fuel adds a bound; the cache
serves only what the same module with the same headers would have fetched,
in memory, and honours `no-store`; the rate limit and fairness only refuse
or delay; the mirror and layout are the operator's and cannot alter bytes
(digests); `precompile` runs the operator's own engine over stated digests,
never a publisher's native code; the codec forwards the same request minus
the same credential. Threats to test explicitly: cache poisoning across
modules (impossible by key), a cached secret surviving a restart
(impossible: memory only), a raw-path request leaking the pull credential
(the invariant test), a mirror serving other bytes (digest mismatch, fatal),
a run holding a slot while it waits for memory (fixed order, bounded by
`ctx`).

## Phasing and effort

| phase | what | effort | lands |
|---|---|---|---|
| ~~1~~ | ~~fuel~~ | - | **Dropped** (not portable to wazero) |
| 2 | policy `rateLimit` | S | **Implemented.** |
| 3 | `limits.concurrency`, fair queueing, `--max-total-run-memory`, `--metrics-label-input-name` | S, M, S, S | **Implemented.** |
| 4 | `--registry-mirror`, `--oci-layout-dir`, `function precompile` (+ `serve` default subcommand) | S, M, S | **Implemented.** |
| 5 | raw-bytes codec, `engine.RunRaw`, credential-stripping fallback to normal path, response stash when meta is present | M | **Implemented.** |

Every phase is additive — new optional `limits`/`sandbox` fields, new
flags, new policy-file sections (a strict-parsed file with a new section
needs the runtime that knows it, a runtime concern, not an Input one), new
enumeration values on existing metric labels. Nothing here gates `v0.1.0`.

## Open questions

- ~~**Fuel as an engine-wide opt-in flag?**~~ Dropped: not portable to
  wazero.
- **Cache key: include the module digest?** Recommendation: yes — a hit rate
  across different modules against the same endpoint is not worth the
  sentence "modules never share responses" being conditional.
- **Rate-limit unit: per module digest or per host?** Recommendation: per
  module first (the digest is already the audit's unit and the key set is
  the memory tier's); per host when a bounded key set is designed.
- **Opt-in `input` metric label from `metadata.name`?** Recommendation:
  yes, off by default, the only identity label ever, documented with its
  cardinality risk; per-Input governance without per-Input observability is
  half a feature.
- **Warm-up from a bounded "last served" file on the cache volume?**
  Recommendation: no — it makes the warm set a function of history that a
  fresh volume lacks and an operator cannot read from the deployment;
  `--warm-modules` (stated) and `function precompile` (stated, a Job) cover
  cold starts, and a persistent volume already keeps artifacts. Revisit if a
  real fleet shows the gap.
- **Codec: global registration or a function-sdk-go server-options PR
  first?** Recommendation: open the upstream PR and, meanwhile, register
  globally behind a comment — the process has one server, and the codec
  falls through to `proto` for every other message.
