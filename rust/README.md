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
  and the run flow of `cmd/function/fn.go`.

```sh
cargo build
cargo test
cargo run -p function-wasm -- --insecure --module-dir ../examples/hello-go
```

## Parity contract

Where a check is implemented, its refusal string is the Go runtime's,
verbatim - the strings are the conformance surface (`cmd/function/testdata/
validate/`, the troubleshooting table in AGENTS.md). Anything the port does
not carry yet is refused with a message naming it, never silently ignored,
so nothing runs wider than the Go runtime would allow.

Serving today: `module.type: Path` sources under `--module-dir`,
`limits.timeout` / `limits.memory` against the `--module-timeout` /
`--module-memory-limit` ceilings, the full ABI v1 run mechanics (including
the in-band `wasmfn.http` refusal for modules with no egress grant), fatal
results for every guest failure, and the meta fill for guests that omit it.

## Not ported yet (refused or absent, in rough order of the plan)

- OCI and HTTP module sources, cosign verification, registry credentials
- The module manifest and the three-layer capability decision (Cedar on
  both layers, `AdmitRequires`); with no manifests there are no sandbox
  grants, so every module gets the default sandbox and `wasmfn.http` answers
  with the no-grant refusal
- `module.from`, `compositionPolicy`, `limits.concurrency`
- The egress client (SSRF block list, budgets, rate limit, audit lines)
- The disk caches (fetched blobs, serialized artifacts, manifests), idle
  TTL, LRU bounds, `--warm-modules`, `/readyz`
- `function validate`, metrics, `--max-concurrent-runs`,
  `--max-total-run-memory`, fair scheduling
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
