# Module and Compiled Artifacts Cache

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 2.2

function-wasm keeps two caches on disk under one fixed directory, both
addressed by content digests, plus a bounded in-memory tier for compiled
modules. Fetched modules always land on disk and are never held in memory;
compiled modules are mapped from their on-disk artifact, held in memory
while they are used and always persisted. Nothing has to be invalidated: an
entry's name is its content. The knobs are safety limits — how many modules
stay resident, how many compile at once — not cache tuning; the one
addition of revision 2.2, `--warm-modules`, decides *when* the caches are
filled (before readiness), not how.

## Layout

```
/tmp/function-wasm-cache/
├── modules/                          fetched blobs, as delivered by the source
│   └── sha256-<hex>                    verified against its name on every read
└── compiled/                         wasmtime artifacts (Module.Serialize)
    └── <wasmtime-go version>-<os>-<arch>/   e.g. v47.0.0-linux-arm64
        └── sha256-<hex>                the artifact for the module that digest pins
```

The directory is `cache.DefaultDir`. It is created at startup; a pod that
must keep its caches across restarts backs it with a volume
(`DeploymentRuntimeConfig` → emptyDir survives container restarts, a PVC
survives rescheduling), a pod with a read-only root filesystem must mount
something writable there. Both stores are `afero` filesystems
(`internal/cache.Store`), so tests run them in memory.

## Keys

Every key is a `sha256:<hex>` **stated in the Input** (or computed for a
served file, `module.path`, hashed when size or mtime change), so resolving
never touches the network and no reference is ever resolved at request time:

- An OCI source is keyed by the **manifest digest** in its reference
  (`repo[:tag]@sha256:…`, printed by `guestfn push`; a tag alone is not
  accepted). The manifest pins the module: it names its layer's digest, the
  registry client verifies the manifest against the reference and the layer
  against the manifest, and the layer is verified again before it is stored.
  The compiled artifact is keyed by the manifest digest; the blob store keeps
  the **layer** under its own digest, as delivered — a raw wasm layer's digest
  is the module's, a tar layer is stored as tar and extracted on every read —
  so every blob is verifiable against its name.
- An HTTP source is keyed by `module.http.digest`, the digest of the module
  bytes; the download is verified against it before it is stored or compiled.

A cache entry is therefore exactly the content its name promises, and a
module fetched from two sources shares its blob when the bytes are the same.

The compiled store is namespaced by `engine.Version()` —
`<wasmtime-go version from the build info>-<GOOS>-<GOARCH>` — because a
serialized artifact is only loadable by the wasmtime that produced it on
the same host type (wasmtime also embeds its exact version and the CPU
features it targeted, and refuses anything else: such an artifact counts as
`stale` in the metrics and is recompiled). At startup other version directories under `compiled/`
are removed once nothing has written them for `cache.StaleVersionAge` (a
day): the running binary could not load them, but during a rolling upgrade
pods of both versions share the volume, and deleting each other's artifacts
on start would recompile everything twice. Loading is defensive anyway: a
corrupt or foreign artifact fails wasmtime's own validation, counts as a
miss, and is overwritten by a fresh compile.

## Tiers and the request path

```
RunFunction
  └─ resolver.Resolve(module)          no I/O: digest from the Input (or file stat)
  └─ ref.Verify(ctx)                   cosign, when a key is set — before any tier
  └─ modules.Get(ctx, digest, fetch)   internal/engine.Cache — the module is leased,
       │                               released after the run
       ├─ memory hit → run             (idle TTL 10 min, refreshed on every use;
       │                                LRU beyond --max-cached-modules)
       ├─ compiled/<ver>/<digest> hit  → map the file (ms, no copy) → memory → run
       └─ miss →                       one load per digest; under the cache's own
            fetch(ctx):                context, not the first requester's
              oci: manifest GET (verified against the ref) → layer digest
              ├─ modules/<blob digest> hit → bytes                   (blob hit)
              └─ miss → source (registry layer / HTTP GET / file)    (blob miss)
                        → verify sha256 → modules/<blob digest> ← write
              oci tar layer: extract the .wasm
            → wait for a compile slot (--max-concurrent-compiles, default 1)
            → Compile (~25 CPU-seconds, ~1 GB peak for a 75 MB Go guest)
            → Serialize → compiled/<ver>/<digest> ← write
            → memory → run
```

- **Memory (compiled modules).** One entry per module in use, kept while it
  is used and dropped after `engine.DefaultIdleTTL` (10 minutes) of idleness
  or, with `--max-cached-modules`, when it is the least recently used entry
  past the bound; a request after that goes to the disk artifact, not back
  to the network or the compiler. Modules are reference-counted: a dropped
  module is freed the moment its last run ends, not when the garbage
  collector eventually notices a tiny Go wrapper around 100 MB of native
  code. Loads are single-flight per digest — concurrent first requests for
  one module wait for one load, each with its own context, while the load
  itself runs under the cache's (`DefaultLoadTimeout`, 10 minutes) so a
  cancelled first requester never poisons it — and at most
  `--max-concurrent-compiles` (default 1) modules compile at once: a compile
  already uses every core, and more in flight only multiplies its ~1 GB
  peak. `--enable-memory-cache=false` turns the tier off: every request maps the
  artifact from disk (6–8 ms for the largest Go guest) and releases it.
- **Compiled artifacts on disk.** Written after every compile, mapped on
  every memory miss with `NewModuleDeserializeFile`: the code stays
  file-backed (~90 MB resident for a 75 MB Go guest, shared with the page
  cache and between processes) instead of being copied into the Go heap
  (145 MB per load). Restarts and rolling upgrades of the pod cost that map
  (6–8 ms) instead of a compile; a wasmtime upgrade costs one compile per
  module and, a day later, drops the old directory.
- **Fetched modules on disk.** Written after every successful fetch, read
  before any download. An HTTP server outage does not stop reconciling for
  modules seen before, and a rescheduled pod (with a volume) never re-pulls;
  an OCI module whose artifact is gone costs one manifest read (a few KB, to
  learn the layer digest) and no download. Served files (`module.path`) are
  not copied — they are on disk already.

Nothing keeps raw module bytes in memory beyond the compile call itself.
Warm-up (next) is the same path entered at startup instead of at a request.

### Warm-up

Ready is not warm by default: the health service reports Serving once the
caches are open, and the first request for each module on a pod pays a map
(warm volume) or a compile (cold). `--warm-modules` (repeatable, or one
comma-separated value, or `WARM_MODULES`) names modules to load first —
OCI references pinned to their manifest digest (`repo[:tag]@sha256:…`,
pulled with the runtime's Docker config; there is no step credential at
startup) and, with `--module-dir`, `path:<file>` entries. Each entry takes
the request path exactly (`cmd/function/warm.go` → the same `load` as
`RunFunction`): resolve, `Verify` (a `--cosign-key` runtime refuses an
unsigned or non-OCI entry as it would a request), then `Get` — memory, the
artifact on disk, or fetch and compile — and the lease is returned at once:
the memory tier keeps its own, and with `--enable-memory-cache=false` the
artifact on disk is the point. Ordering:

1. the engine and both caches open (a failure here still refuses to start);
2. the health service is registered as *Not Serving* and the gRPC server
   starts listening — the port is open, a probe reads Not Serving rather
   than a refused connection, and a request that arrives early is served
   cold or joins the warm load already in flight for its digest;
3. the entries load, at most `--max-concurrent-compiles` at a time (the
   compile slots: more in flight would only queue on them holding fetched
   bytes, and a warm compile takes its slot like any first request, so
   early requests for other modules queue behind it as they would behind
   each other);
4. every entry loaded or failed → *Serving*.

A failure is logged (`Cannot warm module` with the entry and the reason —
a tag instead of a digest, a missing file, an unreachable registry, bytes
that do not compile) and nothing else: readiness is never held back by an
entry, and the module it names is loaded on its first request as before.
Warm-up therefore only ever costs time between listen and Serving — one
map per entry on a warm volume, one compile per entry on a cold cache — and
a gRPC liveness probe on the same port needs a failure threshold that
outlasts the longest expected warm-up (a TCP probe does not care). A volume
under `/tmp/function-wasm-cache` remains the way to make that time short.

## Failure behaviour

- Cache directory not creatable at startup: the function refuses to start,
  naming the directory — running without caches would re-pull and recompile
  on every restart, silently.
- Disk write failure at run time (full volume, transient I/O error): ignored
  by the store's caller; the request proceeds with the fetched or compiled
  result, and the next miss tries again.
- Corrupt blob (digest mismatch on read): removed, treated as a miss.
- Corrupt or foreign artifact (wasmtime refuses it): treated as a miss and
  overwritten.
- Signature policy (`--cosign-key`) is checked before any tier is consulted,
  once per digest per process: an artifact a keyless runtime left on a
  shared or persisted volume is never served by a keyed one.
- Digest stated in the Input (or by the manifest it pins) does not match
  what the source delivers: a fatal result naming both digests; nothing is
  stored.

## Observability

`function_wasm_module_cache_events_total{cache,event}` with `cache` ∈
`compiled` (memory tier), `compiled-disk` (artifacts), `blob` (fetched
modules) and `event` ∈ `hit`, `miss`, `stale` (compiled-disk: an artifact
wasmtime refused);
`function_wasm_module_fetch_duration_seconds{source}` and
`function_wasm_module_compile_duration_seconds` time the miss paths;
`function_wasm_module_cache_bytes{cache}` is each store's size as of the
last sweep. Debug logs carry the module digest and fetch size.

## Not done, on purpose

- No eviction on disk by default: entries are immutable and named by
  digest; a Composition that stops referencing a module leaves its files
  behind, and every entry is reproducible. `--max-cache-size` turns the
  sweep on: at startup and every ten minutes the least recently used
  entries across both stores (a read counts as a use — the store touches
  what it serves) go until nine tenths of the bound is left;
  `function_wasm_module_cache_bytes` reports what each store holds. A
  mapped artifact whose file is swept keeps serving the resident module (the
  mapping outlives the name); its next miss recompiles.
- No configuration of the disk tiers: one directory, one TTL. Flags came
  out because every option was a way to run without a cache; the knobs that
  remain (`--enable-memory-cache`, `--max-cached-modules`,
  `--max-concurrent-compiles`) bound memory and CPU and keep both disk
  caches on, and `--warm-modules` only fills them earlier.
- No warm-up from a manifest of "every module ever served": what to warm is
  the operator's list, stated like a Composition states a module — a digest,
  never a tag — so a pod never resolves anything at startup either.
- Compressed tar layers are stored as delivered and decompressed on every
  artifact miss (~150 ms for a 75 MB guest); `guestfn push` and `oras push`
  produce raw `application/wasm` layers, which cost nothing to extract.
