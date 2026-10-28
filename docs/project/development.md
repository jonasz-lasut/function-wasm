# Development

```shell
cargo build --workspace && cargo test --workspace   # engine, runtime, guestfn - conformance goldens and scaffold goldens included
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
(cd examples/pdb-addon && go test ./...)            # the Go example and its vendored internal/wasmfn glue
make -C examples/pdb-addon render                   # the development loop: function validate, the runtime served, crossplane render
make e2e                                            # the end-to-end suite in a kind cluster (testing.md)
```

The workspace tests build every scaffold (what `guestfn init` writes) to
WebAssembly and run them all through the host when
their toolchains (go, cargo with the rust scaffold's pinned
toolchain, zig, npm, python3) are on PATH, and skip the ones that are not.
[Testing](testing.md) sets out the two tiers and where a new test goes. See
[AGENTS.md](https://github.com/jonasz-lasut/function-wasm/blob/main/AGENTS.md) for the layout and conventions.

Design documents live under `docs/` as one-pagers, one per feature, each
headed by its status: implemented (cache, module source schema, trust
model, resource governance, sandbox, admission and inspection tooling,
the module manifest, manifest-less sources, the three-layer authorization
model, the policy engine, governance and performance phases, the use-case
examples, ABI v2 on the component model, guest language support),
withdrawn (the local loop, a Nix development environment) or superseded
(request-delivered secrets).
[Design records](design-records.md) lists them.
