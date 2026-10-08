# hello-moonbit

A [Crossplane](https://crossplane.io) composition function in
[MoonBit](https://www.moonbitlang.com) on
[ABI v2](https://github.com/jonasz-lasut/function-wasm/blob/main/docs/abi-v2.md):
a WebAssembly component of about 107 KB, compiled by `moon` and run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm). The one
`moon` binary is the compiler, the build system, the test runner and the
package fetcher; `guestfn build` embeds the world into the core module it
links and wraps that into the component - no wasm-tools, no adapter
(nothing imports `wasi_snapshot_preview1`). An example only
([#155](https://github.com/jonasz-lasut/function-wasm/issues/155)):
`guestfn build` detects and builds a MoonBit project by its `moon.mod`,
`guestfn init` does not scaffold one.

- `wit/world.wit` is this guest's world: the `wasmfn:function` contract
  restated in a `local:guest` package with `run` declared sync (a sync-lifted
  `run` satisfies the runtime's async world) and
  `wasi:http/outgoing-handler@0.2.12` imported for the greeting fetch;
  `wit/deps/` carries the WASI 0.2.12 packages. It is the `c` scaffold's
  `wit/`, byte for byte. `make gen-bindings` runs
  [wit-bindgen](https://github.com/bytecodealliance/wit-bindgen)'s MoonBit
  generator (`wit-bindgen-cli` 0.62.0) over it into `src/`: `gen/` (the
  export shims, `cabi_realloc` over MoonBit's allocator, and the `link`
  section that names the core exports `run`, `cabi_post_run` and
  `cabi_realloc`), `world/function/` (the typed `log` import) and
  `interface/wasi/*` (the WASI 0.2 bindings) - checked in, so a plain `moon
  build` needs only moon. The generator declares `run` in
  `src/gen/world/function/top.mbt` and leaves its body to the guest:
  `run.mbt` beside it is the one hand-written file among the bindings, and
  that package's `moon.pkg.json` (generated too) must import the glue, which
  `make gen-bindings` re-applies after regenerating.
- `proto/run_function.proto` is crossplane's `RunFunction` contract, vendored
  (its `google/protobuf` imports resolve from protoc's own include
  directory); `make gen-proto` compiles it with
  [protoc-gen-mbt](https://github.com/moonbitlang/protoc-gen-mbt) 0.2.0
  (run through `moonx`, no install) into `src/fnv1`, checked in as well. The
  well-known types (`Struct`, `Value`, `Duration`) come from the
  [moonbitlang/protobuf](https://mooncakes.io/docs/moonbitlang/protobuf)
  0.1.3 runtime, the module's one dependency (`moon.mod`).
- `src/hello/hello.mbt` - `run_function` over the generated structs (edit
  this). It takes its host as two functions (the log and an HTTP GET), so it
  links no host import and `moon test` runs `hello_test.mbt` on moonrun with
  fakes, through the codec both ways: any package that links the bindings
  imports the world's `log`, which moonrun cannot provide. `src/wasmfn/` is
  the ABI glue: the world's `run` (decode, run, encode; errors become fatal
  results), the typed `log` import as a logger and a `wasi:http@0.2` client
  (`get_text`). `config.greetingUrl` fetches the greeting through the host,
  within the egress grant of the module's manifest.

MoonBit strings are UTF-16 in linear memory, so `guestfn build` embeds the
world with that encoding and the canonical ABI transcodes every string the
guest lowers (a log line, `run`'s error); the `log` line in the runtime's
output and the fatal result's message are how to see it working.

```shell
# Unit tests run on moonrun.
make test                           # moon test --target wasm -p jonasz-lasut/hello-moonbit/hello

# Compile to a component: moon build, then guestfn embeds the world (UTF-16)
# into _build/wasm/release/build/gen/gen.wasm and wraps it.
make build                          # guestfn build (from this repository) → fn.wasm

# What the runtime sees.
guestfn inspect fn.wasm             # ABI v2, exports run, imports wasi:* and log

# Publish it as an OCI artifact; it prints the module block for the Composition.
guestfn push ghcr.io/example/hello-moonbit:v0.1.0
```

Reference the module from a Composition step of function-wasm:

```yaml
- step: hello-moonbit
  functionRef:
    name: function-wasm
  input:
    apiVersion: wasm.fn.crossplane.io/v1
    kind: Input
    module:
      type: OCI
      oci:                     # printed by guestfn push
        ref: ghcr.io/example/hello-moonbit:v0.1.0@sha256:<manifest digest>
    config:
      greeting: hi
```

`example/` renders locally with the function-wasm runtime serving this
directory (`--module-dir`) under `example/policy.cedar` and the fixture
server `render.sh` starts; `make render-check` runs both xprin cases - the
configured greeting, and the greeting fetched through the host's egress under
`example/wasmfn.local.yaml`, a manifest that requires it:

```shell
make render                         # crossplane render example/xr.yaml ...
make render-check                   # example/xprin.yaml (xprin on PATH, or XPRIN)
```

## Toolchain

Written and tested against `moon` 0.1.20260920 (`moonc` v0.10.14+7d59c7ec9,
`moonrun` 0.1.20260920; the official installer,
`curl -fsSL https://cli.moonbitlang.com/install/unix.sh | bash`, into
`~/.moon`, then `moon update` once: the installer does not fetch the
registry index that resolves `moonbitlang/protobuf`; the render job pins
the same release by checksum),
`wit-bindgen-cli` 0.62.0 and `protoc-gen-mbt` 0.2.0 (`moonx
moonbitlang/protoc-gen-mbt@0.2.0`, fetched on first use) with `protoc`
3.x or newer for the regeneration targets only, and the
`moonbitlang/protobuf` 0.1.3 runtime. Build: ~1 s for the 101 KB core
module (`moon build`), 107 KB wrapped; the runtime compiles it in ~170 ms.

## Gotchas

- **Manifest format.** This moon writes `moon.mod` and `moon.pkg` (a plain
  syntax) and deprecates the `.json` manifests wit-bindgen 0.62.0 and
  protoc-gen-mbt 0.2.0 still write. The module file and the hand-written
  packages (`src/hello`, `src/wasmfn`, `src/fnv1`'s config) use the current
  format; the generated packages keep their `moon.pkg.json`. `moon fmt`
  migrates every manifest it touches in place and reformats generated
  code, so `make lint` runs `moon fmt --check` over the hand-written
  packages only; after a regeneration, leave the generated files as
  written.
- **The `run` slot.** The export is declared in the generated package
  `src/gen/world/function` and implemented there (`run.mbt`); the
  generated `moon.pkg.json` beside it carries the glue import, which a
  regeneration overwrites (`make gen-bindings` restores it). Moving the
  body elsewhere is not possible: a `declare`d function without a body
  links to a trap.
- **Never `println`.** It imports `spectest.print_char`, which no host
  provides; log through `@wasmfn.info` and friends (the world's `log`).
- **Deprecation noise.** `moon check` and `moon build` print a few hundred
  warnings from the generated code (`derive(Show)`, implicit impl-as-method
  promotion, `try?`): the generators have not caught up with this moon.
  They are warnings, not errors; the hand-written packages are clean.
- **Tests need the host injected.** `run_function` takes a `Host` struct of
  two functions; `hello_test.mbt` passes fakes. A test in a package that
  imports `src/wasmfn` would not instantiate on moonrun (the `log` import).
- **The async lift.** With the world's `run` declared `async`, wit-bindgen
  (no `--async` flag) emits a callback-driven `[async-lift]run` over its
  bundled `async-core` scheduler, which this runtime serves too;
  `--async=all` does not wrap, because it async-lowers the sync imports
  (`log`, `wasi:http@0.2`). The example keeps the sync lift, the shape the
  C and Zig guests share.
