# guestfn

[![CI](https://github.com/jonasz-lasut/guestfn/actions/workflows/ci.yml/badge.svg)](https://github.com/jonasz-lasut/guestfn/actions/workflows/ci.yml)
[![Docs](https://github.com/jonasz-lasut/guestfn/actions/workflows/docs.yml/badge.svg)](https://jonasz-lasut.github.io/guestfn/)

Neither sandbox has had an independent security review. From 1.0.0 the
Input, the guest contracts, the runtime flags and the rest of what
[Compatibility](https://jonasz-lasut.github.io/guestfn/contract/compatibility.html)
lists change only through its deprecation policy. Before it they could
change between minor releases.

guestfn builds [Crossplane](https://crossplane.io) composition functions
that run code supplied through a Composition step's Input, in a sandbox,
instead of code built into the function's own package. One Rust workspace
builds both:

| function | runs | in | README |
|---|---|---|---|
| function-wasm | a WebAssembly component (`wasm@1.0`) | a [Wasmtime](https://wasmtime.dev) sandbox | [`package/wasm`](package/wasm/README.md) |
| function-oci | an OCI image (`oci@0.1`) | a [Firecracker](https://firecracker-microvm.github.io) microVM, one per request | [`package/oci`](package/oci/README.md) |

A module follows the contract of a native function,
`RunFunction(RunFunctionRequest) → RunFunctionResponse`, in the module's own
language and toolchain. The step references the module by digest, or the
composite resource field that does, so one installed function serves any
number of modules and a new module version is a new reference. It fits when
the logic of a pipeline step should change without a new Function package, or
when the people who maintain that logic aren't the people who maintain the
Composition.

What the two functions share, and what this repository is about:

- **The Input** (`guestfn.crossplane.io/v1`): the module's source, the
  composite resource fields that choose the module and contain its
  configuration (`module.from`, `config.from`), the Composition author's
  own Cedar policy and the run's limits
  ([Input reference](https://jonasz-lasut.github.io/guestfn/operators/input-reference.html)).
- **The module manifest** (`guestfn.yaml`), published with the module: what
  the module needs beyond the default sandbox, and the schema of its
  configuration
  ([Module manifests](https://jonasz-lasut.github.io/guestfn/guests/module-manifests.html)).
- **The policy layers.** A module has no network, filesystem or
  environment by default; it gets a capability only where the manifest
  requests it, the Composition's `compositionPolicy` permits it and the
  operator's policy permits it. The same layers fence what a module reads
  from and writes to the cluster
  ([Cedar policy](https://jonasz-lasut.github.io/guestfn/operators/cedar-policy.html),
  [trust model](https://jonasz-lasut.github.io/guestfn/operators/trust-model.html)).
- **Digests, signatures and caches.** References pin a digest, the
  operator's policy can require cosign signatures per repository, and the
  runtime keeps fetched and compiled modules on disk
  ([Signatures](https://jonasz-lasut.github.io/guestfn/operators/signatures.html)).
- **The transparent proxy.** The module receives the whole request and
  returns the whole response; the runtime edits only the step credentials
  the module wasn't granted and the configuration it read for it.
- **`guestfn`**, the CLI: scaffolds, builds, inspects and publishes
  WebAssembly modules, publishes an image as an OCI module, writes a
  Composition step from a manifest; and `function validate`, which runs the
  runtime's own admission over Compositions offline.

## Get started

```shell
# Prebuilt, from a guestfn-v* release (guestfn-linux-amd64, -linux-arm64, -darwin-amd64 or -darwin-arm64):
curl -fsSLo guestfn https://github.com/jonasz-lasut/guestfn/releases/download/guestfn-v0.1.0/guestfn-linux-amd64 && chmod +x guestfn
# Or from source:
cargo install --git https://github.com/jonasz-lasut/guestfn guestfn

guestfn init greeter --module github.com/example/greeter   # a WebAssembly module: --lang go (default), rust, ts or python
guestfn build && guestfn push ghcr.io/example/greeter:v0.1.0

guestfn push ghcr.io/example/tagger:v0.1.0-module --image ghcr.io/example/tagger@sha256:…   # an OCI module: the image with its manifest
```

Each function's README has the Composition step and the install snippet:
[function-wasm](package/wasm/README.md), [function-oci](package/oci/README.md).

## Tested versions

What each function's latest release passed on the [release matrix](https://jonasz-lasut.github.io/guestfn/project/testing.html#the-release-matrix).

### function-wasm

<!-- tested-versions:function-wasm:start -->
| Release | Crossplane |
|---|---|
| v0.6.0 | not recorded |
<!-- tested-versions:function-wasm:end -->

### function-oci

<!-- tested-versions:function-oci:start -->
| Release | Crossplane | Firecracker × guest kernel |
|---|---|---|
| v0.2.0 | 2.4 (v2.4.3), 2.3 (v2.3.7), 2.2 (v2.2.7) | 1.18.0 × 6.18.56, 1.18.0 × 6.1.188, 1.18.0 × 5.10.271, 1.17.0 × 6.18.56, 1.17.0 × 6.1.188, 1.17.0 × 5.10.271, 1.16.2 × 6.18.56, 1.16.2 × 6.1.188, 1.16.2 × 5.10.271 |
<!-- tested-versions:function-oci:end -->

## Documentation

The manual at <https://jonasz-lasut.github.io/guestfn/> comes from [`docs/`](docs), built with mdBook. Start from the page
for your role:

- Evaluators - [Introduction](https://jonasz-lasut.github.io/guestfn/introduction.html), [Use cases](https://jonasz-lasut.github.io/guestfn/use-cases.html) and [A request, step by step](https://jonasz-lasut.github.io/guestfn/how-a-request-runs.html).
- function-wasm - [Install](https://jonasz-lasut.github.io/guestfn/wasm/install.html), [Write a module](https://jonasz-lasut.github.io/guestfn/wasm/write-a-module.html) and [Other languages](https://jonasz-lasut.github.io/guestfn/wasm/other-languages.html).
- function-oci - [Install](https://jonasz-lasut.github.io/guestfn/oci/install.html) and [OCI modules](https://jonasz-lasut.github.io/guestfn/oci/modules.html).
- Operators, for both runtimes - [Input reference](https://jonasz-lasut.github.io/guestfn/operators/input-reference.html), [Runtime flags](https://jonasz-lasut.github.io/guestfn/operators/runtime-flags.html) and [Troubleshooting](https://jonasz-lasut.github.io/guestfn/operators/troubleshooting.html).
- Anyone depending on a release - [Compatibility](https://jonasz-lasut.github.io/guestfn/contract/compatibility.html).
- Contributors - [Development](https://jonasz-lasut.github.io/guestfn/project/development.html) and [Security policy](https://jonasz-lasut.github.io/guestfn/project/security.html).

The use-case examples under [`examples/`](examples) are one guest per
language, `function-wasm-<lang>` and `function-oci-<lang>`.

## Development

```shell
cargo build --workspace && cargo test --workspace   # engine, runtime, guestfn - conformance goldens and scaffold goldens included
cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings
mdbook build                                        # the documentation site, from docs/ (book.toml)
```

[Development](https://jonasz-lasut.github.io/guestfn/project/development.html)
has the rest. [AGENTS.md](AGENTS.md) has the layout and conventions.
