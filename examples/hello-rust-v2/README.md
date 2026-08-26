# hello-rust-v2

A [Crossplane](https://crossplane.io) composition function in async Rust,
compiled to a WebAssembly **component** of about 220 KB implementing
**ABI v2** ([docs/abi-v2.md](../../docs/abi-v2.md)) and run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm). It is the
same greeting function as every other example; what is different is the
contract: the component targets the `wasmfn:function@2.0.0-draft` WIT world,
and its `run` is an `async fn` that fetches `config.greetingUrl` by awaiting
`wasi:http/client@0.3.0` through the host's egress policy - real async
networking, no hand-written ABI glue at all (the canonical ABI owns what the
v1 flavours carry by hand: the allocator pin-set, the JSON http codec, the
log payload).

The toolchain is **stable Rust**: `rustup target add wasm32-wasip2`, plus
`protoc` for prost-build. (The `wasm32-wasip3` target is the eventual home -
tier 3 today, its wasi-libc not yet shipped - and the component the wasip2
target produces implements the same world; the runtime links WASI 0.2 and
0.3 both.)

This example is ABI v2's first guest: it passes the same behaviour tests as
the other guests (the guests suite through the real host, and
`make render-check`), but `guestfn init` does not scaffold it yet.

- `proto/run_function.proto` is crossplane's `RunFunction` contract, vendored
  (prost generates the codec at build time)
- `wit/wasmfn-function.wit` is the ABI v2 world, vendored byte-identical to
  the runtime's copy; `wit/guest.wit` is this guest's own world - the
  contract plus the `wasi:http` client it fetches with; `wit/deps/` carries
  the wasi WIT that client needs
- `src/lib.rs` is the function - ordinary async Rust over the protobuf
  messages, natively testable (`cargo test`) with a fetch double
- `src/bindings.rs` (wasm only) is the wit-bindgen world: the async `run`
  export and the `wasi:http` fetch. One subtlety worth copying: the writer
  half of a body's trailers future must be **dropped** once the request is
  built - holding it keeps the body incomplete and hangs the send

```shell
make build         # fn.wasm (a component; stable Rust + wasm32-wasip2)
make test          # native unit tests
make render        # serve this directory with the runtime and crossplane render
```
