# Testing

function-wasm has two test tiers, with nothing between them: the Rust unit
and component tests, run by `cargo test` on every push, and a
Kubernetes-native end-to-end suite, also Rust, run against a kind cluster by
`make e2e`. The project has no render tests and no shell scenarios:
`crossplane render` is a [development loop](../guests/render-locally.md)
rather than a test.

| Tier | Runs | What it proves |
|---|---|---|
| unit and component | `cargo test --workspace`, every push and pull request | the engine over WAT fixtures and the committed end-to-end guest; the runtime's admission, resolver, policy, egress and signature code against in-memory stand-ins (a registry, a Sigstore instance); `function validate` against recorded goldens; the served runtime over gRPC; every scaffold built and run through the host (the guest suite) |
| end-to-end | `make e2e` (`/e2e` on a pull request; the [release matrix](#the-release-matrix) on a release branch) | the package installed by Crossplane in a kind cluster: the runtime's behaviour as an XR author and an operator see it, one profile of runtime flags at a time |

## The tier for a new test

| The change | The test |
|---|---|
| a guest-visible behaviour of the engine (a trap, a limit, a WASI interface) | a WAT fixture in `crates/engine/tests/engine.rs`, or a setting of the committed guest driven from `crates/engine/tests/committed_guest.rs` |
| an Input rule, a refusal, a policy decision | a unit test beside the code, a fixture under `crates/function/testdata/validate/` and a conformance golden (`UPDATE_GOLDENS=1 cargo test -p function-wasm --test conformance` records it; a re-record is a behaviour change, say so in the commit) |
| a scaffold change | the template, then `UPDATE_GOLDENS=1 cargo test -p guestfn`; the guest suite builds the goldens |
| a new fixture or stand-in a test needs | `function_wasm_engine::testing` for a guest component, `function_wasm::testing` for a registry, a signer, an in-process runtime, a server or a policy (both behind the `testutil` feature); a suite never declares its own copy |
| something only Crossplane shows - a condition, an event, a Secret read, a `requirements` entry resolved, a pod restarted, a metric as scraped | a feature in the end-to-end suite |

## The end-to-end suite

The suite is in `crates/e2e` (package `function-wasm-e2e`), shaped like
Linkerd's `policy-test`, with a namespace per test, typed helpers over
kube-rs, waits with timeouts and `assert_eq!` on what the cluster contains.
The cluster is provisioned before the tests, never from inside one. The
crate is empty without its `cluster` feature, so `cargo test --workspace`
builds none of it.

### The cluster

`make e2e.up` provisions everything through the Crossplane build submodule's
make library (`build/makelib`, pinned `kind`, `kubectl`, `helm` and
`crossplane` CLI under `.cache/tools`) and the suite's own setup binary:

1. `controlplane.up`: a kind cluster (`KIND_CLUSTER_NAME`, default
   `function-wasm-e2e`) with Crossplane at `CROSSPLANE_VERSION` (the
   Makefile's default is the current Crossplane release, moved by Renovate;
   the environment overrides it), then `e2e.crossplane`, a `kubectl wait`
   for the Crossplane deployment and the package CRDs its init container
   registers.
2. `e2e-setup` (`crates/e2e/src/bin/setup.rs`) sets up the `e2e` namespace;
   a `registry:3` behind Basic auth and TLS from a CA the setup generates
   (reused across runs); provider-nop (`PROVIDER_NOP_VERSION`); the test
   module pushed under every variant and signed; the profiles' ConfigMaps,
   Secrets and volume in `crossplane-system`; the Guest XRD; and
   `_output/e2e/state.json`, which the tests read.
3. The runtime image (`make e2e.image`: the Dockerfile, or `Dockerfile.dev`
   around a prebuilt binary named by `FUNCTION_BIN`), the Function package
   from it (`xpkg.build.function-wasm`), and the package installed from
   Crossplane's cache with `packagePullPolicy: Never`
   (`local.xpkg.deploy.function.function-wasm`), on the default profile.

`make e2e.test.<profile>` runs one profile's tests and `make e2e.test` the
three in sequence. `make e2e.down` deletes the cluster, and `make e2e` does
everything. `make e2e.dump` writes the cluster's state under
`_output/controlplane-dump` for a failed run, and a failed test keeps its
namespace and dumps its objects, events and the function pod's log under
`_output/e2e/logs/<namespace>`.

### The profiles

The Function is reconfigured between suites: each test binary switches its
`runtimeConfigRef` to the profile's `DeploymentRuntimeConfig`
(`crates/e2e/profiles/<profile>/drc.yaml`) before its first test and waits
for the rollout. One profile per binary.

| Profile | Runtime | Tests |
|---|---|---|
| `default` | `--cosign-key` (the suite's key) and `--cosign-trusted-root` (the in-memory Sigstore instance's root merged with the public-good root); a policy that requires signatures from the suite's registry and the published examples, that permits the suite's release identity and this repository's release workflow as signers (`trustSigner`) and that grants egress to the registry host with a `dialAddress` hole for kind's service CIDR, the step credential `e2e`, the env binding and the private `/tmp`; the registry CA through `SSL_CERT_FILE`, with `SSL_CERT_DIR` naming the image's bundle again | every admitted case - signatures (key, keyless trusted and untrusted, the published pdb-addon against the public-good root), egress and an in-band rule refusal, credentials, env, the private `/tmp` with and without the manifest requiring it, the `requirements` round trip, a fatal result, the deadline, the memory limit, `module.from` inside and outside its fence, unsigned, skipped under `allowEmpty`, and the `composeResource` fence admitting the guest's NopResources under a permit and refusing them by name without one |
| `strict` | no verifier; a policy that requires a signature for the plain variant only and grants egress to the registry host with no `dialAddress` hole | the no-verifier refusal, the block list refusing the service CIDR in band, env, credential and `/tmp` refusals |
| `lifecycle` | `--debug`, `--warm-modules` on the published pdb-addon, a PVC-backed cache bounded below that module's size, a readiness probe on `/readyz`, `--cosign-trust-instance public-good` with a policy requiring the published examples' signatures and trusting this repository's release workflow (no root file mounted) | readiness waiting for the warm-up, caches kept across a restart while the sweep bounds them, the trusted root fetched over TUF from the real public-good repository (the metrics, the keyless pdb-addon admitted, again after a restart from the cache on the volume) |

### The test module

`crates/e2e/guest` is the one module the suite drives: a Rust ABI v2 guest
on the `rust` scaffold's `wit/` and `proto/` (read in place rather than
vendored) in its own Cargo workspace, built with `make e2e.guest` on the
scaffold's pinned toolchain and committed as `fn.wasm`. Its Guest XR's
`spec` says what to exercise:

| Knob | What the module does |
|---|---|
| `nopResources`, `readyAfterSeconds` | composes that many provider-nop `NopResource`s, Ready after the delay; marks each ready once its observed counterpart is |
| `egress.url` | sends a GET request to the URL through `wasi:http` and writes the status and body, or the host's refusal, to `status.egress` |
| `env`, `credentials` | writes those variables and the keys of those step credentials to status |
| `privateTmp` | writes and reads back a file under `/tmp`, reporting the outcome |
| `requirements.environmentConfig` | asks Crossplane for that EnvironmentConfig and echoes its data once resolved |
| `fatal`, `spin`, `growMiB` | a fatal result with the message; a spin past the deadline; a memory growth refused at the limit |

The variants are one binary under nine manifests (`crates/e2e/guest/manifests`
and three signature variants derived from the plain one), each with a
distinct digest: the runtime caches a signature verdict per digest, so two
variants sharing one would share a verdict. The same `fn.wasm` is the
engine's `wasi:http` fixture (`crates/engine/tests/committed_guest.rs`).

### Running it locally

```shell
make e2e.up                # kind, Crossplane, provider-nop, the registry, the package (Docker running)
make e2e.test.default      # or strict, lifecycle; cargo test -p function-wasm-e2e --features cluster --test <profile> does the same
make e2e.down
```

`FUNCTION_BIN=<linux binary> make e2e.up` wraps a prebuilt runtime with
`Dockerfile.dev` instead of compiling the workspace in the Dockerfile's build
stage (what CI does). `KUBECONFIG` and the current context are the cluster's
after `controlplane.up`. `CROSSPLANE_VERSION=2.2.7 make e2e` runs the whole
suite on another Crossplane (the chart version, no `v`).

### The workflows

`ci.yml` runs lint with `--all-features` and `cargo test --workspace` on
every push. `/e2e` on a pull request runs `e2e.yml` - the guest suite with
`FUNCTION_WASM_REQUIRE_TOOLCHAINS=1` (a missing toolchain fails instead of
skipping), one build job per example toolchain (build, unit tests, lint,
codec and bindings drift), and the cluster job, which runs the three
profiles one after another on one kind cluster at the Makefile's
`CROSSPLANE_VERSION` and uploads `make e2e.dump` on failure. The jobs cache
the Rust builds and the guests' dependencies under keys prefixed `e2e-`, a
namespace of their own. The run builds a pull request's code in the default
branch's cache scope, so no trusted workflow (`ci.yml`, the release matrix)
ever restores an `e2e-` key, and a poisoned cache could mislead only this
workflow's verdict. GitHub hands a run that anyone may start (an
`issue_comment` run) a read-only cache token, under which every save is
skipped. The workflow declares `cache-mode: write` in response, the
documented opt-in that the namespace makes acceptable. A push to a
`release-*` branch runs the release matrix below.

### The release matrix

Crossplane maintains its three most recent minor releases, published
quarterly and each maintained for about nine months
([release cycle](https://docs.crossplane.io/latest/learn/release-cycle/)),
and the function supports the same three - the current minor, which every
`/e2e` run tests, and the two before it. `release-matrix-e2e.yml` (#189)
proves it before a release is cut. Each push to a `release-X.Y` branch (the
cut of a new line, or a patch on an existing one) builds the runtime from
the branch and runs `make e2e` three times in parallel, one kind cluster per
version, with the cluster dump uploaded for a failed version. `gh workflow run
"Release Matrix E2E" --ref <ref>` runs it by hand on any ref. The versions live
in two places and nowhere else: the Makefile's `CROSSPLANE_VERSION` default
is the current one, and the workflow's `CROSSPLANE_PREVIOUS` and
`CROSSPLANE_OLDEST` are the latest patch of the two older minors. Renovate
moves all three (`renovate.json5`) - the default to any new release, the
two pins to a new patch of their own minor only. When a new Crossplane
minor is released, Renovate's PR that moves the default is the signal to
shift both pins down one minor by hand in that PR. The `cut-release` skill
pushes the branch, waits for the run and cuts the tag only when it's green.
A red version is a release blocker, or the floor this page then states. The
supported versions today are 2.4, 2.3 and 2.2.
