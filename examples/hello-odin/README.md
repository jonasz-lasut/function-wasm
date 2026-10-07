# hello-odin

A [Crossplane](https://crossplane.io) composition function in
[Odin](https://odin-lang.org) on
[ABI v2](https://github.com/jonasz-lasut/function-wasm/blob/main/docs/abi-v2.md):
a WebAssembly component of about 70 KB, run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm). The Odin
guest implements the world through the `c` flavour's wit-bindgen C
bindings, talks protobuf through nanopb over Odin's foreign interface, and
reaches the host's HTTP egress through the `c` flavour's own client. `odin`
compiles the guest to one wasm32 object, `zig cc` compiles the C half and
links the core module, and `guestfn build` wraps it into the component - no
wasi-sdk, no wasm-tools, no adapter download. There is no `guestfn init
--lang odin`: this example is the Odin path
([#155](https://github.com/jonasz-lasut/function-wasm/issues/155)).

- `wit/world.wit` and `wit/deps/` are the `c` flavour's: the
  `wasmfn:function` contract restated in a `local:guest` package with `run`
  declared sync and `wasi:http/outgoing-handler@0.2.12` imported for the
  greeting fetch. `src/gen` is wit-bindgen's C output over it (`function.h`,
  `function.c`, `function_component_type.o`), byte for byte the `c`
  flavour's, checked in; `make gen-bindings` regenerates it.
- `proto/` and `src/fnv1` are the vendored crossplane proto and the nanopb
  codec the `c` flavour generates from it, byte for byte (`make gen-proto`
  regenerates it); the plumbing test in this repository holds all four
  directories identical to the `c` template. `src/fnv1.odin` declares the
  generated structs again in Odin with the same layout, and holds every
  size and offset it relies on with `#assert`s against what `zig cc`
  computes for wasm32, so a drift fails the build.
- `src/main.odin` - `run_function` over the mirrored structs (edit this).
  `src/structpb.odin` reads and builds `google.protobuf.Struct` values;
  `src/wasmfn.odin` is the ABI glue: the world's `run` export
  (`exports_function_run`, the one C function the bindings' run shim
  expects; decode, run, encode; errors become fatal results), the typed
  `log` import as a logger, `http_get_text` over the C client, and an Odin
  allocator over wasi-libc's heap, the module's one heap, so what Odin
  allocates may be handed to the canonical ABI and freed there. Only the
  export and the imports are wasm-specific: natively the logger prints to
  stderr and a test installs a fake host for HTTP.
- `src/wasmfn.c` is the `c` flavour's `wasi:http@0.2` client, kept as it is
  with the parts the Odin side implements cut out (the export, the codec,
  the log).
- `build.zig` runs `odin build src -target:freestanding_wasm32
  -build-mode:obj -no-entry-point`, compiles the C half with `zig cc` and
  links the core module with `zig cc` (a wasip1 reactor that imports nothing
  from `wasi_snapshot_preview1`, so `guestfn build` links no adapter).
  Freestanding, because Odin's runtime then imports nothing; the libc its
  allocations come from is the one the module links anyway. `core:fmt`
  is deliberately not used: it brings reflection and type tables that
  grow the module from 68 KB to 198 KB.
- `src/fn_test.odin` - the unit tests, run natively by `odin test` over the
  C half compiled for the host (`zig build test`): the function with a fake
  host, and one request through nanopb's decode, run and encode.

Toolchain: Odin `dev-2026-10` (`brew install odin`, or the
[release](https://github.com/odin-lang/Odin/releases) for your platform on
PATH) and Zig 0.16. The Odin compile takes 0.2 s; a cold `guestfn build`
(the C half, the link, the wrap) about 7 s, a warm one 0.2 s; the runtime
compiles the component in about 10 ms.

```shell
# Unit tests run natively; lint vets the Odin sources for the wasm target.
make test                           # zig build test (odin test over the host-compiled C half)
make lint                           # zig fmt --check build.zig, odin check -vet -strict-style

# Compile to a component: zig build, then guestfn wraps zig-out/bin/fn.wasm.
make build                          # guestfn build (from this repository) → fn.wasm

# What the runtime sees.
guestfn inspect fn.wasm             # ABI v2, exports run, imports wasi:http@0.2.12 and log

# Publish it as an OCI artifact; it prints the module block for the Composition.
guestfn push ghcr.io/example/hello-odin:v0.1.0
```

Reference the module from a Composition step of function-wasm:

```yaml
- step: hello-odin
  functionRef:
    name: function-wasm
  input:
    apiVersion: wasm.fn.crossplane.io/v1
    kind: Input
    module:
      type: OCI
      oci:                     # printed by guestfn push
        ref: ghcr.io/example/hello-odin:v0.1.0@sha256:<manifest digest>
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

## What Odin needs on wasm

- A foreign import whose name ends in `.o` (`foreign import bindings
  "function.o"`) is, on a wasm target, a set of plain link-time symbols; a
  bare name or `system:` makes them wasm imports of that module. Odin never
  opens the file in an object build, so the name only labels where the
  symbols come from (natively, for the tests, the same imports name the
  objects `zig build test` compiles). A foreign variable (the nanopb message
  descriptors) must sit in a library-less `foreign {}` block.
- `@(require, linkage = "strong")` on the export: nothing in Odin references
  `exports_function_run`, and an `@(export)` would make it a wasm export of
  its own beside the world's `run`.
- A `proc "c"` sets its own `context`; the default one's allocators are
  replaced with the libc heap here. Struct returns by value follow clang's
  wasm32 C ABI (a hidden pointer), so nanopb's `pb_istream_from_buffer` is
  called as declared.
- Odin's runtime defines `memcpy`, `memset`, `memmove` and `bzero` strongly
  on every wasm target; zig's libc ones are weak and lose, but its `bzero`
  returns a value where libc's returns nothing, which `wasm-ld` words as a
  signature-mismatch warning. `zig build-exe` turns the warning into a
  failure, `zig cc` passes it through, which is why `build.zig` links with
  `zig cc` and a cold build prints the warning once.
- A panic (a bounds check, a nil dereference) traps without a message: the
  freestanding runtime has nowhere to write it. The host reports the trap as
  the request's fatal result.
- No `_startup_runtime` runs: a global with a runtime initializer or an
  `@(init)` procedure would stay uninitialised. This guest has none.
