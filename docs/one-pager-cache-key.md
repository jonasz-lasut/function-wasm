# Compiled-artifact cache key

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.0 (a key drawn per process and no flag, for finding F-2 of the 1.0 security audit)

The compiled tier of the cache, `/tmp/function-wasm-cache/compiled/<version>/`,
contains Wasmtime artifacts: native code the runtime maps and runs through
`Engine::deserialize_file`, whose soundness rests on the file being the
runtime's own output. The fetched-module and manifest stores verify what
they read against the digest in the file's name. The compiled store can't do
that, because an artifact's name is the digest of the module the runtime
compiled it from, which says nothing about the artifact's own bytes. Anyone
with write access to the directory could plant an artifact under a
legitimate digest, and the runtime would run it once the module's own checks
had passed. The store now refuses every artifact it didn't write itself.

## Design

The runtime draws 32 random bytes at startup (`getrandom`) and keys the
compiled store with them (`store.rs`, `Store::with_key`). It keeps the key
in memory and writes it nowhere, logs it nowhere and shares it with nothing.

- `put` writes the artifact, then an HMAC-SHA256 of its bytes under the key
  as `<name>.mac` beside it, each through a temporary file and a rename. A
  crash between the two leaves an entry with no sidecar, and the next lookup
  refuses it.
- `path` reads the entry and its sidecar, recomputes the MAC and compares
  it in constant time before it hands the path to the engine. A missing or
  wrong sidecar is `Lookup::Stale`: the store removes both files, the cache
  counts `cache_events_total{cache="compiled-disk",event="stale"}` and
  compiles the module again, writing a new entry under its own key.
- The sweep and the `cache_bytes` gauge skip sidecars, and the sweep removes
  a sidecar with its entry.

A restart draws a new key. The previous process's artifacts on a persisted
volume fail the check once, and the runtime compiles the modules again. The
volume still spares the pod the fetch: the modules store serves the bytes,
verified against their digest. Pods must not share a volume, since each
would refuse and overwrite the other's artifacts. Each pod gets its own.

## Decision

Jonasz (2026-10-28) chose a key drawn at startup over a key mounted from a
Secret behind a flag. A flag means one more thing for every operator to
configure, and he accepted the price of the per-process key: a cold compile
of every module after a restart, with or without a persisted volume. The
audit's draft of this record proposed the opt-in `--cache-key-file`.

## Alternatives

Storing the module's digest inside the artifact, or beside it, binds
nothing: whoever can plant an artifact can plant the digest with it. Only a
secret the writer of a planted file doesn't hold distinguishes the runtime's
output from anyone else's.

A key mounted from a Secret (`--cache-key-file`) would keep a persisted
volume's artifacts valid across a restart. It also means one more flag and
one more Secret for every operator, and a key outside the process is a key
someone can read. Rejected (Jonasz, 2026-10-28).

Signing the artifacts with the operator's cosign key would tie the cache to
the signature path and cost a public-key verification per load. The MAC is
symmetric and local. No party in the trust model needs to verify the
runtime's own artifacts except the runtime.

## Trade-offs

A MAC over the whole artifact is one pass over the file per cold load from
disk, proportional to the artifact's size (tens of megabytes for a large Go
module). The runtime pays it once per process per module, since the memory
tier keeps the mapped module from then on. The sidecar doubles the number of
files in the compiled store. The compile after a restart is the accepted
price. `--warm-modules` moves it ahead of readiness.

## Tests

`store.rs` checks that a keyed store serves what it wrote and refuses an
entry under another key, a planted entry and a tampered one, removing what
it refuses. `cache.rs` makes the audit's reproduction permanent. The cache
refuses an artifact planted under a digest and fetches and compiles the
module, and a second cache under the same key serves the entry the compile
wrote. The lifecycle end-to-end profile, whose cache is a PVC, checks that a
restart reuses the fetched module and refuses the previous process's
artifact.

## Contract impact

An operator configures nothing, and the refusal wording and the goldens are
the same as before. The trust model's cache section, the flags page's cache storage section, the sizing
and readiness pages describe the per-process key and what a volume keeps.
