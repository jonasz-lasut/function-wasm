# function-wasm

function-wasm is a [Crossplane](https://crossplane.io) composition function
that runs a WebAssembly module in a [Wasmtime](https://wasmtime.dev) sandbox.
A module follows the contract of a native function,
`RunFunction(RunFunctionRequest) → RunFunctionResponse`. You write an ordinary
function in Go (with
[function-sdk-go](https://github.com/crossplane/function-sdk-go)), Rust,
TypeScript or Python, compile it to WebAssembly, publish the module
and point a Composition step at it. It's one of the two functions of the
[guestfn](../../README.md) project, and shares the Input, the manifest, the
policy layers and the CLI with [function-oci](../oci/README.md).

It fits when a pipeline step's logic should change without a new Function
package, or when the people who maintain that logic aren't the people who
maintain the Composition. One installed function-wasm serves any number of
modules, so a new module version is a new reference in the Input.

```yaml
apiVersion: apiextensions.crossplane.io/v1
kind: Composition
metadata:
  name: example
spec:
  compositeTypeRef:
    apiVersion: example.crossplane.io/v1
    kind: XR
  mode: Pipeline
  pipeline:
  - step: greet
    functionRef:
      name: function-wasm
    input:
      apiVersion: guestfn.crossplane.io/v1
      kind: Input
      module:
        type: OCI
        oci:
          ref: ghcr.io/example/greeter:v1@sha256:4c9d…  # printed by guestfn push; the digest pins the module, the tag is for humans
      config:
        from: spec.config    # the composite resource field holding the module's configuration
```

The module receives the whole `RunFunctionRequest` and returns the whole
`RunFunctionResponse`, so desired resources, results, conditions, requirements
(extra resources), context and TTL pass through as they do for a native
function. The runtime withholds only the step credentials a module wasn't
granted, and replaces the Input's `config` reference with the configuration
it read from the composite resource.

A module has no network, filesystem or environment by default. Anything
more needs the module's manifest to request it and both the Composition's
`compositionPolicy` and the operator's policy to permit it.
The [use cases](https://jonasz-lasut.github.io/guestfn/use-cases.html) show where that fits, and the
[trust model](https://jonasz-lasut.github.io/guestfn/operators/trust-model.html) says what it protects.

## Install

```yaml
apiVersion: pkg.crossplane.io/v1
kind: Function
metadata:
  name: function-wasm
spec:
  package: ghcr.io/jonasz-lasut/function-wasm:v0.6.0
```

The package is also published at `xpkg.upbound.io/jonasz-lasut/function-wasm`.
[Install](https://jonasz-lasut.github.io/guestfn/wasm/install.html) has the
runtime's flags, the policy file and the sizing.

## Write a module

```shell
cargo install --git https://github.com/jonasz-lasut/guestfn guestfn

guestfn init greeter --module github.com/example/greeter   # --lang go (default), rust, ts or python
cd greeter
go test ./...                                   # unit tests run natively
guestfn build                                   # fn.wasm (an ABI v2 component); prints the ABI verdict and the manifest summary
guestfn inspect fn.wasm                         # size, ABI verdict, exports, imports
guestfn push ghcr.io/example/greeter:v0.1.0     # OCI artifact with the manifest; prints the module block and what the module requires
```

For Go, `guestfn init` writes a plain function-sdk-go `RunFunction`, a
vendored `internal/wasmfn` glue the project owns and a `guestfn.yaml` manifest
declaring what the module needs. `guestfn push` prints the `module:` block to
paste into a Composition step. The
[function-wasm](https://jonasz-lasut.github.io/guestfn/wasm/write-a-module.html) pages cover the full
walk-through, local rendering and `function validate`.

Each scaffold produces an [ABI v2](../../docs/abi-v2.md) component with the
same greeting function. The table lists the languages `guestfn init`
scaffolds, with the toolchain, the scaffolded module's size and an example.

| `guestfn init --lang` | toolchain | module size | example |
|---|---|---|---|
| `go` (default) | Go + function-sdk-go | ~75 MB | [`examples/function-wasm-go`](../../examples/function-wasm-go) |
| `rust` | Rust 1.100+, `wasm32-wasip3` | ~240 KB | [`examples/function-wasm-rust`](../../examples/function-wasm-rust) |
| `ts` | node + npm (esbuild, jco) | ~14 MB | [`examples/function-wasm-ts`](../../examples/function-wasm-ts) |
| `python` | `python3` (componentize-py) | ~21 MB | [`examples/function-wasm-python`](../../examples/function-wasm-python) |

Zig, C, C#, C++, Odin and MoonBit exist as examples only
([`examples/function-wasm-zig`](../../examples/function-wasm-zig),
[`examples/function-wasm-c`](../../examples/function-wasm-c),
[`examples/function-wasm-csharp`](../../examples/function-wasm-csharp),
[`examples/function-wasm-cpp`](../../examples/function-wasm-cpp),
[`examples/function-wasm-odin`](../../examples/function-wasm-odin),
[`examples/function-wasm-moonbit`](../../examples/function-wasm-moonbit)). Any toolchain that
produces a component for the world can target the runtime.
[Other languages](https://jonasz-lasut.github.io/guestfn/wasm/other-languages.html) lists what each needs.

## This directory

`crossplane.yaml` is the package's metadata and `input/` the Input's CRD,
generated from the runtime's types for tooling and editors (Crossplane
never installs a function's Input CRD; the runtime enforces every rule
itself). The package image is `Dockerfile.wasm` at the repository root, and
`extensions/function-wasm/` holds the icon the marketplace shows beside
this file.
