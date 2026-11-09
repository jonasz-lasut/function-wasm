# OCI runtime

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: draft (a second runtime that runs OCI images in a microVM behind the same manifest and policy layers as a WebAssembly module. The owner settled the packaging and the contract and the egress and the artifact and the names and the order on 2026-11-05. The renames and the Engine trait and the manifest's type merged on 2026-11-06 (#244, #246, #247). The first `function-oci` build runs a process under Firecracker behind the `Sandbox` seam. On 2026-11-09 the owner kept Firecracker for now and left scaffolding and building an image to its author, with `guestfn push` to publish it. Snapshots are in 0.1 as an alpha feature. The egress proxy gets a design of its own first, in [#280](https://github.com/jonasz-lasut/guestfn/issues/280), and [#281](https://github.com/jonasz-lasut/guestfn/issues/281) has the order to 0.1)
* Tracking: https://github.com/jonasz-lasut/guestfn/issues/239

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
world. The manifest's contract states the engine, `wasm@1.0` or `oci@1.0`,
and the artifact `guestfn push` publishes moves to generic media types. Both
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
instead, which the Sandbox evaluation section holds. Firecracker stays
for now (Jonasz, 2026-11-09). The first build runs on it, and the spike
the evaluation planned didn't run.

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
- **One CLI.** `guestfn push --image` publishes an image as a module, with
  its manifest appended as one more layer. `guestfn inspect` shows what
  the runtime sees in it, and `function validate --resolve` admits a
  Composition offline.
- **One manifest.** `requires`, `composes`, `config` and `minRuntime` keep
  their meaning. A platform team's Cedar policies and manifest reviews
  transfer without a second vocabulary.

## Non-goals

- **Running the image as a plain container in production.** A container
  shares the host kernel. Whether a container sandbox serves the
  development loop on machines without KVM is open (decision 9), and the
  runtime would refuse it in a cluster without an explicit flag.
- **A second ABI for WebAssembly.** The component world remains the one
  WebAssembly contract. The OCI contract is a process contract.
- **Scaffolding and building the image.** The author writes the project
  and its `Dockerfile` and builds the image with their own tools.
  `guestfn` publishes the result (decision 12).
- **Renaming the WebAssembly package.** The repository became
  `jonasz-lasut/guestfn` at 0.6 (#252); the packages, `function-wasm` and
  `function-oci`, kept their names.
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
Wasmtime engine in `crates/engine` keeps its behaviour behind the trait. An
`oci` module of the same crate runs the microVM behind the same trait,
through a `Sandbox` trait of its own with Firecracker behind it. A Cargo
feature of the `function` crate selects the engine, and each package
image builds the binary with its own:

| package | engine | image contents beyond the binary | the node needs |
|---|---|---|---|
| `function-wasm` | Wasmtime | none | none |
| `function-oci` | Firecracker | the Firecracker binary, the guest kernel, the init | `/dev/kvm` |

One binary never serves both. A `function-oci` pod that meets a `type:
wasm` artifact refuses it with one sentence that states the other package,
and the other way round. A step pointed at the wrong package fails at
admission with that sentence. `function validate` validates for the
runtime it runs in and prints the same sentence.

The OCI package's contract is young. Its documentation labels it alpha
until the process contract and the sandbox flags have held through two
minors. The repository's release tags are function-wasm's today:
`v0.1.0` exists, and the latest is `v0.6.0`. The tags and releases get
cleaned up for a monorepo, and the scheme that versions and releases the
OCI package is open (decision 15).

### The process contract

A module of contract `oci@1.0` is an OCI image. The runtime runs the image's
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
scratch space private to the run. The manifest's `contract` field states
this contract as `oci@1.0` and the component world as `wasm@1.0`: the
engine kind, then the version of that engine's contract. A later minor
extends a contract with new fields. The runtime reads the engine kind from
the field and refuses a version its engine doesn't offer. The field
was `abi`, an integer, through 0.6. An OCI image has no ABI, so the owner
replaced it with no alias (decision 10).

### The artifact

`guestfn push` publishes one artifact per WebAssembly module: the
`application/wasm` layer and the manifest as a second layer of media type
`application/vnd.guestfn.manifest.v1+json`, under a WASM config that lists
the layer digests and the standard `org.opencontainers.image.*`
annotations. `guestfn push` publishes an OCI module the same way, as the
image plus one layer. `guestfn push <ref> --image <image@sha256:…>`
appends a gzip tar layer holding the manifest at `/.guestfn/manifest.json`
to the image's manifest. The layer's `diff_id` and a history entry go into
the image config, and the annotation `io.crossplane.guestfn: manifest`
onto the layer's descriptor, the way Crossplane's `xpkg append` adds a
package's extensions. The result goes into the image's repository under
the reference given. An index gets the layer in each of its platform
manifests.

The appended layer is an ordinary filesystem layer. The image remains a
valid OCI image that every container runtime pulls and runs. The Input
pins the module's digest, which covers the image and the manifest, so one
cosign signature covers both and the trust model doesn't change. The
runtime finds the annotated layer in
the manifest and reads that one small blob before it pulls the rest, so
admission refuses before any layer comes down. An image with no such layer
runs as a manifest-less module in the default sandbox, exactly as a bare
`application/wasm` layer does. function-wasm refuses a module image by
name, since the OCI runtime's package serves it.

The generic media type `application/vnd.guestfn.manifest.v1+json` replaced
`application/vnd.wasmfn.manifest.v1+json` outright for WebAssembly
artifacts, and the runtime reads the old one no longer. A WebAssembly
artifact pushed by an older `guestfn` runs again after a push by the new
one.

### The manifest

```yaml
contract: oci@1.0  # the engine and its contract's version; wasm@1.0 is the component world
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
`wasm@1.0` manifest. `guestfn push --image <ref@sha256:…>` writes the
image's digest into the published manifest as the module's provenance and
appends the manifest to that image, with no module file read. `guestfn
inspect` says that a module image is one and which image the manifest
records as its origin, and `--pull` refuses it: the OCI runtime pulls and
boots the image.
The file is `guestfn.yaml` for both contracts. The
rename from `wasmfn.yaml` goes through the scaffolds, the examples, the
goldens, `module.manifestPath`'s documentation and the guests' pages in
one change, with no alias.

### The sandbox

Firecracker is the sandbox for now (decision 8). The design below holds
for every candidate in the evaluation. Where a candidate differs, the
evaluation says how.

**What boots.** The runtime unpacks the image's layers once per image
digest, applies the whiteouts and preserves ownership and modes without
root, into the form the sandbox boots from. For a VMM with block devices
only, that form is a squashfs image, which a pure Rust writer builds
straight from the layer tarballs; a VMM that shares files through
virtio-fs takes a directory instead. The image config, with the user
resolved against the image's `/etc/passwd` and `/etc/group`, goes
inside the squashfs, so a stored root filesystem describes itself. The
content-addressed store keeps the result beside the compiled WebAssembly
artifacts, under `engine::version()`, under the same LRU sweep and the
same per-process key. The key check refuses a planted root filesystem
before the VMM sees it. The guest kernel comes with the package, either
built by the project from a microVM configuration or supplied by the VMM,
and Renovate tracks it with the VMM. The first build takes the kernel
Firecracker's CI builds from Amazon Linux's microVM kernel (6.18, the
guest kernel Firecracker supports since 1.16.1, with squashfs, overlayfs,
cgroups, virtio and vsock built in), pinned by checksum with Firecracker's
release in `Dockerfile.oci` and the Makefile. The artifacts are at a dated
prefix of Firecracker's bucket, whose retention the project doesn't
control. Building the kernel in the repository from that source and
config is what the record calls the project's own kernel build. Whether a
release may include the published kernel is open
([#281](https://github.com/jonasz-lasut/guestfn/issues/281)).
`guestfn_module_compile_duration_seconds` measures the unpacking.

**One VM per request.** The runtime starts the VM with the kernel, the
root filesystem read-only, a vsock device, memory sized to the step's
`limits.memory` plus the kernel's share, one vCPU (`--vm-vcpus`) and
the deadline. The init mounts the root filesystem with a tmpfs overlay and
reads the run's description over vsock (env, user, cwd, entrypoint and the
proxy settings). It starts the process and pipes the request to its stdin.
It streams stderr to the host as it arrives and collects stdout to end of
file. After the exit it reports the status. The runtime then kills the VM,
which discards every change the run made, and a fresh instance per request
holds. `--vm-snapshots` restores a snapshot of the booted VM in place of
the boot ([Snapshots](#snapshots)).

**Bounds.** `limits.timeout` is the VM's wall clock, the boot included.
`limits.memory` is the process's memory. The init runs it in a cgroup
capped at the limit. The VM gets the limit plus a fixed share for the
kernel, the init and the overlay. The run slots, the fair scheduler by digest, the memory pool,
the step slots and the gRPC deadline cap apply unchanged, because they
wrap `Engine::run`. The kernel kills a process past its cap. The
init reports the exit, and the fatal states the limit as the Wasmtime
limiter's does.

**Egress.** The VM has no network interface, and until the proxy exists
`function-oci` refuses a manifest that requires egress, by name. The proxy
gets a design of its own first, in
[#280](https://github.com/jonasz-lasut/guestfn/issues/280), before
phase 5. Decision 3 is its direction: the init listens on
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
off: it would give the guest the pod's network. The design has to answer
what this direction doesn't, such as the clients the proxy covers, the
authority under `--vm-snapshots` and the wire between the init and the
host.

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
a device plugin that advertises the node's `/dev/kvm` as a resource
(generic-device-plugin) and a `DeploymentRuntimeConfig` that requests one,
with the `kvm` group among the container's supplemental groups. The
runtime refuses to serve with a sentence that states the device when it
can't open it. The VMM runs as the pod's user, and the
pod's cgroup is the jail, without `privileged` and without
`CAP_SYS_ADMIN`.

**The development loop.** The loop runs on a Linux machine with
`/dev/kvm`, where the runtime boots the VM it boots in the pod.
Firecracker has no macOS port, and a machine without KVM has no loop. A
container sandbox behind `--insecure` would
give it one: it would run the image through the Docker daemon or a
rootless `crun` with the same init protocol over a Unix socket, with a
warning at startup and under every step `function validate` prints.
Whether 0.1 needs that fallback is open (decision 9).

### Sandbox evaluation

The owner asked which sandbox fits before the record commits to one, and
weighs a Rust code base as a plus. The comparison remains here as the
evaluation wrote it. The candidates below all run an unmodified OCI image
under a guest kernel, except the last two rows, which the table keeps for
contrast. Edera, a Xen-based runtime the owner asked about, is under
Alternatives: it sandboxes pods at the node, not requests inside a pod.

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
project understands best. The evaluation planned a spike to pick one by
measurement rather than by reading, with a prototype of each behind one
`Sandbox` seam. The first build of `function-oci` is the Firecracker
prototype. A static release binary, a published kernel and no library to
link made it the one to write first. It runs the end-to-end suite's `oci`
profile on a GitHub runner.

The spike didn't run. Firecracker stays for now (Jonasz, 2026-11-09,
decision 8), and the libkrun prototype and the comparison are off the
path to 0.1, under Alternatives. The status stays `draft` until the first
`function-oci` package, which makes it `implemented, revision 1.0`.
gVisor stays the candidate for a cluster without `/dev/kvm`, behind the
same `Sandbox` trait, if demand appears.

### Snapshots

`--vm-snapshots` restores a snapshot of an image's booted VM in place of a
boot. It's a prototype, off by default and Firecracker's alone, and 0.1
includes it as an alpha feature (decision 13). On a laptop's nested KVM
the median run went from 1123 ms to 250 ms.
[Per-image microVM snapshots](one-pager-vm-snapshots.md) is its design record.

### Offline validation and the CLI

`function validate --resolve` pulls the artifact and reads the manifest. It
resolves the image digest to the node's platform and reads the image
config. It checks the contract's preconditions, that the config has an
entrypoint or a command, that the user resolves and that the platform
exists. It
prints the same admission verdicts as today.

For an OCI module `guestfn` publishes and inspects, and the author
scaffolds the project and builds the image (decision 12):

| command | `oci@1.0` |
|---|---|
| `guestfn build` | refuses an `oci@1.0` manifest: the command builds WebAssembly modules, and the author builds the image with `docker build` (or `buildx`) |
| `guestfn push --image <ref@sha256:…>` | appends the manifest to the image the author pushed, as one more layer, and writes the image's digest into the manifest's `image`. It doesn't read a module file |
| `guestfn inspect` | says that a module image is one and which image the manifest records as its origin, with the manifest's summary. `--pull` refuses it |
| `guestfn scaffold composition` | unchanged |

An OCI guest is an ordinary function with a `main` around the language's
own SDK that reads the request from stdin and writes the response to
stdout. `examples/function-oci-go` is one in Go, with its `Dockerfile`.

### The Input

The `module` block, `compositionPolicy`, `limits` and `config` keep their
schema. Both packages serve the Input as `guestfn.crossplane.io/v1`, which
replaces `wasm.fn.crossplane.io/v1` outright: the WebAssembly runtime
refuses the old group with a sentence that states the new one, `function
validate` prints the same sentence, and the CRD under `package/wasm/input/`,
the scaffolds' compositions, the examples and the goldens move with it.
The `v1beta1` deprecation goes with the old group.

## Code shape

- `crates/function`: `engine.rs` selects the engine by Cargo feature
  (`wasm`, `oci`) and names it `Selected`; `runner.rs`, `cache.rs`,
  `validate.rs` and `ops.rs` take the trait. `manifest.rs` gains the
  `Contract` enum and `image`. `oci.rs` reads the wrapping artifact, and
  `resolver.rs` fetches an image through it into a layout.
- `crates/engine`, the one engine crate (the owner asked for few crates,
  2026-11-06): the `Engine` trait and what every engine shares at its
  root, the Wasmtime engine under the `wasm` feature, the OCI engine under
  `oci` as the `oci` module - the `Sandbox` trait with Firecracker
  behind it (`firecracker.rs`: the machine config, the devices, the
  lifecycle), `rootfs.rs` unpacking the layers into the squashfs root
  filesystem, `layout.rs` reading the image layout the runtime hands over,
  `image.rs` holding the image config and the run description,
  `initrd.rs` packing the initramfs around the init, `run.rs` applying the
  bounds and wording the fatals, with the egress bridge onto
  `HttpRequester` to come - and, as the crate's two binaries built static
  for musl with no feature on, the guest init (`guestfn-init`: it mounts,
  overlays, starts the process and relays the streams; the proxy with its
  authority comes with the egress phase) and the fixture process the VM
  tests run. `protocol.rs` is what the init and the host say to each
  other (`Description`, `Exit`, the frame kinds), framed over vsock.
- `crates/guestfn`: `push.rs` appends the manifest to an image under
  `--image`, and `inspect.rs` describes a module image. `buildcmd.rs`
  refuses an `oci@1.0` manifest.
- `package/oci/` beside `package/wasm/`: one package root per runtime, each
  with its `crossplane.yaml` and a copy of the Input CRD.
  `Dockerfile.oci` adds the VMM and the kernel, and a `kernel/` directory
  holds the kernel configuration and the build the release workflow runs
  where the project builds its own.
- `.github/workflows`: the kernel build, `build-oci` jobs in `e2e.yml`, an
  `oci` e2e profile on a KVM runner, and `publish-function.yml` publishing either
  package.

## Tests

- Unit: the layer unpacking against fixtures with whiteouts and
  ownership, read back from the form the sandbox boots. The init protocol
  framing. The manifest's `contract`, `image` and per-engine rules. The
  wrapping artifact in `push.rs`.
- The engine's VM suite runs a static test image built in the test
  from a small binary that echoes a fixed response, unpacked and run under
  the sandbox on a KVM runner. The cases cover the deadline, the memory
  limit, a non-zero exit, stderr log lines and a proxied request against a
  fake requester.
- Conformance goldens: a new fixture family for the OCI refusals and the
  package mismatch sentence.
- The example: `examples/function-oci-go` tests its function and the
  process contract natively, and the `build-oci-go` job runs those tests.
- End-to-end: an `oci` profile with the `function-oci` package on kind with
  `/dev/kvm` mounted, on a KVM-capable runner, that exercises a committed
  test image through the same Guest XR.

## Documentation

An operators page for the OCI runtime (install, the device and the flags),
a guests page for the process contract and for publishing an image as a
module, the manifest page's `contract` and `image` fields, the artifact's
media types on the trust model page, the Input reference's `apiVersion`
note, and the troubleshooting entries for the new refusals.

## Compatibility

The project is at 0.6, and the owner accepts the breaks below before 1.0
(2026-11-05). The WebAssembly runtime's admission, wording, flags and
metrics don't change otherwise.

| today | after | who moves |
|---|---|---|
| `wasmfn.yaml` | `guestfn.yaml` | the scaffolds, the examples, the goldens, `module.manifestPath` users |
| `application/vnd.wasmfn.manifest.v1+json` | `application/vnd.guestfn.manifest.v1+json` | every published artifact, by a new push |
| `wasm.fn.crossplane.io/v1` (and `v1beta1`) | `guestfn.crossplane.io/v1` | every Composition's Input, the CRD under `package/wasm/input/` |

The runtime refuses the old names with a sentence that states the new one,
and `function validate` prints the same sentence. The `function-oci`
package starts alpha. The scheme for its tags and releases is open
(decision 15).

## Alternatives

- **gVisor as the sandbox.** A user-space kernel, with no KVM required on
  its systrap platform, slower system calls and a software-only boundary.
  Behind the microVM candidates in the evaluation. A later sandbox behind
  the same `Sandbox` trait if a cluster can't offer KVM.
- **A libkrun prototype and the comparison.** The evaluation's spike was
  one week for two prototypes behind one `Sandbox` seam, libkrun and
  Firecracker, each running the committed test image's process contract
  with the init over vsock. Its measurements were to come from a KVM
  runner and, for libkrun, an Apple Silicon laptop: time from `run` to
  the process's first byte, request round trip for a 1 MiB request,
  resident memory per idle VM, the unpacking time for a 100 MB image, the
  behaviour under `limits.memory` and `limits.timeout`, and whether the
  pod runs without `privileged`. Dropped from the path to 0.1 (Jonasz,
  2026-11-09), with Firecracker staying for now.
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
  the host kernel. A development fallback at most, which decision 9
  leaves open, since Firecracker has no macOS port.
- **The image as a gRPC function server inside the VM.** Any existing
  function image would run unchanged, sandboxed, with the runtime as its
  proxy. It doubles the number of contracts and depends on each image's
  flags for an insecure listener. Rejected by the owner (2026-11-05) in
  favour of the process contract alone.
- **The manifest as an OCI 1.1 referrer of the image.** Standard, but the
  signature on the image wouldn't cover the manifest, the manifest would
  need its own signature, and the Input would pin the image rather than
  the manifest. The appended layer keeps one pinned digest and one
  signature.
- **A wrapping artifact beside the image.** The first build published the
  manifest as the one layer of an artifact of its own (`artifactType`
  `application/vnd.guestfn.module.v1` over the empty config) whose
  manifest pinned the image. One pinned digest and one signature too, but
  a second repository to name and sign and a second pull on every cold
  load, and no container runtime could run the artifact. The owner chose
  the appended layer on 2026-11-08 (decision 11).
- **The manifest as a plain file inside the image.** Reading it needs a
  pull of the layers, and admission couldn't refuse before the fetch. The
  annotated layer is that file, found through the manifest and read on
  its own.
- **An ext4 image as the root filesystem.** The first draft's form for a
  VMM with block devices only. A squashfs writer keeps the ownership
  without root, and the guest mounts the image read-only under its
  overlay, the read-only root the design describes. The build writes
  squashfs.
- **The VM's memory as the process's limit.** The first build sized the
  VM to `limits.memory` alone. Without swap the guest kernel thrashed its
  page cache for the whole budget before its killer acted. The cgroup cap
  inside the VM ends the process at once.
- **A `hostPath` mount of `/dev/kvm`.** The first draft's recipe for the
  pod. It puts the device node in the container but not in its device
  cgroup, so every `open` fails with `Operation not permitted` in an
  unprivileged pod, the runtime exits at startup and Crossplane's gRPC
  client finds the function's headless Service empty, which the e2e
  profile's first run showed. The device plugin admits the device, and
  the operator page keeps the symptom for the reader who meets it.
- **OCI scaffolds and an image build in `guestfn`.** `guestfn init --type
  oci --lang go|python|rust|ts` would write a project with a `Dockerfile`,
  a `main` that reads stdin and writes stdout around the language's
  function SDK, unit tests that run natively and a `guestfn.yaml` with
  `contract: oci@1.0`. `guestfn build` would run `docker build`, then a
  contract check: one run of the image under the sandbox with an empty
  request, with a verdict on whether stdout decoded as a response. The
  guest suite would run each scaffold through the whole host on a KVM
  runner. Dropped (Jonasz, 2026-11-09): scaffolding and building the image
  are the author's, and `guestfn push --image` wraps the result with the
  manifest layer.
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
4. **The manifest's placement.** A layer of the image, found by its
   annotation (first a wrapping artifact, revised by decision 11).
5. **Names.** The `guestfn` namespace: `guestfn.yaml`, the media types
   under `vnd.guestfn`, the Input group `guestfn.crossplane.io/v1`, with
   no alias for the old names (the project is at 0.x).
6. **The Input.** One generic group for both packages, replacing
   `wasm.fn.crossplane.io/v1`.
7. **Order.** The trait, the manifest and the tooling, the init protocol,
   then the microVM sandbox.

The owner decided this on 2026-11-06:

10. **The contract field.** The manifest's `abi` becomes `contract`, an
    enum written `<engine>@<major>.<minor>` with no alias: `wasm@1.0` is
    the component world, `oci@1.0` the process contract, and `type` goes,
    since the contract states the engine. A minor adds fields a module may
    use between versions; a major changes what the engine does with a
    module. The runtime refuses the old name as an unknown field.

The owner decided this on 2026-11-08:

11. **The appended layer.** `guestfn push --image` appends the manifest to
    the image as one more layer, the way Crossplane's `xpkg append` adds a
    package's extensions, instead of publishing a wrapping artifact beside
    it. The image remains runnable and the module's digest covers image
    and manifest, with one repository to name and sign.

The owner decided this on 2026-11-09:

8. **The sandbox.** Firecracker stays for now. The libkrun prototype and
   the comparison are off the path to 0.1, under Alternatives. The
   decision isn't a final verdict on the sandbox.

The owner decided these the same day:

12. **The CLI.** Scaffolding and building the image are the author's.
    `guestfn push --image` publishes the image with the manifest appended
    as a layer (decision 11), and `guestfn inspect` and `function validate
    --resolve` read what it published. The OCI scaffolds and the image
    build in `guestfn` are under Alternatives.
13. **Snapshots.** A snapshot belongs to one image and has the image
    and the VM size as its key. Restores go through the release binary's
    API. 0.1 includes snapshots as an alpha feature behind
    `--vm-snapshots`.
    [Per-image microVM snapshots](one-pager-vm-snapshots.md) has the
    design and the image-independent snapshot it rejected.
14. **The egress design.** The proxy gets a design of its own first, in
    [#280](https://github.com/jonasz-lasut/guestfn/issues/280), before
    phase 5. Decision 3 remains its direction. Whether 0.1 waits for the
    proxy isn't decided.
15. **Tags and releases.** The repository's tags and releases get cleaned
    up for a monorepo. The scheme is an open item of
    [#281](https://github.com/jonasz-lasut/guestfn/issues/281).

Open:

9. **The development loop without KVM.** Firecracker has no macOS port,
   and a machine without KVM has no loop. Whether 0.1 needs a container
   fallback behind `--insecure` is open.

## Phases

[#281](https://github.com/jonasz-lasut/guestfn/issues/281) has the
order from these phases to 0.1, the package's first release.

1. Done (#247): the `Engine` trait (first in its own crate, folded back into
   `crates/engine` on 2026-11-06 at the owner's ask),
   `crates/function` generic over it as `engine::Selected`, Wasmtime behind
   it with no behaviour change and the goldens unchanged.
2. Done. #244 renamed the manifest file and the media type. #246
   moved the Input group and generated the CRD. #247 added the manifest's
   `type` with its per-type contract, with `type: oci` refused by name.
   The next PR made `abi` the `contract` enum and added the manifest's
   `image` with `guestfn push --image`, which first published a wrapping
   artifact and now appends the manifest to the image as a layer (decision
   11), read back by `guestfn inspect`.
3. Done, on Firecracker: the engine crate's `oci` module behind the
   `Sandbox` trait, the init and its protocol, the layer unpacking into
   squashfs, the VM lifecycle, the bounds, `function validate --resolve`
   for images, the `oci` Cargo feature of the `function` crate with its
   flags, the image fetch of the resolver with the manifest read out of
   the image's annotated layer, and the env bindings and the credentials
   through the init.
4. Done: the `oci` e2e profile on a GitHub runner with KVM, as jobs of the
   `/e2e` workflow beside the `wasm` ones (a build job, the engine's VM suite
   and a cluster job with both runtimes installed), with the published
   kernel. The kernel build, where the project builds its own, remains
   open.
5. The egress bridge (the TLS-decrypting proxy in the init) with audit and
   metrics parity, after its design in
   [#280](https://github.com/jonasz-lasut/guestfn/issues/280) (decision
   14). Until then `function-oci` refuses a manifest that requires egress,
   by name.
6. Done in part ([#263](https://github.com/jonasz-lasut/guestfn/issues/263)):
   `examples/function-oci-go`, the hello function as an image on the
   process contract, published by `publish-examples.yml` through `docker
   buildx` and `guestfn push --image`, and the `oci` e2e case that pulls
   the published example from `ghcr.io`. The `function-oci` package's
   publish workflow (`Publish Function`, with `function-oci` chosen), its
   image (`Dockerfile.oci`), its package root (`package/oci`) and its
   operators page are in the tree. Left: the package's first release,
   0.1.0.
7. A prototype ([#255](https://github.com/jonasz-lasut/guestfn/issues/255)):
   snapshots per image behind `--vm-snapshots`
   ([Per-image microVM snapshots](one-pager-vm-snapshots.md)), in 0.1 as
   an alpha feature (decision 13).
   [#279](https://github.com/jonasz-lasut/guestfn/issues/279) has what's
   left.
