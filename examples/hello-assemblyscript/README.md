# hello-assemblyscript

A [Crossplane](https://crossplane.io) composition function in
[AssemblyScript](https://www.assemblyscript.org), compiled to a wasm module of
about 30 KB - the smallest guest in this repository - and run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm). The toolchain
is `npm install`: asc compiles TypeScript-shaped source to wasm, no other
binary needed.

This example is the AssemblyScript MVP: it passes the same behaviour tests as
the other guests (`TestRunFunctionGuests` through the real host, and
`make render-check`), but `guestfn init` does not scaffold it yet.

- `proto/run_function.proto` is crossplane's `RunFunction` contract, vendored
  (with the `google/protobuf` types it imports); `make gen-proto` compiles it
  with [as-proto](https://github.com/piotr-oles/as-proto)'s `as-proto-gen`
  (needs `protoc`) into `assembly/fnv1`, which is checked in so a plain build
  needs only npm. **Four files there stay hand-written** and are kept out of
  protoc's way, correcting what the generator gets wrong about this proto:
  `Value.ts` (the `kind` oneof: the generator writes every member, so any
  encoded Value would decode as its last member; a Value also has to remember
  which member is set to copy request state back unchanged), `Result.ts` and
  `Condition.ts` (proto3 `optional` fields need explicit presence, not
  unconditional writes of ""), and `RequestMeta.ts` (crossplane packs the
  repeated `capabilities` enum; the generator only reads the unpacked form
  and desyncs on the whole request).
- `assembly/main.ts` - the entry, exporting `wasmfn_alloc`/`wasmfn_run`.
  Deliberately not `index.ts`: json-as's transform demotes a source whose
  internal path is exactly `assembly/index` to a library (meaning its own
  package entry), which silently drops a user entry's exports.
- `assembly/fn.ts` - `runFunction` over the generated messages (edit this);
  `abi.ts` (decode, run, encode; errors become fatal results), `host.ts` (the
  `wasmfn.log`/`wasmfn.http` imports and the abort handler - AssemblyScript's
  default abort imports `env.abort`, which the runtime refuses; ours logs
  through the host and traps), `http.ts` (the egress helper,
  [json-as](https://github.com/JairusSW/json-as) + base64), `log.ts`,
  `structpb.ts`. The module is built with the stub runtime: a bump allocator
  with no collector, which a fresh-instance-per-request guest never misses,
  and which makes the re-entrant `wasmfn_alloc` calls of `wasmfn.http` safe.
  `config.greetingUrl` fetches the greeting through the host, within the
  egress grant of the module's manifest.

```shell
npm ci                 # toolchain and dependencies, all from npm
make build             # asc → fn.wasm

# Behaviour is tested through the real host: TestRunFunctionGuests in the
# repository root, and
make render-check
```

Reference the module from a Composition step of function-wasm:

```yaml
- step: hello-assemblyscript
  functionRef:
    name: function-wasm
  input:
    apiVersion: wasm.fn.crossplane.io/v1beta1
    kind: Input
    module:
      type: OCI
      oci:                     # printed by guestfn push
        ref: ghcr.io/example/hello-assemblyscript:v0.1.0@sha256:<manifest digest>
    config:
      greeting: hi
```

`example/` renders locally with the function-wasm runtime serving this
directory (`--module-dir`) and `crossplane render`:

```shell
make build
crossplane render example/xr.yaml example/composition.yaml example/functions.yaml
```
