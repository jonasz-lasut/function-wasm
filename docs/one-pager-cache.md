# Module and compiled artifacts cache

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 2.7 (revision 2.7 keys the compiled tier per process, the cache key record's design. Revision 2.6 puts the manifests tier to use, the performance audit's perf-02 under the F-5 rule. Revision 2.5 states the cache posture of the 1.0 audit's F-5, and 2.4 the compiled tier's trust boundary. Revision 2.3 rewrote the record in the present tense over the Rust workspace, where warm-up loads its entries one after another and skips the signature check, which every request makes before it reads a cache)

function-wasm keeps its caches on disk under one fixed directory, every entry
addressed by a content digest, plus a bounded in-memory tier for compiled
modules. The runtime checks each tier before it serves it. It verifies the
fetched modules and the manifests against their digest. It verifies the
compiled artifacts, native code Wasmtime loads without a check of its own,
against an HMAC under a key the process draws at startup
([the cache key record](one-pager-cache-key.md)), and compiles a module again
when another process wrote its artifact. Fetched modules are always written to disk and never held in memory.
Compiled modules are persisted as Wasmtime artifacts, mapped from disk when
needed and held in memory while they're used. An entry never has to be
invalidated: its name is its content. The flags are safety limits (how many
modules stay resident, how many compile at once), not cache tuning, and
`--warm-modules` decides when the runtime fills the caches (before readiness),
not how.

## Layout

```
/tmp/function-wasm-cache/
├── modules/                          fetched blobs, as delivered by the source
│   └── sha256-<hex>                    verified against its name on every read
├── compiled/                         Wasmtime artifacts
│   └── <engine::version()>/            e.g. rust-v<wasmtime version>-linux-aarch64
│       ├── sha256-<hex>                the artifact for the module that digest pins
│       └── sha256-<hex>.mac            its HMAC under this process's key; an entry
│                                       with no sidecar or another key's is refused
├── manifests/                        module manifests by their own digest (the manifest
│   └── sha256-<hex>                    layer's for an OCI source, manifestDigest for an HTTP
│                                       one), verified against the name on every read
└── tuf/<instance>/                   with --cosign-trust-instance: the verified TUF
                                      metadata and trusted root of that Sigstore instance,
                                      outside the LRU sweep (which walks the stores above)
```

The directory is `DEFAULT_DIR` in `crates/function/src/store.rs`, created at
startup. A pod that must keep its caches across restarts backs it with a
volume (a `DeploymentRuntimeConfig` with an `emptyDir` survives container
restarts, a PVC lasts across rescheduling). With a read-only root filesystem, the
pod must mount something writable there. A directory that can't be created
stops the runtime at startup, naming it: running without caches would re-pull
and recompile on every restart, unnoticed.

`Store` writes each entry through a temporary file and a rename. A crash
can't leave a partial entry for a later read to serve. The `modules/`
store is opened verifying: a read recomputes the digest, and a mismatch removes
the entry and counts as a miss. The compiled store can't verify by name,
since an artifact's name is its module's digest, so it verifies by key.
`put` writes an HMAC-SHA256 sidecar beside each entry under 32 bytes the
process draws at startup, and `path` checks it before it hands the path out.
A missing or wrong sidecar is a stale miss that removes the entry. The
runtime keeps the key in memory and nowhere else. A restart compiles every
module again, a volume keeps only the fetched modules and the manifests, and
the store refuses a planted artifact.

## Keys

Every key is a `sha256:<hex>` stated in the Input (or computed for a served
file, `module.path`, hashed when its size or mtime change), so resolving never
touches the network and no reference is resolved at request time:

- An OCI source is keyed by the manifest digest in its reference
  (`repo[:tag]@sha256:...`, printed by `guestfn push`; a tag alone isn't
  accepted). The manifest pins the module by stating its layer's digest. The
  registry client verifies the manifest against the reference and the layer
  against the manifest. The compiled artifact is keyed by the manifest digest;
  `modules/` keeps the layer under its own digest, as delivered. A raw WASM
  layer's digest is the module's. A tar layer is stored as a tar and
  extracted when the module is compiled, so every blob is verifiable against
  its name.
- An HTTP source is keyed by `module.http.digest`, the digest of the module
  bytes. The runtime verifies the download against it before storing or
  compiling it.

A cache entry is then exactly the content its name promises, and a module
fetched from two sources shares its blob when the bytes are the same. The
key is also everything a hit checks. A module one Composition pulled with a
registry credential is served without a fetch to any later request that
states its digest, whatever the source type. The runtime checks the
credential at the pull and never at a hit, and a digest quoted in a
Composition is no secret. The signature verdict and the module's manifest
are kept per digest per process the same way. The resolver remembers a
manifest by the digest that pins it, and a warm request doesn't contact the
registry. The raw bytes also go to the `manifests/` store under their own
digest, verified on read like every disk tier but the compiled one. A
process that lost the memory tier reads the artifact's manifest once per
digest, with the credential, to learn the layer's digest, and takes the
layer from disk. A `wasmfn.yaml` under `--module-dir` is never cached: it's
the development loop.

The compiled store is namespaced by `engine::version()`
(`crates/engine/src/lib.rs`): the Wasmtime crate version `build.rs` reads
from `Cargo.lock`, plus the OS and architecture. A serialized artifact loads
only in the Wasmtime that produced it on the same host type, and a bump moves the namespace on its own. The namespace is
distinct from the Go runtime's on purpose: the two engines' artifacts are
never assumed interchangeable. At startup `remove_stale_versions` deletes the
other version directories under `compiled/` once nothing has written them for
`STALE_VERSION_AGE` (a day). The running binary can't load them, but during a
rolling upgrade pods of both versions share the volume, and deleting each
other's artifacts on start would recompile everything twice, while a rollback
within a day finds its cache warm. Loading is defensive anyway: an artifact
that Wasmtime refuses counts as `stale` and is recompiled over.

## Tiers and the request path

```
RunFunction (crates/function/src/runner.rs)
  └─ resolver.resolve(module)          no I/O: digest from the Input (or file stat)
  └─ verifier                          cosign, when required - before any tier
  └─ cache.get(digest, fetch)          crates/function/src/cache.rs
       │
       ├─ memory hit → run             (idle TTL 10 min, refreshed on every use;
       │                                LRU beyond --max-cached-modules)
       ├─ compiled/<version>/<digest> hit → map the file (no copy) → memory → run
       └─ miss →                       one load per digest, shared by every waiter
            fetch:                     (resolver.rs)
              oci: manifest GET (verified against the ref) → layer digest
              ├─ modules/<blob digest> hit → bytes                   (blob hit)
              └─ miss → source (registry layer / HTTP GET / file)    (blob miss)
                        → verify sha256 → modules/<blob digest> ← write
              oci tar layer: extract /fn.wasm
            → wait for a compile slot (--max-concurrent-compiles, default 1)
            → compile → serialize → compiled/<version>/<digest> ← write
            → memory → run
```

- **Memory (compiled modules).** One entry per module in use, dropped after
  `DEFAULT_IDLE_TTL` (10 minutes) of idleness or, with `--max-cached-modules`,
  when it's the least recently used entry past the bound. A request after that
  goes to the artifact on disk, not back to the network or the compiler.
  Loads are single-flight per digest: concurrent first requests for one module
  wait for one load. A failed load isn't remembered, and the next request retries.
  At most `--max-concurrent-compiles` (default 1) modules compile at once. A
  compile uses every core and about a gigabyte for a large guest, and more in
  flight only multiplies that. A cold load isn't cut short by the request's
  deadline: the compile completes and caches, and the run that follows gets
  the remaining budget. `--enable-memory-cache=false` turns
  the tier off: every request maps the artifact from disk and releases it.
- **Compiled artifacts on disk.** Written after every compile, mapped on every
  memory miss with `Engine::deserialize_file`, and the code is file-backed
  and shared with the page cache instead of copied onto the heap. Restarts and
  rolling upgrades cost that map instead of a compile; a Wasmtime upgrade costs
  one compile per module and, a day later, drops the old directory.
- **Fetched modules on disk.** Written after every successful fetch, read
  before any download. An HTTP server outage doesn't stop reconciling for
  modules seen before, and a rescheduled pod with a volume never re-pulls. An
  OCI module whose artifact is gone costs one manifest read (a few KB, to
  learn the layer digest) and no download. Served files (`module.path`) aren't
  copied: they're on disk already. `guestfn push` and `oras push` produce raw
  `application/wasm` layers, which cost nothing to extract; a compressed tar
  layer is decompressed on every artifact miss.

The runtime holds raw module bytes in memory only during the compile call.
Warm-up is the same path entered at startup instead of at a request.

### Warm-up

Ready isn't warm by default: the first request for each module on a pod pays a
map (warm volume) or a compile (cold). `--warm-modules` (repeatable, one
comma-separated value, or `WARM_MODULES`) names modules to load first: OCI
references pinned to their manifest digest (pulled with the runtime's Docker
config, since there is no step credential at startup) and, with
`--module-dir`, `path:<file>` entries. `warm` in `crates/function/src/ops.rs`
resolves each entry and calls the same `cache.get` a request does, so a warmed
module is what a request would have loaded, whichever tier supplies it. It doesn't check signatures. That stays the
request's job, made before any cache is read, so a warmed module that isn't
signed is never served where a signature is required. The order:

1. The caches open. A failure here refuses to start.
2. The gRPC and health servers start listening, both reporting not serving.
   A probe reads not-ready rather than a refused connection, and a request that
   arrives early is served cold or joins the load already in flight for its
   digest.
3. With `--cosign-trust-instance`, the first trusted-root fetch runs, bounded
   and logged on failure, because a warmed module may need it.
4. The entries load one after another.
5. `/readyz` and gRPC health flip to serving.

A failure is logged (`Cannot warm module`, with the entry and the reason, such as a
tag instead of a digest or an unreachable registry) and nothing else. Readiness is never held back by an entry, and
the module it names loads on its first request as before. Warm-up costs time
between listen and serving, one map per entry on a warm volume and one compile
per entry on a cold cache, so a gRPC liveness probe on the function port needs
a failure threshold that outlasts it. A volume under
`/tmp/function-wasm-cache` is what makes that time short. The operator's view
is [readiness](operators/readiness.md).

## Failure behavior

- Disk write failure at run time (full volume, transient I/O error): ignored by
  the caller. The request proceeds with the fetched or compiled result, and the
  next miss tries again.
- Corrupt blob (digest mismatch on read): removed, treated as a miss.
- Corrupt or foreign artifact (Wasmtime refuses it): treated as a miss and counted
  `stale`. The recompile overwrites it.
- The signature check happens before any tier is consulted, once per manifest
  digest per process, so an artifact that a keyless runtime left on a shared or
  persisted volume is never served by a runtime that requires a key.
- A digest stated in the Input (or by the manifest it pins) that differs from
  what the source delivers is a fatal result stating both digests, and nothing
  is stored.

## Observability

`function_wasm_module_cache_events_total{cache,event}` with `cache` in
`compiled` (memory tier), `compiled-disk` (artifacts) and `blob` (fetched
modules), and `event` in `hit`, `miss` and `stale` (`compiled-disk` only: an
artifact Wasmtime refused). `function_wasm_module_fetch_duration_seconds{source}`
and `function_wasm_module_compile_duration_seconds` time the miss paths.
`function_wasm_module_cache_bytes{cache}` is the size of the `blob` and
`compiled-disk` stores as of the last sweep. The metrics page lists the full
series: [metrics](operators/metrics.md).

## Eviction

Disk entries aren't evicted by default: they're immutable and named by digest,
and a Composition that stops referencing a module leaves its files behind.
`--max-cache-size` (in MB) turns the sweep on. At startup and every ten minutes
it removes the least recently used entries across the three stores (a read
counts as a use: the store touches what it serves) until nine tenths of the
bound is left, so consecutive sweeps don't each remove one entry. A removal is
always safe, since every entry is reproducible; the next request fetches or
compiles again. A mapped artifact whose file is swept keeps serving the
resident module, because the open mapping doesn't depend on the file's name.

## Alternatives

**Eviction on by default.** Entries are reproducible and cheap to keep, and a
default bound would be a number the project can't choose for every volume. The
sweep is opt-in.

**Flags for the disk tiers.** Every option for them was a way to run without a
cache, so the design has one directory and one idle TTL. The flags that remain
(`--enable-memory-cache`, `--max-cached-modules`, `--max-concurrent-compiles`)
bound memory and CPU and leave both disk caches on.

**Warm-up from a list of every module ever served.** What to warm is the
operator's list, each entry a digest (never a tag) stated the way a Composition
states a module. A pod doesn't resolve anything at startup either.
