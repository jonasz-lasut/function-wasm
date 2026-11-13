# Development

This page is for contributors to guestfn. It lists the commands that build, test and lint a change, and says which of them CI runs for you. It assumes a checkout with a Rust toolchain.

## Commands that run on every push

CI runs these on every push, and on a pull request that changes code. A
pull request that touches documentation alone skips them: `docs/`, the
top-level, package and example Markdown files, `.claude/` and `extensions/`.

```shell
cargo build --workspace && cargo test --workspace
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo deny check advisories bans licenses sources   # cargo install cargo-deny --locked
```

The tests include the conformance goldens and the scaffold goldens. The
`cargo deny` run checks `Cargo.lock` against `deny.toml`: every crate is
free of published advisories, under an allowed license and from crates.io. A pull request that touches the documentation (`docs/`, `README.md`, `SECURITY.md` or `book.toml`) also runs Vale, after a check that the design records index and `docs/SUMMARY.md` restate the status line of each one-pager:

```shell
make vale
```

## Commands that run on demand

These need Go, Docker or kind on your machine:

```shell
(cd examples/function-wasm-go && go test ./...)
make -C examples/function-wasm-go render
make e2e
```

The first line tests the Go example and its vendored `internal/wasmfn` glue. The second is the [development loop](../wasm/render-locally.md). The third runs the end-to-end suite in a kind cluster. [Testing](testing.md) explains the two tiers and the `e2e:` labels that select the end-to-end scenarios on a pull request.

`(cd examples/function-oci-go && go test ./...)` tests the OCI example natively. `make oci.test` runs the suite of the OCI engine in real VMs and needs `/dev/kvm`.

## Guest toolchains

The workspace tests build every scaffold (what `guestfn init` writes) to WebAssembly and run each through the host. The tests skip a scaffold whose toolchain isn't on `PATH`, so an all-green run can mean a guest never ran. The toolchains are `go`, `cargo` and `rustup` for the `rust` scaffold's pinned toolchain (which also needs `protoc`), `npm` and `python3`. Set `GUESTFN_REQUIRE_TOOLCHAINS=1` to make a missing toolchain a failure.

## Where to go next

[Testing](testing.md) says where a new test goes. [AGENTS.md](https://github.com/jonasz-lasut/guestfn/blob/main/AGENTS.md) describes the source layout and conventions. [Design records](design-records.md) lists the design documents.
