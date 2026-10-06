# hello-dotnet

A [Crossplane](https://crossplane.io) composition function in C#, compiled
ahead of time by NativeAOT-LLVM with
[componentize-dotnet](https://github.com/bytecodealliance/componentize-dotnet)
into an **ABI v2** component ([docs/abi-v2.md](../../docs/abi-v2.md)) of
about 4 MB and run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm). It is the
same greeting function as every other example; its `greetingUrl` fetch goes
through `wasi:http@0.2`'s `outgoing-handler`, which the runtime serves
through the same egress policy, budgets and audit line as every other
guest's networking. The .NET runtime inside the component imports WASI
0.2.6 for itself; the host links it beside the 0.2.12 interfaces the guest
names.

## Toolchain

The .NET 10 SDK. The project references componentize-dotnet
0.8.0-preview00011 and the NativeAOT-LLVM build it was released against
(10.0.0-rc.1.26306.1, from the `dotnet-experimental` feed that
`nuget.config` adds); componentize-dotnet runs wit-bindgen over `wit/` on
every build and downloads the WASI SDK 29 on the first one. NativeAOT-LLVM
publishes its compiler for linux-x64, linux-arm64 and win-x64 hosts only,
so `make` builds with the host's `dotnet` on Linux and in the
`mcr.microsoft.com/dotnet/sdk:10.0` container everywhere else (macOS),
keeping NuGet packages and the WASI SDK in the `function-wasm-dotnet` Docker
volume. `make build DOTNET=dotnet` forces the host's own; `make test` makes
the same choice, and the tests also run under a macOS `dotnet`
(`make test DOTNET=dotnet`).

Like the TypeScript and Python guests, `run` is declared sync in this
guest's wit: a sync-lifted function satisfies the runtime's async world.
An async `run` awaiting `wasi:http@0.3` works with this toolchain only
through hand-written canonical-ABI glue, because wit-bindgen's C# async
bindings (0.58 through 0.62) do not compile for this world.

## Layout

- `proto/run_function.proto` is crossplane's `RunFunction` contract,
  vendored; the checked-in `src/Gen/RunFunction.cs` is protoc's output
  (`make gen-proto` to redo) over
  [Google.Protobuf](https://www.nuget.org/packages/Google.Protobuf).
- `wit/world.wit` is this guest's world, in a `local:guest` package of its
  own: the wasmfn contract with a sync `run`, plus the `wasi:http` import
  it fetches with; `wit/deps/` carries the wasi 0.2 WIT that import needs.
- `src/Function.cs` is the function (edit this): ordinary C# over the
  protobuf messages, natively testable (`make test`, MSTest) with a fetch
  double.
- `src/App.cs` (wasm only) is the world wiring over wit-bindgen's generated
  bindings: the `run` export, the typed log adapter and the blocking
  `wasi:http` fetch.
- `HelloDotnet.csproj` builds the component; `test/` is the native test
  project, which compiles `src/Function.cs` and `src/Gen` without
  `src/App.cs`. Package versions live in `Directory.Packages.props`.

This example is example-only: it passes the same behaviour tests as the
other guests (the guests suite through the real host, and
`make render-check`), but neither `guestfn init` nor `guestfn build`
handles it yet.

```shell
make build         # fn.wasm (a component)
make test          # unit tests under dotnet test
make render        # serve this directory with the runtime and crossplane render
```
