# function-wasm guest ABI v2

ABI v2 is the guest contract of function-wasm: a guest is a WebAssembly
**component** targeting the WIT world `wasmfn:function@2.0.0`
([`wit/wasmfn-function.wit`](../wit/wasmfn-function.wit)). The payload is
protobuf: `RunFunctionRequest` bytes in, `RunFunctionResponse` bytes out,
so payload evolution stays protobuf's job; the canonical ABI owns memory
movement, so a guest carries no allocator export and no pointer packing of
its own. It is the only ABI the runtime serves: ABI v1 (a wasip1 core
module exporting `wasmfn_run`, with JSON host imports) was deprecated in
v0.6.0 and removed in 1.0.0 (#114, #129); the runtime refuses a core
module at load (below).

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

The runtime reads the binary format off the first eight bytes of the
module: a component (layer 1 in the wasm header) is loaded and typechecked
against the world; a core module (layer 0) is refused with one sentence,
whatever it exports:

```
module is a core module, which function-wasm 1.0.0 no longer runs (ABI v1 was removed); build it as an ABI v2 component (docs/abi-v2.md)
```

The runtime reports it as a fatal result (`cannot load module <desc>: …`),
and `guestfn build`, `guestfn inspect` and `function validate --resolve`
print the same sentence. There is no flag and no Input field. A module
manifest declares `abi: 2`; a manifest declaring any other value is refused
before the module is looked at (`abi must be 2 (this runtime implements
ABI v2 only; ABI v1 was removed in function-wasm 1.0.0), got 1`).

## The world

```wit
package wasmfn:function@2.0.0;

world function {
    enum log-level { debug, info, warn, error }

    import log: func(level: log-level, msg: string, kv: list<tuple<string, string>>);

    export run: async func(request: list<u8>) -> result<list<u8>, string>;
}
```

- **`run`** - one request. The host passes the caller's raw
  `RunFunctionRequest` bytes (the step credentials the module was not
  granted, the pull credential always among them, edited out) and returns
  the guest's `RunFunctionResponse` bytes verbatim. `run` is `async`: a
  guest may await its imports (`wasi:http` above all) while the host meters
  its compute. A **sync-lifted** implementation also satisfies the world -
  the canonical ABI accepts a sync function where an async one is expected -
  which is what keeps stable, pre-wasip3 toolchains usable.
- **`log`** - structured logging through the host, typed values rather than
  a payload the guest encodes. The host attaches the module's identity to
  every line and renders it at the runtime's level of the same name:
  `debug` lines only under `--debug`, `info`, `warn` and `error` always.
  The runtime typechecks the enum exactly, so a new case refuses every
  guest built without it - a component built against the two-level draft
  (`debug`, `info`) must be rebuilt.
- **WASI** - the world names no WASI imports; a guest brings whatever its
  toolchain emits. The host links WASI 0.3 and WASI 0.2 (components built for
  WASI 0.2 - jco, componentize-py, Rust's `wasm32-wasip2`, the wasip1
  adapter `guestfn build` links into a Go, C or Zig guest - import 0.2
  interfaces; a `wasm32-wasip3` build imports 0.3 only), under the sandbox:
  no network sockets, no filesystem beyond the granted private `/tmp`
  (the only pre-opened directory, mounted at `/tmp`), env exactly as
  granted, `argv` `["function"]`.
- **HTTP egress** - through `wasi:http/client@0.3.0` (`send: async func`),
  and equally through `wasi:http@0.2`'s `outgoing-handler` (what
  componentize-js's `fetch()` reaches for) - both generations are served by
  one host bridge over the runtime's egress policy: the three-layer grant
  decides whether a send is backed by the policy client or refused, and the
  SSRF block list, budgets, rate limit, audit line and `http_requests_total`
  metric apply to every request. Bodies are complete on both sides: the
  response budget acts on whole responses. A failure the host reports - the
  grant refusal, a blocked address, a budget, a transport error - reaches the
  guest as the `internal-error` code carrying the refusal string (the
  wording is contract, held by the conformance goldens; no other
  `error-code` variant carries a reason). Time the guest spends awaiting a
  send is credited back to its compute deadline.

## Errors

A guest that can produce a response encodes failures into it (a fatal
`Result`). `run` returning `err(string)` is the channel for failures that
happen before a response can be built (a codec that cannot even decode the
request): the host turns it into the request's fatal result as
`module <desc> failed: run returned an error: <string>`. Traps, WASI exits,
deadline interrupts and memory denials are fatal results from the host
naming the module (`run failed: trap: …`, `run failed: module exited with
status N`, `run failed: module exceeded its execution deadline (…)`), never
gRPC errors.

## Sandbox and limits

The three-layer capability decision, `limits`, the epoch deadline
(`limits.timeout` metering guest compute, the request's gRPC deadline the
hard cap), and the memory ceiling apply to every run. A component exports
no top-level memory, so nothing is reserved from `--max-total-run-memory`
before the run: its whole footprint, the initial memory included, is
charged as the guest's memories are claimed, and a growth the pool cannot
serve fails inside the run (`memory.grow` returns -1) rather than before
it.

## Compatibility

Payload evolution is protobuf's (and the world's types are additive-only
while draft). A mechanics change is a new world version; the runtime names
the world it serves in every typecheck refusal (`component does not
implement the wasmfn:function@2.0.0 world: …`).
