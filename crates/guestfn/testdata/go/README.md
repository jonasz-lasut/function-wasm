# my-fn

A [Crossplane](https://crossplane.io) composition function written in Go on
[ABI v2](https://github.com/jonasz-lasut/function-wasm/blob/main/docs/abi-v2.md):
a WebAssembly component run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm).

`fn.go` is an ordinary [function-sdk-go](https://github.com/crossplane/function-sdk-go)
function: edit `RunFunction`, keep the tests in `fn_test.go` passing, and never
touch a wasm toolchain - `main.go` registers the function with the vendored
`internal/wasmfn` glue (yours to edit), which implements the world's `run`
export the function-wasm runtime calls, and gives you a `logging.Logger` over
the world's typed `log` import. `wasmfn.HTTPClient()` is an `*http.Client`
that performs requests through the host over `wasi:http@0.2`
(`config.greetingUrl` uses it); the manifest's `requires.egress` decides which
are allowed, as the Cedar policy layers permit it.

- `wit/world.wit` is this guest's world: the `wasmfn:function` contract
  restated in a `local:guest` package with `run` declared sync (a sync-lifted
  `run` satisfies the runtime's async world) and
  `wasi:http/outgoing-handler@0.2.12` imported for `HTTPClient`; `wit/deps/`
  carries the WASI 0.2.12 packages.
- `internal/bindings/` is [wit-bindgen](https://github.com/bytecodealliance/wit-bindgen)'s
  Go output over it (`wit-bindgen-cli` 0.62.0), checked in, so a plain
  `go build` needs only Go; regenerate it after a change to the world with
  `wit-bindgen go wit -w function --pkg-name github.com/example/my-fn/internal/bindings --out-dir internal/bindings`.
  `export_wit_world/run.go` is hand-written: the slot the glue fills with the
  `run` export. `go.bytecodealliance.org/pkg` in `go.mod` is the bindings'
  runtime, at the version the generator targets. `go vet ./...` flags the
  generated pointer arithmetic (`unsafeptr`): vet your own packages
  (`go vet . ./internal/wasmfn`); `.golangci.yml` keeps golangci-lint off
  the generated files.
- `guestfn build` compiles the guest to a wasip1 reactor (mainline Go has no
  wasip2 port), embeds the world and wraps it into the component, linking the
  wasip1 adapter of the runtime's own wasmtime: no componentize-go, no
  wasm-tools, no adapter download.

```shell
# Unit tests run natively.
go test ./...

# Compile to a component.
guestfn build                       # writes fn.wasm (ABI v2, exports run, imports wasi:http@0.2.12 and log)

# Publish it as an OCI artifact; it prints the module block for the Composition.
guestfn push ghcr.io/example/my-fn:v0.1.0
```

Reference the module from a Composition step of function-wasm:

```yaml
- step: my-fn
  functionRef:
    name: function-wasm
  input:
    apiVersion: wasm.fn.crossplane.io/v1
    kind: Input
    module:
      type: OCI
      oci:                     # printed by guestfn push
        ref: ghcr.io/example/my-fn:v0.1.0@sha256:<manifest digest>
    config:
      greeting: hi
```

`example/` renders locally with the function-wasm runtime serving this
directory (`--module-dir`) and `crossplane render`:

```shell
guestfn build
crossplane render example/xr.yaml example/composition.yaml example/functions.yaml
```
