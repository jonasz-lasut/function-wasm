# Write a module

Install the CLI and scaffold a project:

```shell
cargo install --git https://github.com/jonasz-lasut/function-wasm guestfn

guestfn init greeter --module github.com/example/greeter   # --lang go (default), rust, zig, c, ts or python
cd greeter
```

You get `fn.go` — a plain function-sdk-go `RunFunction` that composes a
ConfigMap greeting the composite resource — `fn_test.go`, and a three-line
`main.go`:

```go
func init() { wasmfn.Register(&Function{log: wasmfn.NewLogger()}) }

func main() {}
```

The scaffold also vendors a small `internal/wasmfn` package the project owns
(there is no external SDK to depend on): it implements the world's `run`
export the runtime calls (over the Go bindings
[wit-bindgen](https://github.com/bytecodealliance/wit-bindgen) writes under
`internal/bindings`, checked in), decodes the request, calls your
`RunFunction`, encodes the response, and gives you a `logging.Logger` that
logs through the runtime's typed `log` import. Your function knows nothing
about WebAssembly, and `wasmfn.GetConfig(req, &cfg)` hands you the Input's
`config` block. Edit it if you need to; it is yours, like the ABI glue the
Zig and C scaffolds carry. The module is an [ABI v2](../abi-v2.md)
component: `guestfn build` runs `go build` (a wasip1 reactor, since
mainline Go has no wasip2 port), embeds the world from `wit/` and wraps the
result, linking the wasip1 adapter of the runtime's own wasmtime - no
componentize-go, no wasm-tools.

```shell
go test ./...                                   # unit tests run natively
guestfn build                                   # fn.wasm (an ABI v2 component); prints the ABI verdict and the manifest summary
guestfn inspect fn.wasm                         # size, ABI verdict, exports, imports
guestfn push ghcr.io/example/greeter:v0.1.0     # OCI artifact with the manifest; prints the module block and what the module requires
```

`guestfn build` ends with the verdict the runtime reaches when it loads the
module - for the Go scaffold `Componentized fn.wasm (wasip1 adapter linked)`
(the wrap), then `Built fn.wasm (74.9 MB, ABI v2, imports log; manifest:
greeter 0.1.0; config schema)` - and fails, in the runtime's words, on a
module the runtime would refuse (`component does not implement the
wasmfn:function world: …`; for a core module, the ABI v1 shape a guest
built before 1.0.0 has, `module is a core module, which function-wasm
1.0.0 no longer runs (ABI v1 was removed); build it as an ABI v2 component
(../abi-v2.md)`, and when that module carries wit-bindgen's
component-type section, as a `zig build` or `go build` output does, the
refusal adds that `guestfn build` wraps it); `guestfn push` refuses to
publish such a module for the same reason. The check is the runtime's own: `guestfn`
compiles the module with the same wasmtime engine (a couple of seconds for a
large Go module), so what it prints is what a load says.
`guestfn inspect fn.wasm` shows what the runtime sees: size, verdict,
exports, imports (the world's by name, the WASI interfaces as a count);
`guestfn inspect ghcr.io/example/greeter:v0.1.0` describes an artifact
from its manifest
(media types, layer, annotations) without pulling, `--pull` reading the
module too; `--output json` for scripts.

The scaffold also has a **`wasmfn.yaml`** — the module's manifest: what it
declares about itself (`name`, `version`, `abi: 2`), the sandbox
capabilities it cannot run without (`requires`: egress rules,
`filesystem.privateTmp`, `env` credential bindings, the step `credentials`
it reads from its request - the scaffold requires nothing; non-secret
configuration belongs in `config`, not env) and the
JSON Schema of its `config` (the scaffold's covers `greeting` and
`greetingUrl`). `guestfn build` validates it and checks the example
Composition's `config` against the schema; `guestfn push` publishes it
beside the module (`--manifest` names another file, `--module-version` and
`--revision` override the version and set the revision annotation) and
prints, under the `module:` block, the `requires:` block so a Composition
author knows what the policy layers must permit; the runtime then refuses
a module whose requirements the Input's `compositionPolicy` or the
operator's `--sandbox-policy-file` does not permit, before the module runs
(see [Module manifests](module-manifests.md)). `guestfn manifest validate`
checks the file, `guestfn manifest show ghcr.io/example/greeter:v0.1.0`
prints what a published module declares, and `guestfn scaffold composition
--from ghcr.io/example/greeter:v0.1.0` (or `--from fn.wasm`) writes a
Composition step — `module` pinned, a `config` skeleton from the schema, and
a commented `compositionPolicy` skeleton derived from the manifest's
`requires` (the `grantEgress`/`usePrivateTmp`/`setEnv` permits it would need,
and a `pullModule` permit for its repository for a `module.from` source);
`--full` for a whole Composition. The `compositionPolicy` block is commented:
it is a starting point for narrowing, never an active grant.

`guestfn push` produces a CNCF wasm OCI artifact: one `application/wasm`
layer, the manifest as an `application/vnd.wasmfn.manifest.v1+json` layer
when the project has one, a wasm config naming both in `layerDigests`, and
the standard `org.opencontainers.image.*` annotations from the manifest.
`oras push ghcr.io/example/greeter:v0.1.0 fn.wasm:application/wasm
wasmfn.json:application/vnd.wasmfn.manifest.v1+json` gives the same result,
and a `FROM scratch` image whose only layer `COPY`s the module to `/fn.wasm`
— that exact path, nothing is guessed from the archive — works too (without
a manifest). Any language whose toolchain produces a component
implementing the [ABI v2](../abi-v2.md) world can target the runtime
(see [Other languages](other-languages.md)).

**Module size.** A guest using function-sdk-go's `request`, `response` and
`resource` packages is about 75 MB (13 MB compressed) — those packages bring
crossplane-runtime and Kubernetes apimachinery, as they do into a native
function binary. `wasmfn` itself adds nothing beyond the protobuf types, so
a guest that works on the raw `RunFunctionRequest`/`RunFunctionResponse`
(`fnv1` and `structpb`) is about 20 MB. Either way the runtime compiles a
module once per digest and caches it.

[`examples/pdb-addon`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/pdb-addon) is a Go component built on what
`guestfn init` writes: its `internal/wasmfn` glue, its `wit/` and its
`internal/bindings` are the scaffold's, byte for byte.
