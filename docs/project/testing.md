# Testing

function-wasm has two test tiers and nothing in between: Rust unit and
component tests, run by `cargo test` on every push, and a Kubernetes-native
end-to-end suite, also Rust, run against a kind cluster by `make e2e`. There
are no render tests and no shell scenarios; `crossplane render` is a
[development loop](../guests/render-locally.md), not a test.

| Tier | Runs | What it proves |
|---|---|---|
| unit and component | `cargo test --workspace`, every push and pull request | the engine over WAT fixtures and the committed end-to-end guest; the runtime's admission, resolver, policy, egress and signature code against in-memory stand-ins (a registry, a Sigstore instance); `function validate` against recorded goldens; the served runtime over gRPC; every scaffold built and run through the host (the guest suite) |
| end-to-end | `make e2e` (`/e2e` on a pull request) | the package installed by Crossplane in a kind cluster: the runtime's behaviour as an XR author and an operator see it, one profile of runtime flags at a time |

## Where a new test goes

| The change | The test |
|---|---|
| a guest-visible behaviour of the engine (a trap, a limit, a WASI interface) | a WAT fixture in `crates/engine/tests/engine.rs`, or a knob of the committed guest driven from `crates/engine/tests/committed_guest.rs` |
| an Input rule, a refusal, a policy decision | a unit test beside the code, a fixture under `crates/function/testdata/validate/` and a conformance golden (`UPDATE_GOLDENS=1 cargo test -p function-wasm --test conformance` records it; a re-record is a behaviour change, say so in the commit) |
| a scaffold change | the template, then `UPDATE_GOLDENS=1 cargo test -p guestfn`; the guest suite builds the goldens |
| something only Crossplane can show: a condition, an event, a Secret read, a requirement resolved, a pod restarted, a metric as scraped | a feature in the end-to-end suite |

## The end-to-end suite

The suite lives in `crates/e2e` (package `function-wasm-e2e`), shaped like
Linkerd's `policy-test`: a namespace per test, typed helpers over kube-rs,
waits with timeouts, `assert_eq!` on what the cluster holds. Nothing is
provisioned from inside a test; the crate is empty without its `cluster`
feature, so `cargo test --workspace` builds none of it.

### The cluster

`make e2e.up` provisions everything through the crossplane build submodule's
make library (`build/makelib`, pinned kind, kubectl, helm and crossplane CLI
under `.cache/tools`) and the suite's own setup binary:

1. `controlplane.up`: a kind cluster (`KIND_CLUSTER_NAME`, default
   `function-wasm-e2e`) with Crossplane at `CROSSPLANE_VERSION` (default
   2.4.3; the environment overrides it).
2. `e2e-setup` (`crates/e2e/src/bin/setup.rs`): the `e2e` namespace; a
   `registry:3` behind Basic auth and TLS from a CA the setup generates
   (reused across runs); provider-nop (`PROVIDER_NOP_VERSION`); the test
   module pushed under every variant and signed; the profiles' ConfigMaps,
   Secrets and volume in `crossplane-system`; the Guest XRD; and
   `_output/e2e/state.json`, which the tests read.
3. The runtime image (`make e2e.image`: the Dockerfile, or `Dockerfile.dev`
   around a prebuilt binary named by `FUNCTION_BIN`), the Function package
   from it (`xpkg.build.function-wasm`), and the package installed from
   Crossplane's cache with `packagePullPolicy: Never`
   (`local.xpkg.deploy.function.function-wasm`), on the default profile.

`make e2e.test.<profile>` runs one profile's tests; `make e2e.test` the
three in turn; `make e2e.down` deletes the cluster; `make e2e` does all of
it. `make e2e.dump` writes the cluster's state under
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
| `default` | `--cosign-key` (the suite's key) and `--cosign-trusted-root` (the in-memory Sigstore instance's root merged with the public-good root); a policy that requires signatures from the suite's registry and the published examples, trusts the suite's release identity and this repository's release workflow, grants egress to the registry host with a `dialAddress` hole for kind's service CIDR, the step credential `e2e`, the env binding and the private `/tmp`; the registry CA through `SSL_CERT_FILE`, with `SSL_CERT_DIR` naming the image's bundle again | every admitted case: signatures (key, keyless trusted and untrusted, the published pdb-addon against the public-good root), egress and an in-band rule refusal, credentials, env, tmp with and without the requirement, the requirements round trip, a fatal result, the deadline, the memory limit, `module.from` inside and outside its fence, unsigned, and skipped under `allowEmpty` |
| `strict` | no verifier; a policy that requires a signature for the plain variant only and grants egress to the registry host with no `dialAddress` hole | the no-verifier refusal, the block list refusing the service CIDR in band, env, credential and tmp refusals |
| `lifecycle` | `--debug`, `--warm-modules` on the published pdb-addon, a PVC-backed cache bounded below that module's size, a readiness probe on `/readyz` | readiness waiting for the warm-up, caches surviving a restart while the sweep bounds them |

### The test module

`crates/e2e/guest` is the one module the suite drives: a Rust ABI v2 guest
on the rust scaffold's `wit/` and `proto/` (read in place, not vendored),
its own Cargo workspace, built with `make e2e.guest` on the scaffold's
pinned toolchain and committed as `fn.wasm`. Its Guest XR's spec says what
to exercise:

| Knob | What the module does |
|---|---|
| `nopResources`, `readyAfterSeconds` | composes that many provider-nop `NopResource`s, Ready after the delay; marks each ready once its observed counterpart is |
| `egress.url` | GETs the URL through `wasi:http` and writes the status and body, or the host's refusal, to `status.egress` |
| `env`, `credentials` | writes the named variables and the keys of the named step credentials to status |
| `privateTmp` | writes and reads back a file under `/tmp`, reporting the outcome |
| `requirements.environmentConfig` | asks Crossplane for that EnvironmentConfig and echoes its data once resolved |
| `fatal`, `spin`, `growMiB` | a fatal result with the message; a spin past the deadline; a memory growth refused at the limit |

The variants are one binary under nine manifests (`crates/e2e/guest/manifests`
and three signature variants derived from the plain one), each with a
distinct digest: the runtime caches a signature verdict per digest, so two
variants sharing one would share a verdict. The same `fn.wasm` is the
engine's wasi:http fixture (`crates/engine/tests/committed_guest.rs`).

### Running it locally

```shell
make e2e.up                # kind, Crossplane, provider-nop, the registry, the package (Docker running)
make e2e.test.default      # or strict, lifecycle; cargo test -p function-wasm-e2e --features cluster --test <profile> does the same
make e2e.down
```

`FUNCTION_BIN=<linux binary> make e2e.up` wraps a prebuilt runtime with
`Dockerfile.dev` instead of compiling the workspace in the Dockerfile's build
stage (what CI does). `KUBECONFIG` and the current context are the cluster's
after `controlplane.up`.

### In CI

`ci.yml` runs lint with `--all-features` and `cargo test --workspace` on
every push. `/e2e` on a pull request runs `e2e.yml`: the guest suite with
`FUNCTION_WASM_REQUIRE_TOOLCHAINS=1` (a missing toolchain fails instead of
skipping), one build job per example toolchain (build, unit tests, lint,
codec and bindings drift), and the cluster job: the three profiles in turn on
one kind cluster, `make e2e.dump` uploaded on failure.
