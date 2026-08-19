# my-fn

A [Crossplane](https://crossplane.io) composition function written in Go and
run as a WebAssembly module by
[function-wasm](https://github.com/jonasz-lasut/function-wasm).

`fn.go` is an ordinary [function-sdk-go](https://github.com/crossplane/function-sdk-go)
function: edit `RunFunction`, keep the tests in `fn_test.go` passing, and never
touch a wasm toolchain — `main.go` registers the function with the vendored
`internal/wasmfn` glue (yours to edit), which is what the function-wasm runtime
calls. `wasmfn.HTTPClient()`
is an `*http.Client` that performs requests through the host (`config.greetingUrl`
uses it); the manifest's `requires.egress` decides which are allowed, as the Cedar policy layers permit it.

```shell
# Unit tests run natively.
go test ./...

# Compile to a wasip1 module.
guestfn build                       # writes fn.wasm

# Publish it as an OCI artifact; it prints the module block for the Composition.
guestfn push ghcr.io/example/my-fn:v0.1.0
```

Reference the module from a Composition step of function-wasm:

```yaml
- step: my-fn
  functionRef:
    name: function-wasm
  input:
    apiVersion: wasm.fn.crossplane.io/v1beta1
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
