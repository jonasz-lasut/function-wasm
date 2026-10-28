# function-wasm

[![CI](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml/badge.svg)](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml)
[![Docs](https://github.com/jonasz-lasut/function-wasm/actions/workflows/docs.yml/badge.svg)](https://jonasz-lasut.github.io/function-wasm/)

> [!CAUTION]
> **Experimental.** function-wasm is young: the sandbox hasn't had an
> independent security review, and nothing here has run in production yet.
> From 1.0.0 the Input, the guest ABI, the runtime flags and the rest of
> what [Compatibility](https://jonasz-lasut.github.io/function-wasm/contract/compatibility.html) lists change only
> through its deprecation policy; before it they could change between
> minor releases. Try it, break it and
> [open an issue](https://github.com/jonasz-lasut/function-wasm/issues),
> but don't build a platform on it yet.

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

A module has no network, filesystem or environment by default. Anything
more needs the module's manifest to request it and both the Composition's
`compositionPolicy` and the operator's policy to permit it.
The [use cases](https://jonasz-lasut.github.io/function-wasm/use-cases.html) show where that fits, and the
[trust model](https://jonasz-lasut.github.io/function-wasm/operators/trust-model.html) says what it protects.

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

## Write a module

```shell
cargo install --git https://github.com/jonasz-lasut/function-wasm guestfn

guestfn init greeter --module github.com/example/greeter   # --lang go (default), rust, zig, c, ts or python
cd greeter
go test ./...                                   # unit tests run natively
guestfn build                                   # fn.wasm (an ABI v2 component); prints the ABI verdict and the manifest summary
guestfn inspect fn.wasm                         # size, ABI verdict, exports, imports
guestfn push ghcr.io/example/greeter:v0.1.0     # OCI artifact with the manifest; prints the module block and what the module requires
```

For Go, `guestfn init` writes a plain function-sdk-go `RunFunction`, a
vendored `internal/wasmfn` glue the project owns and a `wasmfn.yaml` manifest
declaring what the module needs. `guestfn push` prints the `module:` block to
paste into a Composition step. The
[Guest authors](https://jonasz-lasut.github.io/function-wasm/guests/write-a-module.html) pages cover the full
walk-through, local rendering and `function validate`.

Each scaffold produces an [ABI v2](docs/abi-v2.md) component with the same
greeting function. The table lists the languages `guestfn init` scaffolds, with
the toolchain, the scaffolded module's size and an example.

| `guestfn init --lang` | toolchain | module size | example |
|---|---|---|---|
| `go` (default) | Go + function-sdk-go | ~75 MB | [`examples/pdb-addon`](examples/pdb-addon) |
| `rust` | Rust 1.100+, `wasm32-wasip3` | ~240 KB | [`examples/cloudflare-origin`](examples/cloudflare-origin) |
| `zig` | Zig 0.16 | ~60 KB | [`examples/hello-zig`](examples/hello-zig) |
| `c` | C via `zig cc` | ~62 KB | [`examples/hello-c`](examples/hello-c) |
| `ts` | node + npm (esbuild, jco) | ~14 MB | [`examples/policy-gate`](examples/policy-gate) |
| `python` | `python3` (componentize-py) | ~21 MB | [`examples/team-tags`](examples/team-tags) |

C#, Odin and MoonBit exist as examples only
([`examples/dashboard-bundle`](examples/dashboard-bundle),
[`examples/hello-odin`](examples/hello-odin),
[`examples/hello-moonbit`](examples/hello-moonbit)). Any toolchain that
produces a component for the world can target the runtime.
[Other languages](https://jonasz-lasut.github.io/function-wasm/guests/other-languages.html) lists what each needs.

## Documentation

The manual at <https://jonasz-lasut.github.io/function-wasm/> comes from [`docs/`](docs), built with mdBook. Start from the page
for your role:

- Evaluators - [Introduction](https://jonasz-lasut.github.io/function-wasm/introduction.html), [Use cases](https://jonasz-lasut.github.io/function-wasm/use-cases.html) and [A request, step by step](https://jonasz-lasut.github.io/function-wasm/how-a-request-runs.html).
- Guest authors - [Write a module](https://jonasz-lasut.github.io/function-wasm/guests/write-a-module.html).
- Operators - [Install](https://jonasz-lasut.github.io/function-wasm/operators/install.html) and [Troubleshooting](https://jonasz-lasut.github.io/function-wasm/operators/troubleshooting.html).
- Anyone depending on a release - [Compatibility](https://jonasz-lasut.github.io/function-wasm/contract/compatibility.html).
- Contributors - [Development](https://jonasz-lasut.github.io/function-wasm/project/development.html) and [Security policy](https://jonasz-lasut.github.io/function-wasm/project/security.html).

## Development

```shell
cargo build --workspace && cargo test --workspace   # engine, runtime, guestfn - conformance goldens and scaffold goldens included
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
mdbook build                                        # the documentation site, from docs/ (book.toml)
```

[Development](https://jonasz-lasut.github.io/function-wasm/project/development.html)
has the rest. [AGENTS.md](AGENTS.md) has the layout and conventions.
