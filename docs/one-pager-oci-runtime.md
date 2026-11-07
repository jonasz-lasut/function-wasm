# OCI runtime

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: draft (a second runtime that runs OCI images in a microVM behind the same manifest and policy layers as a WebAssembly module. The owner settled the packaging and the contract and the egress and the artifact and the names and the order on 2026-11-05. The renames and the Engine trait and the manifest's type merged on 2026-11-06 (#244, #246, #247). The sandbox waits for the evaluation's spike)
* Tracking: https://github.com/jonasz-lasut/function-wasm/issues/239

## Summary

function-wasm runs one kind of module, a WebAssembly component, in one
kind of sandbox, Wasmtime. The code around the sandbox doesn't depend on
it. The Input and its module sources, the module manifest, the policy
layers, the credential withholding, the egress client, the fences, the
caches, the signature check, `function validate` and the `guestfn` loop of
init, build, push and inspect all work on bytes and references. Only the
engine knows what a module is.

This record adds a second runtime, `function-oci`, built from the same
workspace. It runs an OCI image, any image, as the step's module, inside a
microVM with its own guest kernel. The runtime boots one VM per request
from the image's root filesystem. The VM has no network device. The
process reads the request from its standard input and writes the response
to its standard output. A small init program the runtime boots as the VM's
first process provides the services the WebAssembly guest gets through its
world. The manifest gains a `type` field, `wasm` or `oci`, and the
artifact `guestfn push` publishes moves to generic media types. Both
runtimes then share the manifest format, the artifact layout and the CLI.
The operator installs the runtime they need. The WebAssembly package
remains small and unprivileged. The OCI package needs `/dev/kvm` on its
nodes.

The project is at 0.6 and the owner accepts breaking changes before 1.0.
The names tied to WebAssembly that the OCI runtime would inherit change
outright, with no alias: the manifest file becomes `guestfn.yaml`, the
artifact's media types move under `vnd.guestfn`, and both packages serve
the Input as `guestfn.crossplane.io/v1`. The record proposed Firecracker
as the sandbox. The owner asked for an evaluation of the candidates
instead, which the Sandbox evaluation section holds, with a spike to
settle it.

## Goals

- **An OCI image as a module.** A Composition step references an image, or
  an artifact that wraps one, through the same `module` block. The runtime
  admits, fences, meters and audits it as it does a WebAssembly module. A
  reader of `docs/how-a-request-runs.md` finds the same nine steps, with
  step 8 running a VM instead of a component.
- **Hardware isolation by default.** The image's code runs under its own
  guest kernel under a hypervisor. The guest can attack the hypervisor's
  surface, not the container runtime's.
- **Only what the sandbox granted.** The VM has no network interface. The
  manifest requests egress, env bindings and credentials, and both policy
  layers permit them, as today. A guest that ignores the proxy the init
  offers has no route at all.
- **One CLI.** `guestfn init --type oci` scaffolds a project, `guestfn
  build` builds its image and proves the contract, `guestfn push` publishes
  the image and the artifact, `guestfn inspect` shows what the runtime
  sees, and `function validate --resolve` admits a Composition offline.
- **One manifest.** `requires`, `composes`, `config` and `minRuntime` keep
  their meaning. A platform team's Cedar policies and manifest reviews
  transfer without a second vocabulary.

## Non-goals

- **Running the image as a plain container in production.** A container
  shares the host kernel. Whether a container sandbox serves the
  development loop on machines without a hypervisor depends on the
  sandbox the evaluation picks, and the runtime would refuse it in a
  cluster without an explicit flag.
- **A second ABI for WebAssembly.** The component world remains the one
  WebAssembly contract. The OCI contract is a process contract.
- **Renaming the repository or the WebAssembly package.** `function-wasm`
  keeps its name. The OCI package is `function-oci`. A later record may
  rename the project once both runtimes have a release.
- **Kubernetes-level sandboxing.** The runtime never creates Pods, Jobs or
  RuntimeClass workloads. It remains a gRPC server, and `crossplane render`
  and the Development runtime keep working.
- **GPU, block devices, host mounts.** The VM sees its root filesystem, a
  scratch overlay and the init. It sees nothing from the pod.

## Design

### Two packages, one workspace

`crates/function` becomes generic over an `Engine` trait. The trait is what
`runner.rs` already calls. `compile` produces the cached, loadable form of
a module from its bytes or its reference. `inspect` reports the module's
layout and its contract verdict, for `guestfn` and `validate --resolve`.
`run` serves one request under `RunOptions`, with the egress requester, the
env, the private scratch and the deadline. `version` namespaces the
compiled store, and the engine registers its own metric series. The
Wasmtime engine in `crates/engine` keeps its behaviour behind the trait. A
new `crates/engine-oci` drives the microVM behind the same trait, through
a `Sandbox` trait of its own, one type per candidate in the evaluation. A
Cargo feature of the `function` crate selects the engine, and each package
image builds the binary with its own:

| package | engine | image contents beyond the binary | the node needs |
|---|---|---|---|
| `function-wasm` | Wasmtime | none | none |
| `function-oci` | the microVM sandbox | the VMM (a library or a binary, per the evaluation), the guest kernel, the init | `/dev/kvm` |

One binary never serves both. A `function-oci` pod that meets a `type:
wasm` artifact refuses it with one sentence that states the other package,
and the other way round. A step pointed at the wrong package fails at
admission with that sentence. `function validate` validates for the
runtime it runs in and prints the same sentence.

The OCI package versions with the workspace on the shared release train
(`/cut-release` tags both packages). Its contract is young. Its
documentation labels it alpha until the process contract and the sandbox
flags have held through two minors.

### The process contract

A module of `type: oci` is an OCI image. The runtime runs the image's
configured entrypoint and command once per request, as the image's
configured user, in the image's configured working directory, with the
image's configured environment plus the bindings the manifest requested.
The contract fits in a table:

| stream | content |
|---|---|
| standard input | the raw `RunFunctionRequest` bytes the proxy forwards, as the component's `run` receives them, then end of file |
| standard output | the raw `RunFunctionResponse` bytes, read to end of file after exit. The proxy returns them verbatim and appends `meta` at the wire level when the guest omitted it, as today |
| standard error | log lines. A line that parses as a JSON object with `level` and `msg` becomes a typed log line with its other keys as fields. Every other line logs at info. `module` and `digest` attach as they do for the typed `log` import |
| exit status | zero with a response on stdout is a run. Non-zero is `module <desc> failed: process exited with status <n>: <the last stderr line>`, a fatal result on a fresh response, as a component's `err(string)` is |

The process gets the image's own arguments and nothing more. The runtime
sends it no signal before the deadline. At the deadline the runtime kills
the VM and returns the deadline fatal in the wording family the engine uses
today. The process sees `/tmp` writable and the rest of its root
filesystem writable through an overlay the VM discards on exit.
`requires.filesystem.privateTmp` remains accepted and keeps its meaning, a
scratch space private to the run. The manifest's `abi` field versions this
contract for `type: oci` (`abi: 1`), the way it versions the component
world for `type: wasm` (`abi: 2`). The runtime reads `type` first, then
holds `abi` to the contract that type selects.

### The artifact

`guestfn push` publishes one artifact per module today. It has an
`application/wasm` layer, the manifest as a second layer of media type
`application/vnd.wasmfn.manifest.v1+json`, a WASM config that lists the
layer digests, and the standard `org.opencontainers.image.*` annotations.
An image's layers are its root filesystem, and a manifest layer would break
every container runtime that pulls the image. The artifact for `type: oci`
wraps the image instead:

- `artifactType: application/vnd.guestfn.module.v1`, the empty config
  descriptor OCI 1.1 defines for artifacts, and one layer, the manifest,
  under the generic media type `application/vnd.guestfn.manifest.v1+json`.
- The manifest's `image` field references the image by digest:
  `ghcr.io/acme/tagger@sha256:…`. `guestfn push` fills it in from the image
  it pushed or from the digest the user passed.

The Input pins the artifact's digest. The signature covers the artifact and
the artifact covers the manifest. The manifest pins the image, and the
runtime pulls the image by that digest with the step's pull credential. The
trust chain the trust model record describes extends to the image without
a second signature. An image index under the pinned image digest resolves
to the node's platform.

The generic media type replaces `application/vnd.wasmfn.manifest.v1+json`
outright, for WebAssembly artifacts too, and the runtime reads the old one
no longer. A WebAssembly artifact pushed by an older `guestfn` runs again
after a push by the new one. When the Input references a plain image, with
no wrapping artifact, the runtime runs it as a manifest-less module in the
default sandbox, exactly as it runs a bare `application/wasm` layer today.

### The manifest

```yaml
type: oci          # wasm (the default when absent) or oci
abi: 1             # the process contract; 2 is the component world
name: tagger
version: 0.3.0
image: ghcr.io/acme/tagger@sha256:…   # written by guestfn push
requires:
  egress:
    http:
      - method: GET
        host: cmdb.example.org
        path: /teams/
  credentials: [cmdb]
config:
  schema: {…}
```

The other fields keep their meaning and their validation. `guestfn manifest
validate` holds `image` to a digest-pinned reference and refuses it in a
`type: wasm` manifest. The file is `guestfn.yaml` for both types. The
rename from `wasmfn.yaml` goes through the scaffolds, the examples, the
goldens, `module.manifestPath`'s documentation and the guests' pages in
one change, with no alias.

### The sandbox

The design below holds for every candidate in the evaluation. Where a
candidate differs, the evaluation says how.

**What boots.** The runtime unpacks the image's layers once per image
digest, applies the whiteouts and preserves ownership and modes without
root, into the form the sandbox boots from: an ext4 block image written
straight from the layer tarballs for a VMM with block devices only, or a
directory for a VMM that shares files through virtio-fs. The
content-addressed store keeps the result beside the compiled WebAssembly
artifacts, under `engine::version()`, under the same LRU sweep and the
same per-process key. The key check refuses a planted root filesystem
before the VMM sees it. The guest kernel comes with the package, either
built by the project from a microVM configuration or supplied by the VMM,
and Renovate tracks it with the VMM.
`function_wasm_module_compile_duration_seconds` measures the unpacking.

**One VM per request.** The runtime starts the VM with the kernel, the
root filesystem read-only, a vsock device, memory sized to the step's
`limits.memory` under the operator's ceiling, one vCPU (`--vm-vcpus`) and
the deadline. The init mounts the root filesystem with a tmpfs overlay and
reads the run's description over vsock (env, user, cwd, entrypoint and the
proxy settings). It starts the process and pipes the request to its stdin.
It streams stderr to the host as it arrives and collects stdout to end of
file. After the exit it reports the status. The runtime then kills the VM,
which discards every change the run made, and a fresh instance per request
holds. A later revision may snapshot a booted VM per image digest and
restore it per request where the VMM supports it. The record puts it under
Alternatives until measurements ask for it.

**Bounds.** `limits.timeout` is the VM's wall clock. `limits.memory` is the
VM's memory. The run slots, the fair scheduler by digest, the memory pool,
the step slots and the gRPC deadline cap apply unchanged, because they wrap
`Engine::run`. A VM that exceeds its memory kills its own process. The init
reports the exit, and the fatal states the limit as the Wasmtime limiter's
does.

**Egress.** The VM has no network interface. The init listens on
`127.0.0.1:3128` inside the guest and sets `HTTP_PROXY`, `HTTPS_PROXY` and
`NO_PROXY=` (empty) in the process's environment. The init decrypts TLS for
`https` targets with a certificate authority it generates at boot. It
installs that authority where the common runtimes look (`SSL_CERT_FILE`,
`REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`, `/etc/ssl/certs`) and relays
each request in the clear over vsock to the host. On the host the run's
egress client judges it as it judges a component's. The manifest's rules on
method, host and path apply, as do the block list per resolved address,
the redirect check per hop, the budgets, the rate limit, the audit line and
the outcome metric. A refusal comes back as an HTTP 502 with the refusal
sentence as its body. Without a `requires.egress` grant the init skips the
proxy and the variables. A VMM's own socket forwarding (libkrun's TSI) is
off: it would give the guest the pod's network.

**Credentials and env.** `requires.env` bindings become process environment
through the init. `requires.credentials` entries travel in the request
bytes, and `protowire::retain_credentials` withholds every other step
credential before the runtime sends the bytes to the VM, unchanged. The
runtime withholds the pull credential from the VM as it does from a
component.

**Time and randomness.** The guest's clock comes from the host's
paravirtual clock, its randomness from virtio RNG. Both are what a
container gets.

**Node requirements.** The pod needs `/dev/kvm`. The install page documents
a `DeploymentRuntimeConfig` with a `hostPath` of type `CharDevice` and the
`kvm` group among the container's supplemental groups. The runtime refuses
to serve with a sentence that states the device when it can't open it. The
VMM runs as the pod's user, and the pod's cgroup is the jail, without
`privileged` and without `CAP_SYS_ADMIN`.

**The development loop.** A VMM with a macOS port runs the loop on a
laptop as it runs in the pod. For the candidates without one, a container
sandbox behind `--insecure` would run the image through the Docker daemon
or a rootless `crun` with the same init protocol over a Unix socket, with a
warning at startup and under every step `function validate` prints. The
spike's verdict says whether the project needs it.

### Sandbox evaluation

The owner asked which sandbox fits before the record commits to one, and
weighs a Rust code base as a plus. The candidates below all run an
unmodified OCI image under a guest kernel, except the last two rows, which
the table keeps for contrast. Edera, a Xen-based runtime the owner asked
about, is under Alternatives: it sandboxes pods at the node, not requests
inside a pod.

| candidate | language | form | rootfs | host-mediated I/O | macOS | notes |
|---|---|---|---|---|---|---|
| [libkrun](https://github.com/libkrun/libkrun) | Rust | a library linked into the binary, with a Rust API | virtio-fs from a directory, without a block image | vsock, with TSI off | yes, HVF on ARM64 | kernel from libkrunfw. crun's `krun` handler runs OCI containers on it today. Apache-2.0 |
| [Firecracker](https://github.com/firecracker-microvm/firecracker) | Rust | a separate process driven over an API socket | block devices only, so the ext4 writer | vsock | no | 1.17.0; snapshots and restore; the jailer; the widest production record |
| [cloud-hypervisor](https://github.com/cloud-hypervisor/cloud-hypervisor) | Rust | a separate process driven over an API socket | virtio-fs and block | vsock | no | 54; a fuller device model than the runtime needs |
| [Dragonball](https://github.com/kata-containers/kata-containers/tree/main/src/dragonball) | Rust | a library inside Kata's Rust runtime | virtio-fs and block | vsock | no | the default VMM of Kata 4.0's runtime-rs; built for Kata's shim |
| [gVisor](https://gvisor.dev) | Go | a user-space kernel behind an OCI runtime, `runsc` | a directory | pipes | Linux only | the systrap platform runs without `/dev/kvm`. A software boundary. Go |
| crun, youki | C, Rust | OCI runtimes | a directory | pipes | Linux only | namespaces on the host kernel; the development fallback at most |

The criteria, in the order the owner weighs them:

1. **The boundary.** A guest kernel under a hypervisor, with the device
   model as what the guest can attack. Every microVM candidate passes. gVisor's boundary
   is a user-space kernel, smaller than a container's exposure but
   software.
2. **Rust.** libkrun, Firecracker, cloud-hypervisor and Dragonball are
   Rust. libkrun alone offers a Rust API the engine links directly, which
   spares the engine a sidecar process, its API socket and its
   supervision.
3. **The image's root filesystem.** virtio-fs removes the ext4 writer and
   the block image cache: the runtime unpacks the layers into a directory
   and the VMM shares it read-only. Firecracker has block devices only.
4. **The development loop.** libkrun runs on macOS over HVF, with the same
   binary, the same kernel and the same init. Firecracker, cloud-hypervisor,
   Dragonball and gVisor need a container fallback on a laptop.
5. **Operations.** A library needs `/dev/kvm` and nothing else in the
   image. A process needs its binary, its socket directory and a
   supervisor. Firecracker's jailer adds a second layer the pod's cgroup
   and seccomp already provide.
6. **Maturity.** Firecracker runs Lambda and Fargate. libkrun runs
   podman's machines and crun's `krun` handler. Its `main` is a 2.0 with
   an API still moving, and the engine would pin a `stable-*` branch.
   cloud-hypervisor runs Kata deployments.

On the criteria, libkrun leads and Firecracker is the alternative the
project understands best. The spike picks one by measurement rather than
by reading:

- **The spike.** One week, two prototypes behind one `Sandbox` seam,
  libkrun and Firecracker, each running the committed test image's
  process contract with the init over vsock. Measured on a KVM runner and,
  for libkrun, on an Apple Silicon laptop: time from `run` to the
  process's first byte, request round trip for a 1 MiB request, resident
  memory per idle VM, the unpacking time for a 100 MB image, the behaviour
  under `limits.memory` and `limits.timeout`, and whether the pod runs
  without `privileged`.
- **The verdict.** The record takes the chosen sandbox into its Design,
  its measurements under Decisions, and the other candidate under
  Alternatives. The status stays `draft` until the first `function-oci`
  package, which makes it `implemented, revision 1.0`. gVisor stays the candidate for a cluster without
  `/dev/kvm`, behind the same `Sandbox` trait, if demand appears.

### Offline validation and the CLI

`function validate --resolve` pulls the artifact and reads the manifest. It
resolves the image digest to the node's platform and reads the image
config. It checks the contract's preconditions, that the config has an
entrypoint or a command, that the user resolves and that the platform
exists. It
prints the same admission verdicts as today.

| command | `type: oci` |
|---|---|
| `guestfn init --type oci --lang go\|python\|rust\|ts` | a project with a `Dockerfile`, a `main` that reads stdin and writes stdout around the language's function SDK (function-sdk-go, function-sdk-python, function-sdk-rust, the TypeScript codec the ts scaffold vendors), the same greeting behaviour as the WebAssembly scaffolds, unit tests that run natively, and a `guestfn.yaml` with `type: oci` |
| `guestfn build` | `docker build` (or `buildx`) with the project's `Dockerfile`, then a contract check. The image runs once under the sandbox with an empty request, and the verdict says whether stdout decoded as a response |
| `guestfn push` | pushes the image by digest when the project built it, or takes `--image <ref@sha256>`, then publishes the artifact with the manifest's `image` filled in, reproducible under `SOURCE_DATE_EPOCH` |
| `guestfn inspect` | the image config (entrypoint, user, platform, size), the manifest summary and the contract verdict |
| `guestfn scaffold composition` | unchanged |

The OCI scaffolds run the language's own SDK natively. An OCI guest is an
ordinary function with a twenty-line `main`. The guest behavioural suite
runs each OCI scaffold through the whole host on a KVM runner, with the
same expectations the WebAssembly scaffolds meet.

### The Input

The `module` block, `compositionPolicy`, `limits` and `config` keep their
schema. Both packages serve the Input as `guestfn.crossplane.io/v1`, which
replaces `wasm.fn.crossplane.io/v1` outright: the WebAssembly runtime
refuses the old group with a sentence that states the new one, `function
validate` prints the same sentence, and the CRD under `package/input/`,
the scaffolds' compositions, the examples and the goldens move with it.
The `v1beta1` deprecation goes with the old group.

## Code shape

- `crates/function`: an `Engine` trait and a `Module` associated type in a
  new `engine.rs`. `runner.rs`, `cache.rs`, `validate.rs` and `ops.rs` take
  the trait, and the `wasm` and `oci` Cargo features select the engine.
  `manifest.rs` gains `type`, `image` and the per-type `abi` check.
  `oci.rs` reads both manifest media types and the wrapping artifact.
- `crates/engine`: the trait over Wasmtime, with its behaviour and its
  tests unchanged.
- `crates/engine-oci`: the `Sandbox` trait and the chosen VMM behind it
  (the machine config, the devices and the lifecycle), the layer unpacking
  into the sandbox's root filesystem form, the init protocol over vsock (a
  framed stream with the run description, stdin, stderr lines, stdout and
  the exit), and the egress bridge onto `HttpRequester`.
- `crates/init`: the guest init, a static binary that depends on nothing in
  the image. It mounts, overlays, runs the proxy with its authority, starts
  the process and relays the streams.
- `crates/guestfn`: `--type oci` through init, build, push and inspect, and
  the OCI template sets.
- `package/oci/`: `crossplane.yaml` and the shared Input CRD.
  `Dockerfile.oci` adds the VMM and the kernel, and a `kernel/` directory
  holds the kernel configuration and the build the release workflow runs
  where the project builds its own.
- `.github/workflows`: the kernel build, `build-oci` jobs in `e2e.yml`, an
  `oci` e2e profile on a KVM runner, and `publish-pkg.yml` publishing both
  packages.

## Tests

- Unit: the layer unpacking against fixtures with whiteouts and
  ownership, read back from the form the sandbox boots. The init protocol
  framing. The manifest's `type`, `image` and per-type `abi` rules. The
  wrapping artifact in `push.rs`.
- `crates/engine-oci` integration: a static test image built in the test
  from a small binary that echoes a fixed response, unpacked and run under
  the sandbox on a KVM runner. The cases cover the deadline, the memory
  limit, a non-zero exit, stderr log lines and a proxied request against a
  fake requester.
- Conformance goldens: a new fixture family for the OCI refusals and the
  package mismatch sentence.
- Guest suite: the OCI scaffolds on a KVM runner.
- End-to-end: an `oci` profile with the `function-oci` package on kind with
  `/dev/kvm` mounted, on a KVM-capable runner, that exercises a committed
  test image through the same Guest XR.

## Documentation

An operators page for the OCI runtime (install, the device and the flags),
a guests page for the process contract
and the OCI scaffolds, the manifest page's `type` and `image` fields, the
artifact's media types on the trust model page, the Input reference's
`apiVersion` note, and the troubleshooting entries for the new refusals.

## Compatibility

The project is at 0.6, and the owner accepts the breaks below before 1.0
(2026-11-05). The WebAssembly runtime's admission, wording, flags and
metrics don't change otherwise. The manifest's `type` defaults to `wasm`.

| today | after | who moves |
|---|---|---|
| `wasmfn.yaml` | `guestfn.yaml` | the scaffolds, the examples, the goldens, `module.manifestPath` users |
| `application/vnd.wasmfn.manifest.v1+json` | `application/vnd.guestfn.manifest.v1+json` | every published artifact, by a new push |
| `wasm.fn.crossplane.io/v1` (and `v1beta1`) | `guestfn.crossplane.io/v1` | every Composition's Input, the CRD under `package/input/` |

The runtime refuses the old names with a sentence that states the new one,
and `function validate` prints the same sentence. The `function-oci`
package starts alpha on the shared release train.

## Alternatives

- **gVisor as the sandbox.** A user-space kernel, with no KVM required on
  its systrap platform, slower system calls and a software-only boundary.
  Behind the microVM candidates in the evaluation. A later sandbox behind
  the same `Sandbox` trait if a cluster can't offer KVM.
- **Kata Containers through a RuntimeClass.** The runtime would create a
  Pod per request and talk to it. Seconds of latency per request, RBAC to
  create Pods and no `crossplane render`. Rejected.
- **Edera.** A Xen-based hypervisor, with its control plane in Rust, that
  gives each pod its own microVM and kernel through a RuntimeClass. The
  node boots into Edera's hypervisor, which is the operator's platform
  decision, and the open source part, krata (the Xen control plane,
  GPL-2.0), is a component of the commercial Edera Protect. The function
  pod can't embed it, and a zone contains a whole pod. Rejected as the
  runtime's sandbox for the same reason as Kata. An operator who runs the runtime pod itself
  under an Edera RuntimeClass adds a second boundary around the whole
  runtime, which this record neither needs nor prevents.
- **A plain OCI runtime (crun, youki) as the production sandbox.** Shares
  the host kernel. A development fallback at most, if the evaluation picks
  a VMM without a macOS port.
- **The image as a gRPC function server inside the VM.** Any existing
  function image would run unchanged, sandboxed, with the runtime as its
  proxy. It doubles the number of contracts and depends on each image's
  flags for an insecure listener. Rejected by the owner (2026-11-05) in
  favour of the process contract alone.
- **The manifest as an OCI 1.1 referrer of the image.** Standard, but the
  signature on the image wouldn't cover the manifest, the manifest would
  need its own signature, and the Input would pin the image rather than
  the manifest. The wrapping artifact keeps one pinned digest and one
  signature.
- **The manifest as a file inside the image.** Reading it needs a pull, and
  admission couldn't refuse before the fetch. Rejected.
- **Snapshot and restore per image digest.** Firecracker restores a booted
  VM in tens of milliseconds, and libkrun has no snapshots. Deferred until
  cold boot measurements ask for it. The `Sandbox` trait leaves room.
- **A separate repository.** The policy, proxy and tooling code is most of
  the product and changes together. Rejected.

## Decisions

The owner settled these on 2026-11-05:

1. **Packaging.** Separate packages from one workspace, each built with its
   engine's Cargo feature. `function-wasm` keeps its size and runs
   without a device. `function-oci` needs `/dev/kvm`.
2. **The contract.** The process contract alone. A gRPC server inside the
   VM is under Alternatives.
3. **Egress.** A TLS-decrypting proxy in the init with a per-run
   authority, for full rule parity on method and path.
4. **The manifest's placement.** A wrapping artifact whose manifest pins
   the image by digest.
5. **Names.** The `guestfn` namespace: `guestfn.yaml`, the media types
   under `vnd.guestfn`, the Input group `guestfn.crossplane.io/v1`, with
   no alias for the old names (the project is at 0.x).
6. **The Input.** One generic group for both packages, replacing
   `wasm.fn.crossplane.io/v1`.
7. **Order.** The trait, the manifest and the tooling, the init protocol,
   then the microVM sandbox.

Open:

8. **The sandbox.** The evaluation's spike (libkrun against Firecracker)
   decides, and the verdict goes into the record.
9. **The development loop without KVM.** Follows the sandbox: a VMM with a
   macOS port runs the loop by itself, the others need a container
   fallback behind `--insecure`.

## Phases

1. Done (#247): the `Engine` trait in `function-wasm-engine-api`,
   `crates/function` generic over it as `engine::Selected`, Wasmtime behind
   it with no behaviour change and the goldens unchanged.
2. Done in part: the renames (`guestfn.yaml` and the media type in #244,
   the Input group and the generated CRD in #246), the manifest's `type`
   and per-type `abi` with `type: oci` refused by name (#247). Left for
   the next PR: the manifest's `image`, the wrapping artifact's
   `artifactType`, and `guestfn` init, build, push and inspect for `type:
   oci`, with the contract check deferred to phase 3.
3. The sandbox spike, then `crates/engine-oci` with the chosen VMM behind
   the `Sandbox` trait, the init and its protocol, the layer unpacking, the
   VM lifecycle, the bounds, `function validate --resolve` for images, the
   contract check in `guestfn build`, and the OCI scaffolds in the guest
   suite on a KVM runner.
4. The `oci` e2e profile on a KVM runner, with the kernel build where the
   project builds its own.
5. The egress bridge, the env bindings and the credentials through the
   init, with audit and metrics parity.
6. The `function-oci` package, its install page and the publish workflow,
   plus one use-case example.
