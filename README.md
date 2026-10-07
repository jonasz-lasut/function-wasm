# function-wasm

[![CI](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml/badge.svg)](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml)
[![Docs](https://github.com/jonasz-lasut/function-wasm/actions/workflows/docs.yml/badge.svg)](https://jonasz-lasut.github.io/function-wasm/)

> [!CAUTION]
> **Experimental.** function-wasm is young: the sandbox has not had an
> independent security review, and nothing here has run in production yet.
> From 1.0.0 the Input, the guest ABI, the runtime flags and the rest of
> the surface listed under [Compatibility](https://jonasz-lasut.github.io/function-wasm/contract/compatibility.html) change only
> through its deprecation policy; before it they could change between
> minor releases. Try it, break it and
> [open an issue](https://github.com/jonasz-lasut/function-wasm/issues),
> but do not build a platform on it yet.

A [Crossplane](https://crossplane.io) composition function that runs a
WebAssembly module in a [wasmtime](https://wasmtime.dev) sandbox. The module
implements the same contract as a native composition function —
`RunFunction(RunFunctionRequest) → RunFunctionResponse` — so you write an
ordinary [function-sdk-go](https://github.com/crossplane/function-sdk-go)
function, compile it to WebAssembly, publish the module, and point a
Composition step at it. One installed `function-wasm` serves any number of
modules; changing composition logic never means building, publishing and
installing another Function package.

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

The whole `RunFunctionRequest` reaches the module and its whole
`RunFunctionResponse` goes back to Crossplane, so desired resources, results,
conditions, requirements (extra resources), context and TTL all work exactly
as they do for a native function.

function-wasm is the right tool when the *logic* of a pipeline step should be
replaceable without rebuilding, publishing and installing a Function package,
or when the person who owns that logic is not the person who owns the
Composition. By default a module gets no network, filesystem or environment;
what it gets beyond that is decided per capability by its manifest, the
Composition's `compositionPolicy` and the operator's Cedar policy, three
AND-combined layers. The [use cases](https://jonasz-lasut.github.io/function-wasm/use-cases.html)
and the [trust model](https://jonasz-lasut.github.io/function-wasm/operators/trust-model.html)
say where it fits and what it protects.

## Install

```yaml
apiVersion: pkg.crossplane.io/v1
kind: Function
metadata:
  name: function-wasm
spec:
  package: ghcr.io/jonasz-lasut/function-wasm:v0.6.0
```

Also mirrored to `xpkg.upbound.io/jonasz-lasut/function-wasm`.

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

You get a plain function-sdk-go `RunFunction`, a vendored `internal/wasmfn`
glue the project owns, and a `wasmfn.yaml` manifest declaring what the
module needs; `guestfn push` prints the `module:` block to paste into a
Composition step. The whole walk-through, the manifest, local rendering and
`function validate` are under
[Guest authors](https://jonasz-lasut.github.io/function-wasm/guests/write-a-module.html)
on the documentation site.

Every scaffold is an [ABI v2](docs/abi-v2.md) component, the same greeting
function in each language; any toolchain that produces a component
implementing the world can target the runtime:

| `guestfn init --lang` | toolchain | module size | example |
|---|---|---|---|
| `go` (default) | Go + function-sdk-go | ~75 MB (13 MB compressed) | [`examples/pdb-addon`](examples/pdb-addon) |
| `rust` | Rust 1.100+, `wasm32-wasip3` | ~240 KB | [`examples/cloudflare-origin`](examples/cloudflare-origin) |
| `zig` | Zig 0.16 | ~60 KB | [`examples/hello-zig`](examples/hello-zig) |
| `c` | C via `zig cc` | ~62 KB | [`examples/hello-c`](examples/hello-c) |
| `ts` | node + npm (esbuild, jco) | ~14 MB | [`examples/policy-gate`](examples/policy-gate) |
| `python` | `python3` (componentize-py) | ~21 MB | [`examples/team-tags`](examples/team-tags) |

C# and Odin are examples only ([`examples/dashboard-bundle`](examples/dashboard-bundle),
~4.5 MB; [`examples/hello-odin`](examples/hello-odin), ~71 KB, on the `c`
flavour's bindings and codec). The supported, tested and untested languages, with what each
toolchain brings, are on
[Other languages](https://jonasz-lasut.github.io/function-wasm/guests/other-languages.html).

## Documentation

The manual lives at <https://jonasz-lasut.github.io/function-wasm/>, built
from [`docs/`](docs) by mdBook:

- [Introduction](https://jonasz-lasut.github.io/function-wasm/introduction.html),
  [Use cases](https://jonasz-lasut.github.io/function-wasm/use-cases.html),
  [How a request runs](https://jonasz-lasut.github.io/function-wasm/how-a-request-runs.html)
- Guest authors:
  [Write a module](https://jonasz-lasut.github.io/function-wasm/guests/write-a-module.html),
  [Other languages](https://jonasz-lasut.github.io/function-wasm/guests/other-languages.html),
  [Module manifests](https://jonasz-lasut.github.io/function-wasm/guests/module-manifests.html),
  [Render locally](https://jonasz-lasut.github.io/function-wasm/guests/render-locally.html),
  [Validate a Composition](https://jonasz-lasut.github.io/function-wasm/guests/validate.html)
- Operators:
  [Install](https://jonasz-lasut.github.io/function-wasm/operators/install.html),
  [Input reference](https://jonasz-lasut.github.io/function-wasm/operators/input-reference.html),
  [Runtime flags](https://jonasz-lasut.github.io/function-wasm/operators/runtime-flags.html),
  [Operator grant policy](https://jonasz-lasut.github.io/function-wasm/operators/grant-policy.html),
  [Cedar policy reference](https://jonasz-lasut.github.io/function-wasm/operators/cedar-policy.html),
  [HTTP egress](https://jonasz-lasut.github.io/function-wasm/operators/http-egress.html),
  [Readiness and warm-up](https://jonasz-lasut.github.io/function-wasm/operators/readiness.html),
  [Sizing](https://jonasz-lasut.github.io/function-wasm/operators/sizing.html),
  [Metrics](https://jonasz-lasut.github.io/function-wasm/operators/metrics.html),
  [Trust model](https://jonasz-lasut.github.io/function-wasm/operators/trust-model.html),
  [Signatures](https://jonasz-lasut.github.io/function-wasm/operators/signatures.html),
  [Troubleshooting](https://jonasz-lasut.github.io/function-wasm/operators/troubleshooting.html)
- Contract:
  [ABI v2](https://jonasz-lasut.github.io/function-wasm/abi-v2.html),
  [Compatibility](https://jonasz-lasut.github.io/function-wasm/contract/compatibility.html)
- Project:
  [Development](https://jonasz-lasut.github.io/function-wasm/project/development.html),
  [Security policy](https://jonasz-lasut.github.io/function-wasm/project/security.html),
  [Use-case examples](https://jonasz-lasut.github.io/function-wasm/one-pager-use-case-examples.html),
  [Design records](https://jonasz-lasut.github.io/function-wasm/project/design-records.html)

## Development

```shell
cargo build --workspace && cargo test --workspace   # engine, runtime, guestfn - conformance goldens and scaffold goldens included
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
mdbook build                                        # the documentation site, from docs/ (book.toml)
```

[Development](https://jonasz-lasut.github.io/function-wasm/project/development.html)
has the rest; [AGENTS.md](AGENTS.md) has the layout and conventions.
