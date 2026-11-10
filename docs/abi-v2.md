# Guest ABI v2

This page is for people who write a guest toolchain or its glue. It states what the runtime requires of a module and what it provides, so you can build to the contract exactly. It assumes you know WIT and the component model; to write a function, start at [Write a module](wasm/write-a-module.md). Grants are on [Cedar policy](operators/cedar-policy.md), the Input on the [Input reference](operators/input-reference.md) and the manifest on [Module manifests](guests/module-manifests.md).

A guest is a WebAssembly component targeting a world of the WIT package `guestfn:function@1.0.0` ([`wit/guestfn-function.wit`](https://github.com/jonasz-lasut/guestfn/blob/main/wit/guestfn-function.wit)). The payload is Protobuf: `RunFunctionRequest` bytes in, `RunFunctionResponse` bytes out. The canonical ABI moves memory, so a guest has no allocator export. ABI v2 is the only ABI the runtime serves.

## The world

```wit
package guestfn:function@1.0.0;

world function {
    enum log-level { debug, info, warn, error }

    import log: func(level: log-level, msg: string, kv: list<tuple<string, string>>);

    export run: async func(request: list<u8>) -> result<list<u8>, string>;
}
```

- `run` handles one request. The host passes the caller's raw `RunFunctionRequest` bytes and returns the guest's `RunFunctionResponse` bytes verbatim. The host edits out the step credentials the module wasn't granted, and the pull credential in every case. `run` is `async`, so a guest may await its imports, `wasi:http` especially. The host meters the guest's compute while it does. A sync-lifted `run` also satisfies the world, because the canonical ABI accepts a sync function where an async one is expected.
- `log` is structured logging through the host, with typed values. The host attaches the module's identity to every line and renders it at the runtime's level of the same name. `debug` lines appear only under `--debug`.
- The runtime typechecks `log-level` exactly, so `log` changes only with `2.0.0`.
- `function-sync`, the package's second world, is this contract with `run` declared sync, for a toolchain that can't async-lift it. The runtime checks every guest against `function`.

## WASI and the sandbox

The world doesn't import WASI. A guest brings whatever its toolchain emits, and the host links both WASI generations:

- WASI 0.2: components built by jco, componentize-py or Rust's `wasm32-wasip2`, and a guest that imports `wasi_snapshot_preview1`, the Go scaffold's, through the wasip1 adapter `guestfn build` links in.
- WASI 0.3: a `wasm32-wasip3` build.

The engine accepts WebAssembly 2.0 plus the `extended-const` and
`relaxed-simd` proposals. Relaxed SIMD runs with its deterministic semantics,
and a module computes the same result on every host. The engine refuses the
other WebAssembly 3.0 proposals at compile: threads, GC, function references,
exceptions, tail calls, multi-memory and memory64. No guest toolchain in the matrix emits any of
them (every scaffold and example was measured with `wasm-tools validate` on
2026-10-28), and a proposal can be enabled later as a minor.

The sandbox allows:

- no network sockets;
- no filesystem beyond the granted private `/tmp`, the only pre-opened directory;
- environment exactly as granted;
- `argv` is `["function"]`.

The [three-layer decision](operators/cedar-policy.md) grants these capabilities. `limits` and the deadlines apply to every run: `limits.timeout` meters guest compute, and the request's gRPC deadline is the hard cap. A component has no top-level memory export, and the host reserves nothing from `--max-total-run-memory` before the run. It charges the whole footprint as the guest claims its memories, and a growth the pool can't serve fails inside the run: `memory.grow` returns `-1`.

### HTTP egress

A guest sends HTTP requests through `wasi:http/client@0.3.0` (`send: async func`) or `wasi:http@0.2`'s `outgoing-handler`, which componentize-js's `fetch()` uses. One host bridge serves both. The runtime's egress client performs each send or refuses it under the grant. The policy is on [HTTP egress](operators/http-egress.md).

- Bodies are complete on both sides, and the response budget applies to whole responses.
- The guest receives a host-reported failure as the `internal-error` code, whose payload is the refusal string, for a grant refusal, a blocked address, an exhausted budget or a transport failure. The wording is contract, held by the conformance goldens.
- The time a guest spends awaiting a send is credited back to its compute deadline.

## Errors

A guest that can produce a response encodes failures into it as a fatal `Result`. `run` returning `err(string)` is for failures before a response can be built, such as a codec that can't decode the request. The host turns it into the request's fatal result: `module <desc> failed: run returned an error: <string>`.

Traps, WASI exits, deadline interrupts and memory denials are fatal results from the host that name the module, never gRPC errors:

- `run failed: trap: …`
- `run failed: module exited with status N`
- `run failed: module exceeded its execution deadline (…)`

## Detection

The runtime reads the binary format from the first eight bytes of the module. A component (layer 1 in the WASM header) is loaded and typechecked against the world. A core module (layer 0) is refused with one sentence, whatever it exports:

```
module is a core module, which function-wasm 1.0.0 no longer runs (ABI v1 was removed); build it as an ABI v2 component (docs/abi-v2.md)
```

The runtime reports it as a fatal result (`cannot load module <desc>: …`), and `guestfn build`, `guestfn inspect` and `function validate --resolve` print the same sentence. Detection has no flag and no Input field.

A typecheck refusal begins `component does not implement the guestfn:function@1.0.0 world: …`.

A module manifest declares `contract: wasm@1.0`, this world. Another version is refused before the module is looked at: `contract wasm@0.9 is not one the wasm engine implements: wasm@1.0`.

## Versioning

`guestfn:function@1.0.0` is the contract from function-wasm 1.0.0 on. The world's version moves on its own, by what a change asks of guests:

- A 1.x minor may only add imports: a new interface or a new function. A component imports only what it uses. The runtime serves a guest built against an earlier 1.x unchanged. A guest that imports the addition declares the contract minor that added it, and a runtime that doesn't offer that contract refuses it by name.
- Within 1.x, `run`'s signature, the `log` import (the `log-level` cases included) and anything else already in the world stay as they are. Changing or removing any of them is `2.0.0`, a world every guest is rebuilt against.
- A new field of `RunFunctionRequest` or `RunFunctionResponse` moves no world version. Payload evolution is Protobuf's.

A guest vendors the WASI 0.2 and 0.3 interfaces it imports under its `wit/deps/`, from the wasmtime-wasi-http release the runtime builds on. The release notes list the WASI versions the runtime links. A guest built against an older vendored set keeps running while those versions are linked, and otherwise fails the world typecheck at load.

Until `1.0.0` is published, a Wasmtime release that moves the WASI 0.3 interfaces is taken on `1.0.0` itself, and guests are rebuilt.

## Getting the world

Guest toolchains read a local `wit/` directory, and a guest keeps the package there as a file, `wit/deps/guestfn-function.wit`, byte-identical to the runtime's copy. Keep that file unmodified, because a WIT package that appears twice must have the same contents.

The guest's own world, `wit/world.wit` in a `local:guest` package, includes one of the two worlds beside whatever else the guest imports:

```wit
package local:guest;

world function {
    include guestfn:function/function-sync@1.0.0;

    import wasi:http/outgoing-handler@0.2.12;
}
```

`guestfn init` writes both files. The `rust` scaffold includes `function`. The `go`, `ts` and `python` scaffolds and the C, C++, Zig, Odin, MoonBit and C# examples include `function-sync`, the signature their toolchains take today.

### Publication

`Publish WIT` (`publish-wit.yml`), run by hand from a `wit-v<version>` tag, publishes the package, signed and attested, as an OCI artifact in the CNCF Wasm OCI layout, the format of `wkg`: `ghcr.io/jonasz-lasut/guestfn/function:<version>`. A published version is never pushed again.

Once a version is published, map the namespace in the [`wkg`](https://github.com/bytecodealliance/wasm-pkg-tools) configuration:

```toml
[namespace_registries]
guestfn = { registry = "guestfn", metadata = { preferredProtocol = "oci", "oci" = { registry = "ghcr.io", namespacePrefix = "jonasz-lasut/" } } }
```

`wkg get guestfn:function@1.0.0 --format wit` then writes `guestfn_function@1.0.0.wit`, the file a guest keeps under `wit/deps/`.
