# Development

```shell
cargo build --workspace && cargo test --workspace   # engine, runtime, guestfn - conformance goldens and scaffold goldens included
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
(cd examples/pdb-addon && go test ./...)            # the Go example and its vendored internal/wasmfn glue
make -C examples/pdb-addon render-check             # function validate, then the example's xprin suite through the real runtime
test/e2e/oci/run.sh                                 # the OCI path end to end: registry, cosign, module.from (test/e2e/README.md)
```

The workspace tests build every scaffold (what `guestfn init` writes) to
WebAssembly and run them all through the host when
their toolchains (go, cargo with the rust scaffold's pinned
toolchain, zig, npm, python3) are on PATH, and skip the ones that are not. See
[AGENTS.md](https://github.com/jonasz-lasut/function-wasm/blob/main/AGENTS.md) for the layout and conventions.

Design documents live under `docs/` as one-pagers: the implemented ones
(cache, module source schema, trust model, resource governance, sandbox,
admission and inspection tooling, the module manifest, manifest-less
sources, the three-layer authorization model, the policy engine,
governance and performance phases, the use-case examples) and the drafts
(ABI v2 on the component model, guest language support, a Nix development
environment).
[Design records](design-records.md) lists them.
