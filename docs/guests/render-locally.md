# Render locally

This page is for a module author with a scaffolded guest project who wants to
see what their Composition produces before publishing anything. It runs the
runtime built from a checkout of this repository against a `Path` module on
your machine. Installing the runtime in a cluster is out of scope.

You need the `crossplane` CLI, Docker (the render engine runs in it) and a
Rust toolchain to build the runtime from the checkout.

Use two terminals. In the checkout, start the runtime with
`--module-dir` set to your project directory (an absolute path is simplest):

```shell
cargo run --release -p function-wasm -- --insecure --debug --module-dir=<your project directory>
```

In your project directory, build the module and render with the Development
runtime that the scaffold's `example/functions.yaml` declares:

```shell
guestfn build
crossplane render example/xr.yaml example/composition.yaml example/functions.yaml
```

The example Composition uses `module.type: Path` with `path: fn.wasm`. For a
cluster, use `type: OCI` with an `oci` reference, as the
[Input reference](../operators/input-reference.md) describes.

Rendering asserts nothing. To run this loop on the examples in this
repository, see the [development guide](../project/development.md).
