# Compiled-artifact cache key

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: draft (proposed for 1.0.0 by the 1.0 security audit, finding F-2)

The compiled tier of the cache, `/tmp/function-wasm-cache/compiled/<version>/`,
contains Wasmtime artifacts: native code the runtime maps and runs. The
runtime trusts those files by name. It loads an entry named by a module's
digest through `Engine::deserialize_file`, whose soundness rests on the file
being the runtime's own output, and nothing checks the file's origin. Anyone with
write access to the directory can plant an artifact under a legitimate
digest. The runtime then runs that code once the fetch, the digest check and
the signature check have passed for the module the digest names, because
those checks look at the module and never at the artifact. This record
proposes an opt-in key that makes the tier tamper-evident, so a persisted
cache volume needs no more trust than the runtime binary.

## Background

The fetched-module store hashes a blob against its name before serving it
(`crates/function/src/store.rs`). The compiled store has no such check. An
artifact's name is the digest of the module the runtime compiled it from,
which says nothing about the artifact's own bytes. Wasmtime validates an
artifact's header and version, and nothing about its origin. The 1.0
contract states the boundary instead: the cache directory is the runtime's
alone, backed by an `emptyDir` or a volume that only this Function's pods
mount ([trust model](operators/trust-model.md)). That rules out a cache
shared with another workload, and an operator keeps the rule by hand.

## Design

A flag, `--cache-key-file <path>` (environment variable `CACHE_KEY_FILE`),
names a file of at least 32 bytes that the operator mounts from a Secret.
With the flag set:

- `Store::put` for the compiled store writes, beside each entry, an
  HMAC-SHA256 of the artifact's bytes under the key, as `<name>.mac`.
- Reads go through a `verified_path` that recomputes the MAC over the file
  and compares it to the sidecar before handing the path to the engine. A
  missing or wrong MAC counts as the stale miss the store already reports,
  `cache_events_total{cache="compiled",event="stale"}`, and the runtime
  compiles the module again and rewrites the entry with its MAC.
- The key stays in the process. The runtime reads it once at startup and
  keeps it out of the logs and out of the cache directory.
- The sweep treats `<name>.mac` as part of its entry: the two go together,
  and the `cache_bytes` gauge leaves the sidecar out, as it leaves out the
  temporary-file markers.

Unset, nothing changes: the flag is additive, and a runtime without it keeps
the 1.0 behaviour and the documented boundary. The `sha2` crate is already a
dependency. The `hmac` crate is one more.

An operator creates a Secret from 32 random bytes, mounts it at
`/etc/function-wasm/cache.key` through the `DeploymentRuntimeConfig` and
sets `--cache-key-file=/etc/function-wasm/cache.key`. Under that key a
planted artifact has no sidecar, or a sidecar computed under another key,
and the runtime compiles over it. Rotating the key invalidates every artifact at once, which
costs one compile per module, the same as a Wasmtime upgrade.

## Alternatives

Storing the module's digest inside the artifact, or beside it, binds
nothing: whoever can plant an artifact can plant the digest with it. Only a
secret the writer of a planted file doesn't hold distinguishes the runtime's
output from anyone else's, which is what the keyed MAC is.

A key required whenever the cache is on a persisted volume wouldn't need an
operator to opt in. The mount point gives the runtime no way to distinguish
an `emptyDir` from a PVC, and a required flag changes a default, which the
compatibility promise makes a major after 1.0.0, while an opt-in flag is
additive at any minor.

A per-process random key, with no flag, would protect a persisted volume
across restarts only by invalidating every artifact on each restart, which is
the cold start the persisted volume exists to avoid.

Signing the artifacts with the operator's cosign key would tie the cache to
the signature path and cost a public-key verification per load. The MAC is
symmetric and local. No party in the trust model needs to verify the
runtime's own artifacts except the runtime.

## Trade-offs

A MAC over the whole artifact is one pass over the file per cold load from
disk, proportional to the artifact's size (tens of megabytes for a large Go
module). The runtime pays it once per process per module, since the memory
tier keeps the mapped module from then on. The sidecar doubles the number of
files in the compiled store. An operator who loses the Secret loses the
cache's warmth and keeps its correctness.

## Tests

A unit test in `store.rs` checks that a keyed store refuses a planted
artifact and accepts one it wrote. The audit's reproduction becomes a
permanent test in `cache.rs`. With no key, the cache serves an artifact
placed under a digest without calling `fetch`. With a key and no MAC, the
cache compiles the module instead. The end-to-end lifecycle profile, whose cache is a PVC,
runs with a key.

## Contract impact

One new flag, unset by default. The refusal wording and the goldens stay as
they are. The trust model's cache paragraph and the flags page's cache
storage section describe the key once the flag exists.

## Open questions

- Whether the lifecycle end-to-end profile sets the key by default and
  exercises a persisted cache with the MAC, or runs with and without it.
