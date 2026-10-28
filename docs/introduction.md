# function-wasm

[![CI](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml/badge.svg)](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml)

> [!CAUTION]
> **Experimental.** function-wasm is young: the sandbox hasn't had an
> independent security review, and nothing here has run in production yet.
> From 1.0.0 the Input, the guest ABI, the runtime flags and the rest of
> what [Compatibility](contract/compatibility.md) lists change only
> through its deprecation policy; before it they could change between
> minor releases. Try it, break it and
> [open an issue](https://github.com/jonasz-lasut/function-wasm/issues),
> but don't build a platform on it yet.

This page is for Crossplane users deciding whether to run composition
logic as WebAssembly modules. It assumes you know Compositions and pipeline
mode.

function-wasm is a [Crossplane](https://crossplane.io) composition function
that runs a WebAssembly module in a [Wasmtime](https://wasmtime.dev) sandbox.
A module follows the contract of a native function,
`RunFunction(RunFunctionRequest) → RunFunctionResponse`. You write an ordinary
function, in Go with
[function-sdk-go](https://github.com/crossplane/function-sdk-go) or in Rust,
Zig, C, TypeScript or Python, compile it to WebAssembly, publish the module
and point a Composition step at it.

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
      apiVersion: wasm.fn.crossplane.io/v1
      kind: Input
      module:
        type: OCI
        oci:
          ref: ghcr.io/example/greeter:v1@sha256:4c9d…  # printed by guestfn push; the digest pins the module, the tag is for humans
      config:
        greeting: hi
```

The module receives the whole `RunFunctionRequest` and returns the whole
`RunFunctionResponse`, so desired resources, results, conditions, requirements
(extra resources), context and TTL pass through as they do for a native
function. The runtime withholds only the step credentials a module wasn't
granted.

## Where to go next

- [Write a module](guests/write-a-module.md) to scaffold, build and publish a
  guest. [Other languages](guests/other-languages.md) lists the rest.
- [Install the runtime](operators/install.md) to run function-wasm in a
  cluster.
- [A request, step by step](how-a-request-runs.md) to see what the runtime
  does between Crossplane's request and the module's response.
