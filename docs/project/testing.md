# Testing

This page is for contributors adding or changing a test. It says which of the
two test tiers a change belongs in and how to run the end-to-end suite on your
machine. It assumes a Rust toolchain for the first tier and Docker for the
second.

`crossplane render` belongs to the
[development loop](../wasm/render-locally.md) and isn't part of either tier.

| Tier | Runs | What it covers |
|---|---|---|
| unit and component | `cargo test --workspace`, on every push and on a pull request that changes code | the engine, the runtime and `function validate` against in-memory stand-ins and recorded goldens, and every scaffold built and run through the host (the guest suite) |
| end-to-end | `make e2e`, an `e2e:` label on a pull request, the [release matrix](#the-release-matrix) on a release branch | each function's package installed by Crossplane in a kind cluster, as an XR author and an operator see it |

## The tier for a new test

| The change | The test |
|---|---|
| a guest-visible behaviour of the engine (a trap, a limit, a WASI interface) | a WAT fixture in `crates/engine-wasm/tests/engine.rs`, or a setting of the committed guest driven from `crates/engine-wasm/tests/committed_guest.rs` |
| an Input rule, a refusal, a policy decision | a unit test beside the code, a fixture under `crates/function/testdata/validate/` and a conformance golden |
| a scaffold change | the template, then `UPDATE_GOLDENS=1 cargo test -p guestfn`; the guest suite builds the goldens |
| a fixture or stand-in a test needs | `guestfn_engine_wasm::testing` for a guest component, `guestfn_runtime::testing` for a registry, a signer, an in-process runtime, a server or a policy (both behind the `testutil` feature) |
| something only Crossplane shows, such as a condition, an event, a resolved `requirements` entry, a pod restart or a scraped metric | a feature in the end-to-end suite |

`UPDATE_GOLDENS=1 cargo test -p function-wasm --test conformance` records a
conformance golden. A re-record is a behaviour change, so say so in the commit.
A suite uses the shared `testing` modules and never declares its own copy of a
fixture.

## The end-to-end suite

The suite is in `crates/e2e` (package `guestfn-e2e`). It gives each
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
   cloud. It pushes and signs the test module under every variant and creates
   the profiles' ConfigMaps, Secrets, volume and `DeploymentRuntimeConfig`s in
   `crossplane-system`. When the kind node has `/dev/kvm`, it installs the KVM
   device plugin and puts the node's `kvm` group into the `oci` runtime
   config as a supplemental group. It then creates the Guest XRD and writes
   `_output/e2e/state.json`, which the tests read.
3. `make e2e.image` builds the runtime image from
   `crates/function-wasm/Dockerfile`, or from
   `crates/function-wasm/Dockerfile.dev` around a prebuilt binary named by
   `FUNCTION_BIN`.
   `xpkg.build.function-wasm` builds the Function package from it, and
   `local.xpkg.deploy.function.function-wasm` installs it from Crossplane's
   cache with `packagePullPolicy: Never`, on the `wasm` runtime's `default` profile.
   `function-oci` follows the same way, from
   `crates/function-oci/Dockerfile` in Docker or from
   `crates/function-oci/Dockerfile.dev` around the prebuilt pieces
   `FUNCTION_OCI_TOOLS` names, on the `oci` runtime's `default` profile. `E2E_OCI` decides:
   `auto` (the default) installs it when the kind node has `/dev/kvm`, `1`
   requires it and `0` leaves it out, the two values CI's profile jobs set.
   `E2E_WASM` decides the same for `function-wasm`: `1` (the default)
   installs it and `0` leaves it out, which CI's `oci` profile jobs set.
   Each profile job installs its own runtime alone. Setup pushes every
   variant either way (the `oci` suite's wrong-type case names the `wasm`
   runtime's `plain` variant) and writes the `wasm/default` policy either
   way (the `oci` profile's `DeploymentRuntimeConfig` mounts it); `0` skips
   the package, the `wasm` profiles' runtime configs and the lifecycle
   cache volume.
   The test module as a process is built in a `rust` container on any host
   but Linux. `make e2e.up` builds it every time, and `cargo` rebuilds only
   what changed. A path given in `E2E_OCI_GUEST` names a prebuilt binary,
   which `make e2e.up` uses unchanged.

`make e2e.wasm-profile.<p>` and `make e2e.oci-profile.<p>` run one profile's
tests, `make e2e.test.wasm` and `make e2e.test.oci` one runtime's,
`make e2e.test` every profile the cluster has, `make e2e.down` deletes the
cluster and `make e2e` does everything. After a failed run, `make e2e.dump` writes the cluster's state under
`_output/controlplane-dump`. A failed test also keeps its namespace and dumps
its objects, events and the function pod's log under
`_output/e2e/logs/<namespace>`.

### The profiles

A profile is one runtime configuration, flags and an operator policy with the
mounts they need, for one of the two runtimes. `wasm` and `oci` are each an
installed Function. Each test binary (`crates/e2e/tests/<runtime>_<profile>.rs`)
switches its runtime's Function's `runtimeConfigRef` to its profile's
`DeploymentRuntimeConfig` (`crates/e2e/profiles/<runtime>/<profile>/drc.yaml`)
before its first test and waits for the rollout. Each binary uses one profile.

| Profile | What it tests |
|---|---|
| `wasm/default` | every case a runtime admits, under both signature verifiers |
| `wasm/strict` | the refusals of a narrow operator layer, the read fence's ceiling among them |
| `wasm/lifecycle` | warm-up, the caches across a restart and the trusted root over TUF |
| `oci/default` | the `function-oci` package ([the OCI runtime](../oci/install.md)) beside `function-wasm`, running the test module as a process in a Firecracker VM, and the published `function-oci-go` example. Its runtime config mounts the `wasm/default` policy |
| `oci/privileges` | not a profile of its own: a second suite on the `oci/default` runtime config. Its tests cover the refusal of an image that runs as root, the process's empty capability sets, fifteen privileged operations the kernel denies, and a `setuid` copy of the binary that runs as the image's user |
| `oci/snapshots` | the `oci/default` profile with `--enable-vm-snapshots` and the published `function-oci-go` warmed. Its tests cover the snapshot the warm-up takes, runs restored from a snapshot, eight Guests restoring it at once, and an env binding in a restored process |

The `default` profile runs the runtime with `--cosign-key` (the suite's key)
and `--cosign-trusted-root` (the in-memory Sigstore instance's root merged with
the public-good root). Its policy requires signatures from the suite's registry
and the published examples, and grants the egress, credential, env and private
`/tmp` cases the tests need. The policy is `crates/e2e/profiles/wasm/default/policy.cedar`
and the tests are in `crates/e2e/tests/wasm_default.rs`.

The `strict` profile has no verifier. Its policy requires a signature for the
plain variant only and grants egress to the registry host with no
`dialAddress` hole. It also scopes `requireResource` to the tenant's own
namespace. Its tests cover the no-verifier refusal, the block list refusing
the service CIDR in band, the env, credential and `/tmp` refusals, and the
operator ceiling refusing a cluster-scoped read.

The `lifecycle` profile runs with `--debug`, `--warm-modules` on the published
function-wasm-go, a PVC-backed cache bounded below that module's size, a readiness
probe on `/readyz`, and `--cosign-trust-instance public-good` with no root file
mounted. Its policy requires the published examples' signatures and accepts
this repository's `Publish Examples` workflow as a signer. Its tests cover readiness
waiting for the warm-up, the caches across a restart, and the trusted root
fetched over TUF from the real public-good repository.

### The test module

`crates/e2e/guest` is the one module the suite drives: a Rust ABI v2 guest in
its own Cargo workspace, reading the `rust` scaffold's `wit/` and `proto/` in
place. `make e2e.guest` builds it on the scaffold's pinned
toolchain and the result is committed as `fn.wasm`. The build embeds the
machine's paths. A rebuild never matches the committed file byte for byte.
`make e2e.guest` also writes `fn.wasm.stamp`, a digest of the module's sources
and of the module, and `crates/engine-wasm/tests/committed_guest.rs` fails
when either differs from the tree. Edit the module's source, then run
`make e2e.guest` and commit both files. The stamp leaves `Cargo.lock` out, so
a dependency update that changes the lock alone doesn't need a rebuild.

The Guest XR is the composite resource the suite creates for each test, and its
`spec` says what the module exercises:

| Field | What the module does |
|---|---|
| `nopResources`, `readyAfterSeconds` | composes that many provider-nop `NopResource`s, Ready after the delay, and marks each ready once its observed counterpart is |
| `egress.url` | sends a GET request to the URL through `wasi:http` and writes the status and body, or the host's refusal, to `status.egress` |
| `env`, `credentials` | writes those variables and the keys of those step credentials to status |
| `privateTmp` | writes and reads back a file under `/tmp`, reporting the outcome |
| `requirements.environmentConfig` | asks Crossplane for that EnvironmentConfig and echoes its data once resolved; the cluster cases of the read fence permit and refuse that ask by kind |
| `privileges` | as a process, tries what a Linux capability would allow and runs the `setuid` copy of its binary, reporting each outcome |
| `fatal`, `spin`, `growMiB` | a fatal result with the message, a spin past the deadline, a memory growth refused at the limit |

A variant is the same `fn.wasm` published under one manifest. The suite has
nine, one for each of the six manifests in `crates/e2e/guest/manifests/wasm`,
plus three signature variants derived from the plain one; the `oci` profile's
variants are the same declarations under `manifests/oci`, around the module
as a process. One of them, `oci-root`, is built on an image that doesn't
set a user, which the runtime refuses at load. Each has its own digest,
because the runtime caches a signature verdict per digest and two variants
sharing one would share a verdict. The same `fn.wasm` is the engine's `wasi:http` fixture
(`crates/engine-wasm/tests/committed_guest.rs`).

### Running it locally

```shell
make e2e.up                     # kind, Crossplane, provider-nop, the registry, the packages (Docker running; function-oci when the kind node has /dev/kvm)
make e2e.wasm-profile.default   # or strict, lifecycle; e2e.oci-profile.default with function-oci installed
make e2e.down
```

`cargo test -p guestfn-e2e --features cluster --test <runtime>_<profile>`
runs one profile as the make target does.
`FUNCTION_BIN=<linux binary> make e2e.up` wraps a prebuilt runtime with
`crates/function-wasm/Dockerfile.dev` instead of compiling the workspace in the
build stage of `crates/function-wasm/Dockerfile`, as CI does. After `controlplane.up`, `KUBECONFIG` and the
current context point at the cluster. `CROSSPLANE_VERSION=2.2.7 make e2e` runs
the whole suite on another Crossplane, given as the chart version without a
`v`. 2.2.7 is also the release matrix's oldest pin.

### The workflows

`ci.yml` runs lint with `--all-features` and `cargo test --workspace` on every
push, and on a pull request unless it touches only Markdown, JSON or SVG files.
Two more jobs run the `cargo test` targets that need tools of their own:

- `guest-suite` runs the guest suite on every such change, with
  `GUESTFN_REQUIRE_TOOLCHAINS=1` so a missing toolchain fails instead of
  skipping. The scaffolds don't have lockfiles, and an upstream release that
  breaks one fails this job on every pull request. The suite also runs the
  unit tests and `cargo clippy` of the end-to-end test module, a Cargo
  workspace of its own that `cargo test --workspace` doesn't reach.
- `vm-suite` builds the init and the fixture (musl), fetches the pinned
  Firecracker and kernel (`make oci.tools`) and runs the suite of the OCI
  engine in real VMs under the runner's KVM (`GUESTFN_REQUIRE_KVM=1`: a
  missing tool fails instead of skipping). On a pull request it runs when the
  change touches the OCI engine, the init, the protocol, the fixture,
  `crates/engine`, the dependencies or the Firecracker and kernel pins.

`e2e.yml` runs on a pull request's own events: when it opens, on every push
to it and when someone adds a label. The labels on the pull request select
the scenarios:

| Label | Runs |
|---|---|
| `e2e:all` | every scenario below |
| `e2e:wasm:all` | every `wasm` scenario |
| `e2e:wasm:default`, `e2e:wasm:strict`, `e2e:wasm:lifecycle` | one `wasm` profile |
| `e2e:wasm:examples` | the `build (<lang>)` jobs of the WebAssembly examples |
| `e2e:oci:all` | every `oci` scenario |
| `e2e:oci:default`, `e2e:oci:privileges`, `e2e:oci:snapshots` | one `oci` profile |
| `e2e:oci:examples` | `build (oci-go)` |

A pull request with more than one of these labels runs their union. A
labelled pull request runs again on every push until the label is removed,
and a newer commit cancels the run of the one before it. A pull request
without an `e2e:` label doesn't run a scenario.

The `select` job reads the labels and calls two reusable workflows.
`e2e-wasm.yml` has the scenarios of `function-wasm`:

- `build-tools` compiles the runtime and `guestfn` once
- one `e2e (<profile>)` job per selected profile, each on a kind cluster of
  its own at the Makefile's `CROSSPLANE_VERSION` with `function-oci` left out
  (`E2E_OCI=0`). The profiles run side by side, and a failed job uploads
  `make e2e.dump`
- one build job per example toolchain: build, unit tests, lint, and codec and
  bindings drift, each module wrapped by the `guestfn` that `build-tools`
  compiled

`e2e-oci.yml` has the scenarios of `function-oci`:

- `build-tools` compiles `function-oci`, the init and the test module as a
  process (musl) and fetches the pinned Firecracker and kernel
  (`make oci.tools`)
- one `e2e (<profile>)` job per selected profile, each on a cluster of its
  own with `function-oci` alone (`E2E_WASM=0 E2E_OCI=1`)
- `build (oci-go)` vets and tests the process guest and builds its image,
  pushing nothing

The workflow doesn't post a comment or a commit status, because every job is
already a check on the pull request with a link to its run. One job,
`e2e-gate`, is the check the ruleset on `main` requires. It starts with the
run and stays in progress until the scenarios have ended, so the commit
doesn't count as passed on an earlier run's result while a run is still
going. It then ends as the `e2e-verdict` job did. That job passes when no scenario was
selected or every selected one passed. It fails when a selected scenario, or
a build that scenario waited for, failed or was cancelled. On a labelled pull
request a new commit can't clear a failure, because the push starts the run
again. The workflow reads the labels the pull request has when the run
starts, so starting an old run again selects the current labels.

Any label starts the workflow, an unrelated one such as `kind:bug` too. That
run takes a concurrency group of its own, which keeps it from cancelling a
run in progress, and it doesn't run a scenario. Its `e2e-verdict` looks up
the verdict the head commit already has
(the latest `e2e-gate` check that ran to an end) and repeats it, so an
unrelated label can't replace a failure with a pass. With no verdict yet it
passes, unless the pull request has an `e2e:` label: the run that owes the
verdict is then still going, and this one fails until that run's gate
replaces it.

A pull request's run uses the pull request's own copy of the three workflow
files, so a change to them is exercised before it merges. A pull request
from a fork gets a read-only token without secrets, which is all the jobs
use.

A pull request's run restores caches and saves none, as in `ci.yml`. A
second workflow, `e2e-caches.yml`, saves them on `main` and
never runs on a pull request. It
calls the same two reusable workflows without a profile, which runs both
`build-tools` jobs and every example's build job without a cluster, and its
`seed-suite-cache` job compiles the end-to-end crate under the key every
profile job restores. Because the jobs that save a cache are the jobs that
restore it, the keys can't drift apart.

A cache key reads the compiler, the runner and the dependency files, never
the sources, so a push to `main` runs only the builds whose key it can have
changed. The workflow's `changes` job lists the files the push touched. A
manifest, a lockfile or a toolchain file among them selects the builds that
read it: the Rust builds, the WebAssembly examples or the process guest. A
push that touches none runs nothing.

One run a day builds everything and saves the keys that are missing. It
covers what no push changes: a new stable Rust or runner image, and a cache
GitHub evicted after a week unread. The release matrix restores `main`'s
caches and saves none.

GitHub's Linux runners expose `/dev/kvm` to a udev rule the OCI jobs and
the legs of the release matrix install, and the kind node inherits it. The
node's device is `root:kvm 0660` whatever the rule says, and the pod's user is
the package's. The suite's setup installs the device plugin
(`crates/e2e/cluster/kvm-device-plugin.yaml`), reads the node's `kvm` gid and
puts it into the `oci` runtime config as the supplemental group of the pod: the
recipe on the operator page for a hosted cluster. The same page says why a
`hostPath` mount of the device isn't enough. A
failed profile job prints the description and logs of every function pod into
the job log (`make e2e.dump` does that before the dump itself).

### The release matrix

Crossplane maintains its three most recent minor releases
([release cycle](https://docs.crossplane.io/latest/learn/release-cycle/)), and
the function supports the same three. A pull request's end-to-end run tests
the current minor, and the matrix adds the two before it.

`release-matrix-e2e.yml` runs on every push to a `release-function-wasm-X.Y`
or `release-function-oci-X.Y` branch, and the branch's name says which
function it tests. Its `plan` job reads the runtime from the name and calls
that runtime's reusable workflow, `e2e-wasm.yml` or `e2e-oci.yml`, once with
the three Crossplane versions. The runtime is built from the branch once, and
every profile runs on a kind cluster of its own per version: nine jobs in
parallel, each uploading the cluster dump when it fails. A `function-wasm`
branch doesn't run a VM or an `oci` profile, and a `function-oci` branch
doesn't run a `wasm` profile: the branch doesn't release what those test. A red job
blocks the release.
`gh workflow run "Release Matrix E2E" --ref <ref> -f runtime=<wasm|oci>` runs
it by hand on any ref.

The Makefile's `CROSSPLANE_VERSION` default names the current version. The
workflow's `env` (`CROSSPLANE_PREVIOUS`, `CROSSPLANE_OLDEST`) pins the latest
patch of the two older minors. A red version blocks the release: the release
process pushes the branch, waits for the matrix and tags only when it's green.

On a `function-oci` branch the matrix also runs the OCI engine's VM suite
(`make oci.test`) on nine pairs: the three most recent Firecracker minors against the three guest kernel
lines Firecracker supports (6.18, 6.1 and 5.10). The Dockerfile's pins are the
current Firecracker and kernel, and the workflow's `env` pins the others. Only
the pair the image pins blocks a release, and any other pair that fails stays
out of the record. A last job writes what the released function passed to a
`tested-versions` artifact: the function, the Crossplane versions and, for
`function-oci`, the pairs. The release skills use it to open the pull request
that updates the function's table in `README.md`.
