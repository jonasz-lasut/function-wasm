# Other languages

**Supported and tested.** A language is *supported* when its toolchain
produces a WebAssembly component that implements the [ABI v2](../abi-v2.md)
world, `wasmfn:function`; the runtime checks the world, never the language.
Today that is every language with a
[wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) backend (Rust,
C, C++, C#, Go, MoonBit) and the componentizers for JavaScript and TypeScript
([componentize-js](https://github.com/bytecodealliance/componentize-js)),
Python ([componentize-py](https://github.com/bytecodealliance/componentize-py))
and .NET
([componentize-dotnet](https://github.com/bytecodealliance/componentize-dotnet)).
A language is *tested* when this repository works with it: a scaffold or an
example here, built and run through the host by the guest suite and the render
jobs on every `/e2e`. Tested on ABI v2: Go, Rust, Zig, C, TypeScript,
Python, C# and Odin - every scaffold and example here, since
[#114](https://github.com/jonasz-lasut/function-wasm/issues/114) moved the
last of them (Go, 2026-10-07) off ABI v1. A
supported language that is not tested is expected to work, and a
guest we can run is what moves it into the tested set. A language with no
component path is not supported: AssemblyScript was retired for that reason
(#114, 2026-10-07).

The [ABI v2](../abi-v2.md) world is one export, one import and protobuf
bytes, so any component-model toolchain works. **ABI v1** - a wasip1 core
module exporting `wasmfn_run`, the contract before the component model -
was deprecated in v0.6.0 and **is removed in 1.0.0**
([#114](https://github.com/jonasz-lasut/function-wasm/issues/114),
[#129](https://github.com/jonasz-lasut/function-wasm/issues/129)): the
runtime refuses a core module at load, as a fatal result naming the module
(`cannot load module …: module is a core module, which function-wasm 1.0.0
no longer runs (ABI v1 was removed); build it as an ABI v2 component
(../abi-v2.md)`), and `guestfn build`, `guestfn inspect` and
`function validate --resolve` print the same sentence for one; rebuild
such a guest on a current scaffold. `guestfn` scaffolds and builds six
flavours - the same greeting function each time, every one an ABI v2
component:

| `guestfn init --lang` | example | toolchain | how it talks protobuf | module size |
|---|---|---|---|---|
| `go` (default) | [`examples/pdb-addon`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/pdb-addon) | Go + function-sdk-go (vendored `internal/wasmfn` glue over [wit-bindgen](https://github.com/bytecodealliance/wit-bindgen)'s Go bindings, checked in; `make gen-bindings` + `wit-bindgen-cli` 0.62.0 to redo) - **an ABI v2 component**, sync-lifted `run`, `*http.Client` over `wasi:http@0.2`; `guestfn build` embeds the world into the wasip1 reactor `go build` emits and wraps it (no componentize-go, no wasm-tools) | `request`/`response`/`resource` helpers | ~75 MB (13 MB compressed) |
| `rust` | [`examples/cloudflare-origin`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/cloudflare-origin) | Rust 1.100+ (its beta, pinned by `rust-toolchain.toml`, until 1.100.0), `wasm32-wasip3` (`cargo`, `protoc`) - **an ABI v2 component**, async `run` + `wasi:http` fetch ([docs/abi-v2.md](../abi-v2.md)) | [prost](https://github.com/tokio-rs/prost) over the vendored proto | ~240 KB scaffolded, ~315 KB for the example |
| `zig` | [`examples/hello-zig`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/hello-zig) | [Zig](https://ziglang.org) 0.16 (a single binary) over [wit-bindgen](https://github.com/bytecodealliance/wit-bindgen)'s C bindings through translate-c (the `c` flavour's, checked in; `make gen-bindings` + `wit-bindgen-cli` 0.62.0 to redo) - **an ABI v2 component**, sync-lifted `run`, fetch over `wasi:http@0.2`; `guestfn build` wraps the core module zig links (no libc, no wasm-tools, no adapter) | [zig-protobuf](https://github.com/Arwalk/zig-protobuf) over the vendored proto (generated codec checked in; `protoc` only to regenerate) | ~60 KB |
| `c` | [`examples/hello-c`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/hello-c) | C via `zig cc` (the same zig binary, no wasi-sdk) over [wit-bindgen](https://github.com/bytecodealliance/wit-bindgen)'s C bindings (checked in; `make gen-bindings` + `wit-bindgen-cli` 0.62.0 to redo) - **an ABI v2 component**, sync-lifted `run`, fetch over `wasi:http@0.2`; `guestfn build` wraps the core module zig links (no wasm-tools, no adapter) | [nanopb](https://jpa.kapsi.fi/nanopb/) over the vendored proto (heap-allocated fields, generated codec checked in; `nanopb_generator` only to regenerate) | ~62 KB |
| `ts` | [`examples/policy-gate`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/policy-gate) | node + npm: [esbuild](https://esbuild.github.io) bundles, [jco](https://github.com/bytecodealliance/jco) componentizes - **an ABI v2 component**, sync-lifted `run`, `fetch()` over `wasi:http@0.2` | [protobuf-es](https://github.com/bufbuild/protobuf-es) over the vendored proto (`js+dts` codec checked in; `npm run gen-proto` + protoc to redo) | ~14 MB (SpiderMonkey) |
| `python` | [`examples/team-tags`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/team-tags) | `python3` (a venv with [componentize-py](https://github.com/bytecodealliance/componentize-py)) - **an ABI v2 component**, sync-lifted `run`, fetch over `wasi:http@0.2` | protoc's Python codec over the vendored proto (checked in), on the pure-Python `protobuf` runtime | ~21 MB (CPython) |

**TinyGo** is possible if needed: #114's spike built this greeting guest as a
1.5 MB ABI v2 component on TinyGo's own `wasip2` target with
[wit-bindgen-go](https://github.com/bytecodealliance/go-modules) (vtprotobuf
for the codec, since protobuf-go's panics under TinyGo), at 0.4 s of runtime
compile and 1.8 ms a run. It has no scaffold or test here: that generator is
unmaintained since 2025-05, and TinyGo cannot use componentize-go's bindings
(tinygo#4072), so the `go` flavour above is the one this repository carries.

A **C#** ABI v2 guest exists as an example only as well
([`examples/dashboard-bundle`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/dashboard-bundle), ~4.5 MB; the
.NET 10 SDK, `make build`): compiled by NativeAOT-LLVM with
[componentize-dotnet](https://github.com/bytecodealliance/componentize-dotnet)
over wit-bindgen's generated bindings, its codec protoc's C# output on
Google.Protobuf, its bundle fetched over `wasi:http@0.2` through the same
egress policy and unpacked with `System.IO.Compression` into its private
`/tmp`. Its `run` is sync-lifted, because wit-bindgen's C# async bindings
do not compile for this world yet. NativeAOT-LLVM publishes no
macOS compiler, so on macOS the Makefile builds in the .NET SDK container
(Docker). It solves its own use case instead of greeting, so its unit tests
and its render test cover it rather than the behaviour tests the other
flavours share, and `guestfn` neither scaffolds nor builds it yet.

An **Odin** ABI v2 guest is an example only too
([`examples/hello-odin`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/hello-odin),
~71 KB; Odin dev-2026-10 and Zig 0.16, `make build`): the hello guest again,
on the `c` flavour's plumbing - its wit-bindgen C bindings, nanopb codec,
WIT and proto byte for byte, held identical by the plumbing test. `odin
build -target:freestanding_wasm32 -build-mode:obj` compiles the guest to one
object in 0.2 s, `zig cc` compiles the C half and links the core module (a
wasip1 reactor that imports nothing from `wasi_snapshot_preview1`), and
`guestfn build` takes the project for a `c` one by its `build.zig` and wraps
the result with no adapter: about 7 s cold, 0.2 s warm, and the runtime
compiles the component in about 10 ms, as it does the C and Zig ones. The
Odin side defines the bindings' `exports_function_run` through Odin's
foreign interface, declares nanopb's generated structs again with their
wasm32 layout held by `#assert`s, and reaches the host's HTTP egress through
the `c` glue's `wasi:http@0.2` client; its allocator is wasi-libc's heap,
the module's one heap, so what Odin allocates may be handed to the canonical
ABI and freed there. `core:fmt` is left out on purpose: its reflection
tables would make the module 198 KB. Two things to know: Odin's runtime
defines `bzero` on every wasm target with a return value where libc's has
none, which `wasm-ld` reports as a signature mismatch and `zig build-exe`
treats as an error, so the example links through `zig cc`, which only
warns; and a panic traps without a message on the freestanding runtime. Its
unit tests run natively (`make test`, over the C half compiled for the
host), and `guestfn` builds it but does not scaffold it.

The **TypeScript** flavour (`npm install` is the whole toolchain) is typed
end to end (protobuf-es generated types, `tsc --noEmit` in the test gate),
and its greeting is fetched with the platform's own `fetch()`, which the
runtime serves over `wasi:http@0.2` through the same egress policy as every
other guest. Its `run` is sync-lifted (componentize-js cannot async-lift a
custom world yet - the world accepts that); the TypeScript itself awaits
freely. The scaffold carries no `package-lock.json`: `guestfn build` runs
`npm install` for a project without one, and `npm ci` once it has one.

The **Python** flavour (`python3` is the whole toolchain) bundles the
pure-Python protobuf runtime and fetches its greeting over `wasi:http@0.2`
on componentize-py's poll loop, through the same egress policy. Sync-lifted
like the TypeScript guest; `guestfn build` makes the venv from
`requirements.txt` when the project has none.

`guestfn build` picks the toolchain from the project (`Cargo.toml` → cargo,
targeting `wasm32-wasip3`; a `build.zig` → zig, for the zig and c guests
alike; a `package.json` → npm, for the
TypeScript guest; a `requirements.txt` → a venv with componentize-py, for
the Python guest; a `go.mod` → go)
or takes `--lang`. When a build leaves a core module carrying wit-bindgen's
`component-type` section (what its C generator links in: the `zig` and `c`
flavours' `zig build`; for a `go` project `guestfn build` embeds the
world from its `wit/` into the module `go build` emits first, since
wit-bindgen's Go generator writes bindings only), `guestfn build`
wraps it into an ABI v2 component itself, linking the wasip1 adapter of the
runtime's own wasmtime when the module imports `wasi_snapshot_preview1`:
no wasm-tools install, no adapter download, and an adapter that cannot
drift from the runtime. Every flavour carries
its ABI glue in the open: the Go scaffold vendors it under `internal/wasmfn`
(the world's `run` export, the typed `log`, a `wasi:http@0.2` client behind
an `*http.Client`, over wit-bindgen's Go bindings),
Zig and C carry theirs beside the module (`src/wasmfn.zig`, `src/wasmfn.c`)
over wit-bindgen's C bindings (the typed `log`, a `wasi:http@0.2` client);
each example
has a `make render-check` that runs it through the runtime and asserts what it
composes with an [xprin](https://github.com/crossplane-contrib/xprin) suite,
and the root tests build every scaffold and run it through the host as well -
with and without an egress grant.

**Possible, untested.** A 2026-10-07 survey
([#155](https://github.com/jonasz-lasut/function-wasm/issues/155)) found
these paths to a component; none has a guest in this repository yet, so none
is in the tested set, and none needs a world change:

- **MoonBit**: wit-bindgen's first-party `moonbit` backend (async supported),
  `moon build --target wasm` to a linear-memory core module, the world
  embedded with UTF-16 strings and wrapped with no adapter; MoonBit's own
  measurement is a 27 KB component. `protoc-gen-mbt` for the codec, its
  well-known types unverified. A guest must never `println` (it imports
  `spectest.print_char`). The spike is tracked in #155.
- **D**: wit-bindgen's `d` backend (since 0.61.0) with `ldc2 -betterC`, the
  core module wrapped like a Go guest; LDC 1.43 brings druntime to wasip1 and
  wasip2; nanopb for the codec.
- **C++**: hello-c's C bindings compile as C++ with `zig c++`; the separate
  community `cpp` backend needs C++20 or newer and has no async.
- **Kotlin/Wasm**: Kotlin 2.4.0's experimental Component Model support through
  JetBrains' wit-bindgen fork, a WasmGC module plus the wasip1 adapter (this
  runtime's wasmtime enables GC); no `.proto` codec with well-known types yet.
- **Swift**: the official Swift SDK for WebAssembly (6.2 and later) exports a
  reactor over the C bindings; swift-protobuf does not build for Wasm today
  and Foundation costs about 50 MB.
- **Haskell**: GHC's wasm backend (a tech preview) as a `foreign export ccall`
  reactor over the C bindings; a protobuf library under it is unverified.
- **Lua, Ruby, PHP**: an interpreter embedded in a C reactor; all three need
  setjmp/longjmp, which `zig cc` cannot build for wasm (wasi-sdk's sjlj plus
  the exception-handling proposal can); Ruby is tens of MB, PHP has no
  maintained WASI build.
- **Scala** through the scala-wasm fork (WASIp2 on WasmGC); **Nim** and **V**
  through their C output and a community WASI recipe; any compile-to-JS
  language through the `ts` flavour's componentize-js path.

Ruled out for now: AssemblyScript (its project marks the Component Model as
harmful), Java (TeaVM dropped WASI, GraalVM targets browser WasmGC), Dart,
OCaml, Grain, Elixir, Julia, Perl, R and Crystal, each for want of a
WASI-hosted reactor path.
