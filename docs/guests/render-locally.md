# Render locally

Build the module, then run the runtime from a checkout serving your project
directory, and render with the Development runtime the scaffold's
`example/functions.yaml` declares:

```shell
guestfn build
cargo run --release -p function-wasm -- --insecure --debug --module-dir=.   # from a checkout
crossplane render example/xr.yaml example/composition.yaml example/functions.yaml
```

The example Composition uses `module.type: Path` with `path: fn.wasm`; swap
in `type: OCI` and the `oci` reference for a cluster. In this repository `make -C examples/pdb-addon render`
runs those three steps for the Go example. It's a loop for your eyes and
doesn't test anything: what an example composes is asserted by the repository's
[end-to-end suite](../project/testing.md) in a cluster, through a module of
its own.
