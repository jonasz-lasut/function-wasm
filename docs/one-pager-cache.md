# Module and Compiled Artifacts Cache

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.1

function-wasm keeps two caches on disk under one fixed directory, both
addressed by content digests, plus a short-lived in-memory tier for compiled
modules. Fetched modules always land on disk and are never held
in memory; compiled modules are held in memory for a while and always
persisted. Nothing is configurable and nothing has to be invalidated: an
entry's name is its content.

## Layout

```
/tmp/function-wasm-cache/
├── modules/                          fetched blobs, as delivered by the source
│   └── sha256-<hex>                    verified against its name on every read
└── compiled/                         wasmtime artifacts (Module.Serialize)
    └── <wasmtime-major>-<os>-<arch>/   e.g. v47-linux-arm64
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
`<wasmtime-go major from the import path>-<GOOS>-<GOARCH>` — because a
serialized artifact is only loadable by the wasmtime that produced it on
the same host type. At startup every other version directory under
`compiled/` is removed; the running binary could not load it, and it would
only take space. Loading is defensive anyway: a corrupt or foreign artifact
fails wasmtime's own validation, counts as a miss, and is overwritten by a
fresh compile.

## Tiers and the request path

```
RunFunction
  └─ resolver.Resolve(module)          no I/O: digest from the Input (or file stat)
  └─ modules.Get(digest, fetch)        internal/engine.Cache
       ├─ memory hit → run             (idle TTL 10 min, refreshed on every use)
       ├─ compiled/<ver>/<digest> hit  → Deserialize (ms) → memory → run
       └─ miss →
            fetch():                   internal/module.Ref.Fetch
              oci: manifest GET (verified against the ref) → layer digest
              ├─ modules/<blob digest> hit → bytes                   (blob hit)
              └─ miss → source (registry layer / HTTP GET / file)    (blob miss)
                        → verify sha256 → modules/<blob digest> ← write
              oci tar layer: extract the .wasm
            → Compile (2–3 s for a 75 MB Go guest)
            → Serialize → compiled/<ver>/<digest> ← write
            → memory → run
```

- **Memory (compiled modules).** One entry per module in use, kept while it
  is used and dropped after `engine.DefaultIdleTTL` (10 minutes) of idleness;
  a request after that goes to the disk artifact, not back to the network or
  the compiler. Loads are single-flight per digest: concurrent first requests
  for one module wait for one load. `--disable-memory-cache` turns this tier
  off: nothing is retained between requests and every request deserializes
  the artifact from disk (milliseconds) — the right trade for runtimes
  serving large Go modules, whose compiled form is well over 100 MB each,
  while Rust or TinyGo modules are small enough that the tier costs nothing.
- **Compiled artifacts on disk.** Written after every compile, read on every
  memory miss. Restarts and rolling upgrades of the pod cost a deserialize
  (~15 ms for the 143 MB artifact of a 73 MB Go guest) instead of a compile;
  a wasmtime upgrade costs one compile per module and drops the old directory.
- **Fetched modules on disk.** Written after every successful fetch, read
  before any download. An HTTP server outage does not stop reconciling for
  modules seen before, and a rescheduled pod (with a volume) never re-pulls;
  an OCI module whose artifact is gone costs one manifest read (a few KB, to
  learn the layer digest) and no download. Served files (`module.path`) are
  not copied — they are on disk already.

Nothing keeps raw module bytes in memory beyond the compile call itself.

## Failure behaviour

- Cache directory not creatable at startup: the function refuses to start,
  naming the directory — running without caches would re-pull and recompile
  on every restart, silently.
- Disk write failure at run time (full volume, transient I/O error): logged
  by the store's caller as ignored; the request proceeds with the fetched or
  compiled result, and the next miss tries again.
- Corrupt blob (digest mismatch on read): removed, treated as a miss.
- Corrupt or foreign artifact (wasmtime refuses it): treated as a miss and
  overwritten.
- Digest stated in the Input (or by the manifest it pins) does not match
  what the source delivers: a fatal result naming both digests; nothing is
  stored.

## Observability

`function_wasm_module_cache_events_total{cache,event}` with `cache` ∈
`compiled` (memory tier), `compiled-disk` (artifacts), `blob` (fetched
modules) and `event` ∈ `hit`, `miss`;
`function_wasm_module_fetch_duration_seconds{source}` and
`function_wasm_module_compile_duration_seconds` time the miss paths. Debug
logs carry the module digest and fetch size.

## Not done, on purpose

- No size bound or eviction on disk: entries are immutable and named by
  digest; a Composition that stops referencing a module leaves its files
  behind. Bound the volume, or clear the directory — every entry is
  reproducible. A `guestfn`-style janitor could come later if it hurts.
- No configuration of the disk tiers: one directory, one TTL. Flags came
  out because every option was a way to run without a cache; the only knob
  is `--disable-memory-cache`, which trades milliseconds for memory and still
  keeps both disk caches.
