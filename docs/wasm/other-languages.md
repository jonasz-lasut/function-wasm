# Other languages

This page is for a module author choosing a guest language. It lists the four
flavours `guestfn init` scaffolds, the guests that exist only as examples, and
what the project hasn't tested. Pick a flavour, scaffold it, then follow [Write
a module](write-a-module.md). Any toolchain that produces a component for the
[ABI v2](../abi-v2.md) world, `guestfn:function`, works: the runtime checks the
world and never the language.

## Scaffolded flavours

`guestfn init --lang <flavour>` writes the same greeting function in each of
these, the languages with an official `function-sdk-<lang>` under the
`crossplane` GitHub organisation, and `guestfn build` detects the toolchain from the
project. All four build an ABI v2 component. The guest suite runs them
through the host on every pull request, and the example build jobs build,
test and lint them on a pull request labelled `e2e:wasm:examples`. The sizes
are of the built module.

| `--lang` | example | toolchain | Protobuf codec | module size |
|---|---|---|---|---|
| `go` (default) | [`examples/function-wasm-go`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-go) | Go | function-sdk-go's `request`/`response`/`resource` helpers | ~75 MB |
| `rust` | [`examples/function-wasm-rust`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-rust) | Rust 1.100+, target `wasm32-wasip3`, `cargo` and `protoc` | [prost](https://github.com/tokio-rs/prost) over the vendored proto | ~240 KB scaffolded, ~315 KB for the example |
| `ts` | [`examples/function-wasm-ts`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-ts) | node and npm | [protobuf-es](https://github.com/bufbuild/protobuf-es) over the vendored proto | ~14 MB (SpiderMonkey) |
| `python` | [`examples/function-wasm-python`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-python) | `python3` | protoc's Python codec on the pure-Python `protobuf` runtime | ~21 MB (CPython) |

Each flavour keeps its ABI glue in the project, so you own and can edit it. The
sections below give each toolchain and how `guestfn build` finishes the
component.

### Go

The scaffold vendors `internal/wasmfn` over the Go bindings that
[wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) writes under
`internal/bindings`. Both are checked in. `make gen-bindings` regenerates them
with `wit-bindgen-cli` 0.63.0. Your function is a plain function-sdk-go
`RunFunction`. `go build` emits a wasip1 reactor, and `guestfn build` embeds the
world from the project's `wit/` and wraps the result into a component. `run` is
sync-lifted, and an `*http.Client` reaches the host's egress over
`wasi:http@0.2`.

### Rust

The `rust` flavour needs Rust 1.100 and the `wasm32-wasip3` target. The
scaffold's `rust-toolchain.toml` pins the toolchain, which your Rust installer
fetches on first use. `run` is async, and a fetch awaits `wasi:http`. See [ABI
v2](../abi-v2.md) for the world.

### TypeScript

`npm install` is the whole toolchain. [esbuild](https://esbuild.github.io)
bundles the guest, and [jco](https://github.com/bytecodealliance/jco)
componentizes it with componentize-js. The types come from protobuf-es, the test
gate runs `tsc --noEmit`, and the greeting comes from the platform's `fetch()`,
served over `wasi:http@0.2` through the same egress policy as every other guest.
`run` is sync-lifted because componentize-js can't async-lift a custom world
yet, and your TypeScript can still `await`. The scaffold has no
`package-lock.json`: `guestfn build` runs `npm install` without one and `npm ci`
with one.

### Python

[componentize-py](https://github.com/bytecodealliance/componentize-py) is the
toolchain, and `guestfn build` makes the venv from `requirements.txt` when the
project has none. The guest bundles the pure-Python `protobuf` runtime, and
`run` is sync-lifted. Fetches go over `wasi:http@0.2` on the poll loop of
componentize-py, through the same egress policy.

### How build chooses a toolchain

`guestfn build` reads the project: `Cargo.toml` selects `cargo`, `build.zig`
selects `zig` (the Zig, C, C++ and Odin examples, and any project `zig build`
compiles), `package.json` selects npm, `requirements.txt` selects
componentize-py, `go.mod` selects `go`, and `moon.mod` or `moon.mod.json`
selects `moon`. `--lang` overrides the choice, and `build` also accepts `zig`,
`c` and `moonbit`, which `init` doesn't scaffold.

When a build leaves a core module, `guestfn build` wraps it into a component
with the wasip1 adapter of the runtime's own Wasmtime, which is the adapter
version the runtime was built with. A `zig build` that links wit-bindgen's C
bindings already leaves a core module with the `component-type` section, and
`guestfn build` wraps it directly. For `go` and MoonBit, wit-bindgen writes
bindings only, so `guestfn build` embeds the world from `wit/` first.

## Examples without a scaffold

These guests solve their own use case or show one more language. Each has unit
tests in its language and a build job that checks its generated code for drift.
`guestfn init` scaffolds none of them: `guestfn build` detects Zig, C, Odin and
C++ by their `build.zig` and MoonBit by its `moon.mod`, and not C#. Each has
the plumbing in its own tree (the vendored proto, the codec, the bindings, the
WIT and the glue), so copy the example to start from it.

- **Zig**:
  [`examples/function-wasm-zig`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-zig),
  ~60 KB, built with [Zig](https://ziglang.org) 0.16 by `make build`. It talks
  Protobuf through [zig-protobuf](https://github.com/Arwalk/zig-protobuf) over
  the vendored proto and reads wit-bindgen's C bindings, checked in under
  `src/gen`, through translate-c. The glue is `src/wasmfn.zig`. `zig build`
  links a core module without libc, and `guestfn build` wraps it into a
  component without an adapter or `wasm-tools`. `zig build test` runs the unit
  tests natively. Its scaffold retired on 2026-11-08 (#264).
- **C**:
  [`examples/function-wasm-c`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-c),
  ~62 KB, compiled by `zig cc` behind `build.zig` by `make build`. Zig 0.16 is
  its whole C toolchain. It talks Protobuf through
  [nanopb](https://jpa.kapsi.fi/nanopb/) over the vendored proto and links the
  same C bindings. The glue is `src/wasmfn.c`. The core module imports no
  wasip1, and `guestfn build` wraps it without an adapter. `protoc` and
  `nanopb_generator` are needed only to
  regenerate the codec, `wit-bindgen` only for the bindings. `zig build test`
  runs the unit tests natively. Its scaffold retired on 2026-11-08 (#264).
- **C#**:
  [`examples/function-wasm-csharp`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-csharp),
  ~4.5 MB, needs the .NET 10 SDK. `make build` compiles it with NativeAOT-LLVM
  and
  [componentize-dotnet](https://github.com/bytecodealliance/componentize-dotnet),
  in the .NET SDK container on macOS, which has no NativeAOT-LLVM compiler.
- **Odin**:
  [`examples/function-wasm-odin`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-odin),
  ~71 KB, built with Odin dev-2026-10 and Zig 0.16 by `make build`. It reuses
  the C example's bindings, nanopb codec, WIT and proto. Odin compiles the
  guest to an object, `zig cc` links it with the C half, and `guestfn build`
  treats the project as a `c` one and wraps it without an adapter. `make test`
  runs the unit tests natively.
- **C++**:
  [`examples/function-wasm-cpp`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-cpp),
  ~160 KB, built with Zig 0.16 by `make build`. It reuses the C example's
  bindings, nanopb codec, WIT and proto, with a C adapter over the generated
  structs (nanopb names a field `namespace`, which C++ can't include) and
  the function's logic in C++ over [mbedTLS](https://github.com/Mbed-TLS/mbedtls),
  both fetched by `build.zig.zon`. `zig c++` compiles the logic with libc++
  for `wasm32-wasi`, no exceptions and no RTTI, and `guestfn build` treats the project
  as a `c` one and wraps it with the wasip1 adapter for the clock and the
  randomness it imports. `make test` runs the unit tests natively.
- **MoonBit**:
  [`examples/function-wasm-moonbit`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-moonbit),
  ~107 KB, built with `moon` (version `0.1.20260920`). It uses wit-bindgen's
  MoonBit bindings and the protoc-gen-mbt codec, regenerated by `make
  gen-bindings` and `make gen-proto`. `guestfn build` detects it by `moon.mod`,
  embeds the world stated as UTF-16, MoonBit's string encoding, and wraps it
  without an adapter. The unit tests run on `moonrun`. A guest must never call
  `println`: it imports `spectest.print_char`.

## Untested

- **TinyGo**: the scaffold was retired on 2026-10-07 (#114). The `go` flavour is
  the Go path.
- **AssemblyScript**: not supported, because it has no component path.

