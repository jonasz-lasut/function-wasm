# my-fn

A [Crossplane](https://crossplane.io) composition function in Rust, compiled to
a wasip1 reactor of about 250 KB and run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm).

- `proto/run_function.proto` is crossplane's `RunFunction` contract, vendored;
  `build.rs` compiles it with `prost-build` (needs `protoc`).
- `src/lib.rs` — `run_function` over the prost messages (edit this), `handle`
  (decode, run, encode; errors become fatal results), the `wasmfn.log` import,
  and the `wasmfn_alloc`/`wasmfn_run` exports of the function-wasm
  [ABI](https://github.com/jonasz-lasut/function-wasm/blob/main/docs/abi.md)
  behind `#[cfg(target_os = "wasi")]`, so `cargo test` runs natively.
- `src/http.rs` — `http::get`/`http::send` over the `wasmfn.http` import:
  HTTP through the host, within the Composition's `sandbox.egress` grant
  (`config.greetingUrl` uses it). Natively the host is a closure installed
  with `http::set_host`, so the tests fake it.

```shell
rustup target add wasm32-wasip1

# Unit tests run natively.
cargo test

# Compile to a wasip1 module.
guestfn build                       # cargo build --release --target wasm32-wasip1 → fn.wasm

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
