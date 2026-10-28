# Write a module

This page is for a module author. It takes you from `guestfn init` to a module
published as an OCI artifact, using the Go scaffold as the worked example. The
other scaffolds (`--lang rust`, `zig`, `c`, `ts` or `python`) use the same
commands. See [Other languages](other-languages.md) for what differs.

Install the CLI and scaffold a project:

```shell
cargo install --git https://github.com/jonasz-lasut/function-wasm guestfn

guestfn init greeter --module github.com/example/greeter   # --lang go (default), rust, zig, c, ts or python
cd greeter
```

The Go scaffold writes `fn.go`, `fn_test.go` and `main.go`. `fn.go` is a
plain function-sdk-go `RunFunction` that composes a ConfigMap greeting the
composite resource, with no code specific to WebAssembly. `main.go` only
registers it:

```go
func init() {
	wasmfn.Register(&Function{log: wasmfn.NewLogger(), http: wasmfn.HTTPClient()})
}

func main() {}
```

The glue is a small `internal/wasmfn` package that the project owns. It
exports `run` for the [ABI v2](../abi-v2.md) world: it decodes the request,
calls your `RunFunction` and encodes the response. It also provides a
`logging.Logger` over the runtime's typed `log` import, and
`wasmfn.GetConfig(req, &cfg)` to read the Input's `config` block. The Go
bindings under `internal/bindings` are written by
[wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) and checked in.

[`examples/pdb-addon`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/pdb-addon)
is a Go module built on what `guestfn init` writes. Its `internal/wasmfn`,
`wit/` and `internal/bindings` are the scaffold's, byte for byte.

## Build, inspect and publish

```shell
go test ./...                                   # unit tests run natively
guestfn build                                   # fn.wasm (an ABI v2 component); prints the ABI verdict and the manifest summary
guestfn inspect fn.wasm                         # size, ABI verdict, exports, imports
guestfn push ghcr.io/example/greeter:v0.1.0     # OCI artifact with the manifest; prints the module block and what the module requires
```

`guestfn build` runs `go build`, embeds the world from `wit/` and wraps the
result into a component. It then compiles the component with the runtime's own
Wasmtime engine and prints the verdict a load gives. For the Go scaffold
the output is a `Componentized fn.wasm (wasip1 adapter linked)` line, then a
`Built` line with the file, its size, `ABI v2`, the world's imports and the
manifest summary.

A module the runtime would refuse fails the build with the runtime's own
words, and `guestfn push` refuses to publish it for the same reason. The
refusals are listed in [Troubleshooting](../operators/troubleshooting.md).

`guestfn inspect fn.wasm` shows what the runtime sees: size, verdict,
exports and imports. `guestfn inspect ghcr.io/example/greeter:v0.1.0`
describes a published artifact from its manifest without pulling it, and
`--pull` reads the module too. `--output json` prints JSON for scripts.

## Manifest

The scaffold includes a `wasmfn.yaml`, the module's manifest. It gives the
module a name and version, declares the sandbox capabilities the module can't
run without, and contains the JSON Schema of its `config`. `guestfn build`
validates it and `guestfn push` publishes it beside the module. See
[Module manifests](module-manifests.md) for the fields and for how the runtime
checks a module's requirements.

`guestfn scaffold composition --from ghcr.io/example/greeter:v0.1.0` (or
`--from fn.wasm`) writes a Composition step for the module, pinned by digest,
with a `config` skeleton from the schema and a commented `compositionPolicy`
skeleton. The policy block is a commented starting point for narrowing.
`--full` writes a whole Composition.

## What `guestfn push` publishes

`guestfn push` produces a CNCF Wasm OCI artifact: one `application/wasm`
layer, the manifest as an `application/vnd.wasmfn.manifest.v1+json` layer
when the project has one, a config object listing both digests in
`layerDigests`, and the standard `org.opencontainers.image.*` annotations
from the manifest.

`oras push ghcr.io/example/greeter:v0.1.0 fn.wasm:application/wasm
wasmfn.json:application/vnd.wasmfn.manifest.v1+json` gives the same result.
A `FROM scratch` image whose only layer copies the module to `/fn.wasm` works
too, without a manifest. The runtime reads that exact path and doesn't search
the archive.

## Module size

A Go module that uses function-sdk-go's helpers is about 75 MB. See
[Sizing](../operators/sizing.md) for what that costs the runtime.
