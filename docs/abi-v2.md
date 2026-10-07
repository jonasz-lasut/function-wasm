# function-wasm guest ABI v2

ABI v2 is the component-model guest contract: a guest is a WebAssembly
**component** targeting the WIT world `wasmfn:function@2.0.0-draft`
([`wit/wasmfn-function.wit`](../wit/wasmfn-function.wit)), run by the same
runtime that serves [ABI v1](abi.md) core modules. The payload contract is
v1's, unchanged - protobuf `RunFunctionRequest` bytes in,
`RunFunctionResponse` bytes out - so payload evolution stays protobuf's job.
What the component model removes is v1's mechanics: `wasmfn_alloc`, the
`ptr<<32|len` packing and the re-entrant allocator are gone, because the
canonical ABI owns memory movement.

The world is a **draft**: it freezes at `wasmfn:function@2.0.0` no earlier
than `wasm32-wasip3`'s tier-2 promotion in Rust
(`docs/one-pager-abi-v2.md`). Until then a runtime release may require
guests rebuilt against the current draft.

## Getting the world

Guest toolchains read a local `wit/` directory, so a guest carries the
world as a file. `guestfn init --lang rust` writes it to
`wit/deps/wasmfn-function.wit`, byte-identical to the runtime's copy, and
the guest's own world, `wit/world.wit` in a `local:guest` package, includes
it beside whatever the guest imports (the rust scaffold adds the
`wasi:http` client). Keep the contract's file unmodified: a WIT package
that appears twice must have the same contents, so a guest's additions
belong in its own world. The `ts`, `python`, `c`, `zig` and `go` scaffolds
restate the world in their `wit/world.wit` instead, with `run` declared
sync - the shape jco and componentize-py lift today, the one the
wit-bindgen C bindings the `c` and `zig` scaffolds share take, and the one
stock Go builds through wit-bindgen's Go bindings - which satisfies the
world (below).

From `2.0.0` on, a release that brings a new world version also publishes
it, signed and attested:

- as an OCI artifact in the CNCF Wasm OCI layout (wkg's format):
  `ghcr.io/jonasz-lasut/wasmfn/function:<version>`;
- as `wasmfn-wit-<version>.tar.gz` on that GitHub release.

A pre-release world (the current `2.0.0-draft`) is never published, and a
published version is never pushed again. To fetch it with
[wkg](https://github.com/bytecodealliance/wasm-pkg-tools), map the
namespace in wkg's configuration:

```toml
[namespace_registries]
wasmfn = { registry = "wasmfn", metadata = { preferredProtocol = "oci", "oci" = { registry = "ghcr.io", namespacePrefix = "jonasz-lasut/" } } }
```

`wkg get wasmfn:function@2.0.0 --format wit` then writes
`wasmfn_function@2.0.0.wit`, the file a guest keeps under `wit/deps/`.

## Detection

The binary format is the ABI version: a core module (layer 0 in the wasm
header) is ABI v1, a component (layer 1) is ABI v2. There is no flag and no
Input field; the runtime reads it off the first eight bytes. A module
manifest declares `abi: 2`, and the runtime refuses a manifest whose
declaration does not match the binary
(`manifest says abi: 2, but the module is a core module (ABI v1)`).

## The world

```wit
package wasmfn:function@2.0.0-draft;

world function {
    enum log-level { debug, info, warn, error }

    import log: func(level: log-level, msg: string, kv: list<tuple<string, string>>);

    export run: async func(request: list<u8>) -> result<list<u8>, string>;
}
```

- **`run`** - one request. The host passes the caller's raw
  `RunFunctionRequest` bytes (the step credentials the module was not
  granted, the pull credential always among them, edited out, as for v1) and returns the guest's `RunFunctionResponse` bytes verbatim.
  `run` is `async`: a guest may await its imports (`wasi:http` above all)
  while the host meters its compute. A **sync-lifted** implementation also
  satisfies the world - the canonical ABI accepts a sync function where an
  async one is expected - which is what keeps stable, pre-wasip3 toolchains
  usable.
- **`log`** - v1's `wasmfn.log` with the JSON payload replaced by typed
  values. The host attaches the module's identity to every line and renders
  it at the runtime's level of the same name: `debug` lines only under
  `--debug`, `info`, `warn` and `error` always. `warn` and `error` are v2's
  own (a v1 line is debug or info) and joined the draft before the freeze:
  the runtime typechecks the enum exactly, so a new case refuses every
  guest built without it - a component built against the two-level draft
  must be rebuilt.
- **WASI** - the world names no WASI imports; a guest brings whatever its
  toolchain emits. The host links WASI 0.3 and WASI 0.2 (components built for
  WASI 0.2 - jco, componentize-py, Rust's `wasm32-wasip2` - import 0.2
  interfaces; a `wasm32-wasip3` build imports 0.3 only), under the
  same sandbox as v1: no network sockets, no filesystem beyond the granted
  private `/tmp`, env exactly as granted.
- **HTTP egress** - through `wasi:http/client@0.3.0` (`send: async func`),
  and equally through `wasi:http@0.2`'s `outgoing-handler` (what
  componentize-js's `fetch()` reaches for) - both generations are served by
  one host bridge - implemented over the same egress policy as v1's
  `wasmfn.http`:
  the three-layer grant decides whether `send` is backed by the policy
  client or refuses, and the SSRF block list, budgets, rate limit, audit
  line and `http_requests_total` metric apply unchanged. Bodies are
  complete on both sides, as under v1: the response budget acts on whole
  responses. A failure the host reports - the grant refusal, a blocked
  address, a budget, a transport error - reaches the guest as the
  `internal-error` code carrying exactly the string v1 put in the wire's
  `error` field: the refusal wording is contract, and no other `error-code`
  variant carries a reason. Time the guest spends awaiting `send` is
  credited back to its compute deadline, as v1 credits `wasmfn.http`.

## Errors

A guest that can produce a response encodes failures into it (a fatal
`Result`), exactly as under v1. ABI v2 adds a third channel v1 does not
have: `run` returning `err(string)`. The host turns it into the request's
fatal result as `module <desc> failed: run returned an error: <string>` -
use it for failures that happen before a response can be built (a codec
that cannot even decode the request). Traps, deadline interrupts and memory
denials behave exactly as v1's: fatal results from the host naming the
module, never gRPC errors.

## Sandbox and limits

The three-layer capability decision, `limits`, the epoch deadline
(`limits.timeout` metering guest compute, the request's gRPC deadline the
hard cap), and the memory ceiling all apply unchanged. One accounting
difference: a component exports no top-level memory, so nothing is reserved
from `--max-total-run-memory` before the run - its whole footprint is
charged incrementally as the guest's memories grow, and a growth the pool
cannot serve fails inside the run (`memory.grow` returns -1) rather than
before it.

## Compatibility

Payload evolution is protobuf's (and the world's types are additive-only
while draft). A mechanics change is a new world version. ABI v1 modules
keep running unmodified on the same runtime until 1.0.0 (v1 is deprecated
from v0.6.0, #114 and #129); nothing about v1's mechanics changed when v2
arrived.
