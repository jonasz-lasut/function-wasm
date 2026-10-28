# Testing

This page is for contributors adding or changing a test. It says which of the
two test tiers a change belongs in and how to run the end-to-end suite on your
machine. It assumes a Rust toolchain for the first tier and Docker for the
second.

`crossplane render` belongs to the
[development loop](../guests/render-locally.md) and isn't part of either tier.

| Tier | Runs | What it covers |
|---|---|---|
| unit and component | `cargo test --workspace`, on every push and on a pull request that changes code | the engine, the runtime and `function validate` against in-memory stand-ins and recorded goldens, and every scaffold built and run through the host (the guest suite) |
| end-to-end | `make e2e`, `/e2e` on a pull request, the [release matrix](#the-release-matrix) on a release branch | the package installed by Crossplane in a kind cluster, as an XR author and an operator see it |

## The tier for a new test

| The change | The test |
|---|---|
| a guest-visible behaviour of the engine (a trap, a limit, a WASI interface) | a WAT fixture in `crates/engine/tests/engine.rs`, or a setting of the committed guest driven from `crates/engine/tests/committed_guest.rs` |
| an Input rule, a refusal, a policy decision | a unit test beside the code, a fixture under `crates/function/testdata/validate/` and a conformance golden |
| a scaffold change | the template, then `UPDATE_GOLDENS=1 cargo test -p guestfn`; the guest suite builds the goldens |
| a fixture or stand-in a test needs | `function_wasm_engine::testing` for a guest component, `function_wasm::testing` for a registry, a signer, an in-process runtime, a server or a policy (both behind the `testutil` feature) |
| something only Crossplane shows, such as a condition, an event, a resolved `requirements` entry, a pod restart or a scraped metric | a feature in the end-to-end suite |

`UPDATE_GOLDENS=1 cargo test -p function-wasm --test conformance` records a
conformance golden. A re-record is a behaviour change, so say so in the commit.
A suite uses the shared `testing` modules and never declares its own copy of a
fixture.

## The end-to-end suite

The suite is in `crates/e2e` (package `function-wasm-e2e`). It gives each
test its own namespace (`with_temp_ns` in `crates/e2e/src/ns.rs`) and uses typed
kube-rs helpers. Tests wait with timeouts and compare what the cluster contains with
`assert_eq!`. The crate is empty without its `cluster` feature, so
`cargo test --workspace` builds none of it.

### The cluster

`make e2e.up` provisions the cluster before the tests. It uses the Crossplane build submodule's make library (`build/makelib`, with
pinned `kind`, `kubectl`, `helm` and `crossplane` CLI under `.cache/tools`) and
the suite's own setup binary, in three steps:

1. `controlplane.up` creates a kind cluster (`KIND_CLUSTER_NAME`, default
   `function-wasm-e2e`) with Crossplane at `CROSSPLANE_VERSION`. The Makefile's
   default is the current Crossplane release, which Renovate moves, and the
   environment overrides it. `e2e.crossplane` then waits for the Crossplane
   deployment and its package CRDs.
2. `e2e-setup` (`crates/e2e/src/bin/setup.rs`) creates the `e2e` namespace and
   a `registry:3` behind Basic auth and TLS from a CA it generates. It installs
   provider-nop (`PROVIDER_NOP_VERSION`), a provider whose `NopResource`
   managed resources do nothing, so the tests can compose resources without a
   cloud. It pushes and signs the test module under every variant, creates the
   profiles' ConfigMaps, Secrets and volume in `crossplane-system` and the
   Guest XRD, and writes `_output/e2e/state.json`, which the tests read.
3. `make e2e.image` builds the runtime image from the Dockerfile, or from
   `Dockerfile.dev` around a prebuilt binary named by `FUNCTION_BIN`.
   `xpkg.build.function-wasm` builds the Function package from it, and
   `local.xpkg.deploy.function.function-wasm` installs it from Crossplane's
   cache with `packagePullPolicy: Never`, on the `default` profile.

`make e2e.test.<profile>` runs one profile's tests, `make e2e.test` runs the
three in sequence, `make e2e.down` deletes the cluster and `make e2e` does
everything. After a failed run, `make e2e.dump` writes the cluster's state under
`_output/controlplane-dump`. A failed test also keeps its namespace and dumps
its objects, events and the function pod's log under
`_output/e2e/logs/<namespace>`.

### The profiles

A profile is one runtime configuration: a set of flags, an operator policy and
the mounts they need. Each test binary switches the Function's
`runtimeConfigRef` to its profile's `DeploymentRuntimeConfig`
(`crates/e2e/profiles/<profile>/drc.yaml`) before its first test and waits for
the rollout. Each binary uses one profile.

| Profile | What it tests |
|---|---|
| `default` | every case a runtime admits, under both signature verifiers |
| `strict` | the refusals: no verifier and no grants |
| `lifecycle` | warm-up, the caches across a restart and the trusted root over TUF |

The `default` profile runs the runtime with `--cosign-key` (the suite's key)
and `--cosign-trusted-root` (the in-memory Sigstore instance's root merged with
the public-good root). Its policy requires signatures from the suite's registry
and the published examples, and grants the egress, credential, env and private
`/tmp` cases the tests need. The policy is `crates/e2e/profiles/default/policy.cedar`
and the tests are in `crates/e2e/tests/default.rs`.

The `strict` profile has no verifier. Its policy requires a signature for the
plain variant only and grants egress to the registry host with no
`dialAddress` hole. Its tests cover the no-verifier refusal, the block list
refusing the service CIDR in band, and the env, credential and `/tmp`
refusals.

The `lifecycle` profile runs with `--debug`, `--warm-modules` on the published
pdb-addon, a PVC-backed cache bounded below that module's size, a readiness
probe on `/readyz`, and `--cosign-trust-instance public-good` with no root file
mounted. Its policy requires the published examples' signatures and accepts
this repository's release workflow as a signer. Its tests cover readiness
waiting for the warm-up, the caches across a restart, and the trusted root
fetched over TUF from the real public-good repository.

### The test module

`crates/e2e/guest` is the one module the suite drives: a Rust ABI v2 guest in
its own Cargo workspace, reading the `rust` scaffold's `wit/` and `proto/` in
place. `make e2e.guest` builds it on the scaffold's pinned
toolchain and the result is committed as `fn.wasm`.

The Guest XR is the composite resource the suite creates for each test, and its
`spec` says what the module exercises:

| Field | What the module does |
|---|---|
| `nopResources`, `readyAfterSeconds` | composes that many provider-nop `NopResource`s, Ready after the delay, and marks each ready once its observed counterpart is |
| `egress.url` | sends a GET request to the URL through `wasi:http` and writes the status and body, or the host's refusal, to `status.egress` |
| `env`, `credentials` | writes those variables and the keys of those step credentials to status |
| `privateTmp` | writes and reads back a file under `/tmp`, reporting the outcome |
| `requirements.environmentConfig` | asks Crossplane for that EnvironmentConfig and echoes its data once resolved |
| `fatal`, `spin`, `growMiB` | a fatal result with the message, a spin past the deadline, a memory growth refused at the limit |

A variant is the same `fn.wasm` published under one manifest. The suite has
nine, one for each of the six manifests in `crates/e2e/guest/manifests`, plus
three signature variants derived from the plain one. Each has its own digest,
because the runtime caches a signature verdict per digest and two variants
sharing one would share a verdict. The same `fn.wasm` is the engine's `wasi:http` fixture
(`crates/engine/tests/committed_guest.rs`).

### Running it locally

```shell
make e2e.up                # kind, Crossplane, provider-nop, the registry, the package (Docker running)
make e2e.test.default      # or strict, lifecycle; cargo test -p function-wasm-e2e --features cluster --test <profile> does the same
make e2e.down
```

`FUNCTION_BIN=<linux binary> make e2e.up` wraps a prebuilt runtime with
`Dockerfile.dev` instead of compiling the workspace in the Dockerfile's build
stage, as CI does. After `controlplane.up`, `KUBECONFIG` and the
current context point at the cluster. `CROSSPLANE_VERSION=2.2.7 make e2e` runs
the whole suite on another Crossplane, given as the chart version without a
`v`. 2.2.7 is also the release matrix's oldest pin.

### The workflows

`ci.yml` runs lint with `--all-features` and `cargo test --workspace` on every
push, and on a pull request unless it touches only Markdown, JSON or SVG files.

A `/e2e` comment on a pull request runs `e2e.yml`:

- the guest suite, with `FUNCTION_WASM_REQUIRE_TOOLCHAINS=1` so a missing
  toolchain fails instead of skipping
- one build job per example toolchain: build, unit tests, lint, and codec and
  bindings drift
- the cluster job, which runs the three profiles one after another on one kind
  cluster at the Makefile's `CROSSPLANE_VERSION` and uploads `make e2e.dump` on
  failure

The jobs cache under `e2e-` keys that no trusted workflow restores, and the
workflow sets `cache-mode: write` because a comment-started run gets a
read-only cache token.

### The release matrix

Crossplane maintains its three most recent minor releases
([release cycle](https://docs.crossplane.io/latest/learn/release-cycle/)), and
the function supports the same three. Every `/e2e` run tests the current minor, and the matrix
adds the two before it.

`release-matrix-e2e.yml` runs on every push to a `release-X.Y` branch. It
builds the runtime from the branch and runs `make e2e` three times in parallel,
one kind cluster per version, and uploads the cluster dump for a failed
version. `gh workflow run "Release Matrix E2E" --ref <ref>` runs it by hand on
any ref.

The Makefile's `CROSSPLANE_VERSION` default names the current version. The
workflow's `env` (`CROSSPLANE_PREVIOUS`, `CROSSPLANE_OLDEST`) pins the latest
patch of the two older minors. A red version blocks the release: the release
process pushes the branch, waits for the matrix and tags only when it's green.
