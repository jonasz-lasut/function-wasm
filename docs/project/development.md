# Development

This page is for contributors to function-wasm. It lists the commands that build, test and lint a change, and says which of them CI runs for you. It assumes a checkout with a Rust toolchain.

## Commands that run on every push

CI runs these on every push, and on a pull request that changes code (a
pull request touching only Markdown, JSON or SVG files skips them):

```shell
cargo build --workspace && cargo test --workspace
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The tests include the conformance goldens and the scaffold goldens. A pull request that touches the documentation (`docs/`, `README.md`, `SECURITY.md` or `book.toml`) also runs Vale:

```shell
make vale
```

## Commands that run on demand

These need Go, Docker or kind on your machine:

```shell
(cd examples/pdb-addon && go test ./...)
make -C examples/pdb-addon render
make e2e
```

The first line tests the Go example and its vendored `internal/wasmfn` glue. The second is the [development loop](../guests/render-locally.md). The third runs the end-to-end suite in a kind cluster. [Testing](testing.md) explains the two tiers and the `/e2e` comment that runs them on a pull request.

## Guest toolchains

The workspace tests build every scaffold (what `guestfn init` writes) to WebAssembly and run each through the host. The tests skip a scaffold whose toolchain isn't on `PATH`, so an all-green run can mean a guest never ran. The toolchains are `go`, `cargo` and `rustup` for the `rust` scaffold's pinned toolchain (which also needs `protoc`), `zig`, `npm` and `python3`. Set `FUNCTION_WASM_REQUIRE_TOOLCHAINS=1` to make a missing toolchain a failure.

## Where to go next

[Testing](testing.md) says where a new test goes. [AGENTS.md](https://github.com/jonasz-lasut/function-wasm/blob/main/AGENTS.md) describes the source layout and conventions. [Design records](design-records.md) lists the design documents.
