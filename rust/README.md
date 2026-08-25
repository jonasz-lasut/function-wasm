# The Rust runtime (initial implementation)

The Rust port of the function-wasm runtime, per `docs/one-pager-abi-v2.md`:
the host moves to Rust (native wasmtime and, later, native Cedar) to enable
ABI v2 on the component model. This tree is the phase-1 start: a
feature-subset, contract-faithful runtime; `guestfn` is deliberately not
ported yet.

Two crates:

- `crates/engine` - the wasmtime engine: compiles and ABI-checks modules,
  runs each request in a fresh store (WASI argv `["function"]`, epoch
  deadline, memory limiter, optional private `/tmp` and env), and serves the
  `wasmfn.log` and `wasmfn.http` host imports. It works on request/response
  bytes; the protobuf codec is the caller's.
- `crates/function` - the gRPC function (binary `function`), on
  [function-sdk-rust](https://crates.io/crates/function-sdk-rust): Input
  admission, the Path source resolver, an in-memory compiled-module cache,
  the run flow of `cmd/function/fn.go`, and the `validate` subcommand -
  the same offline admission as the Go tool, over the same files and flags.

```sh
cargo build
cargo test
cargo run -p function-wasm -- --insecure --module-dir ../examples/hello-go
cargo run -p function-wasm -- validate ../cmd/function/testdata/validate/ok.yaml
```

## Parity contract

Where a check is implemented, its refusal string is the Go runtime's,
verbatim - the strings are the conformance surface. Anything the port does
not carry yet is refused with a message naming it, never silently ignored,
so nothing runs wider than the Go runtime would allow.

**The claim is measured, not asserted**: `tests/conformance.rs` builds the
Go runtime from this repository and runs its `function validate` as the
reference against the Rust binary - same fixtures
(`cmd/function/testdata/validate/`), same flags, same stdin - and diffs
stdout, stderr and the exit code. Every case either matches byte-for-byte
(a mismatch is a parity regression) or sits on a known-gaps list with a
recorded reason and is required to keep differing - a gap that closes must
be removed from the list, so the list only ever shrinks deliberately. The
suite skips without a Go toolchain, like the Go tree's guest tests skip
without theirs.

Matching today: admitted steps with limits details and warnings, the
unknown-field warnings, every non-Cedar refusal family, tool failures
(unreadable, unparsable and missing files - down to Go's os error wording,
emulated), stdin, --function-name, the whole `--resolve` Path flow (digest,
size, ABI verdict, host imports, the manifest summary) in both text and
JSON output, and the policy layers themselves: the `compositionPolicy`
Cedar layer (boundary-correct `pullModule`/`spendCredential` fences,
`module.from` materialisation against an XR, docker.io reference
normalization included), the operator `--sandbox-policy-file` (grants, the
per-repository `requireSignature` decision, the SSRF `dialAddress` rule
compiler with Go's exact load errors), and the full three-layer manifest
decision over `manifestPath` modules - a private `/tmp` and env bindings
are granted, refused and materialised exactly as the Go runtime does.
Two known gaps remain: egress that every layer permits (the egress client
is not ported, refused as "no egress mechanism"), and one permanent
wording divergence per embedded library message (cedar parse errors, Go's
json decoder).

Serving today: `module.type: Path` sources under `--module-dir`,
`limits.timeout` / `limits.memory` against the `--module-timeout` /
`--module-memory-limit` ceilings, the full ABI v1 run mechanics (including
the in-band `wasmfn.http` refusal for modules with no egress grant), fatal
results for every guest failure, and the meta fill for guests that omit it.

## Not ported yet (refused or absent, in rough order of the plan)

- OCI and HTTP module sources (fetching; their admission, locations and
  policy fences are ported), cosign verification, registry credentials
- The egress client (SSRF block list judged per resolved address, budgets,
  rate limit, audit lines); egress the layers permit is refused as
  "no egress mechanism" until it lands
- The disk caches (fetched blobs, serialized artifacts, manifests), idle
  TTL, LRU bounds, `--warm-modules`, `/readyz`
- `limits.concurrency`, metrics, `--max-concurrent-runs`,
  `--max-total-run-memory`, fair scheduling
- minRuntime enforcement runs against an empty version (the Go development
  build's behaviour) until the release packaging stamps one
- ABI v2 (the component world) - lands in this tree once the v1 base holds

## Known divergences from the Go runtime

- The gRPC layer decodes the request into generated prost types and the
  runtime re-encodes them for the guest, so fields newer than the vendored
  proto are dropped rather than forwarded (Go's protobuf keeps unknown
  fields). The transparent-proxy property needs a raw-bytes path at the
  tonic codec layer; tracked for a later phase.
- The request context's deadline (gRPC timeout) does not yet bound a run or
  its queue waits; only `--module-timeout` and `limits.timeout` do.
- Guest log records are forwarded with their keys and values rendered as one
  JSON `kv` field (tracing has no dynamic fields), not as first-class
  structured fields.
- Parse errors for `limits.timeout` / `limits.memory` word their messages
  differently (the checks and ceiling refusals match verbatim).
