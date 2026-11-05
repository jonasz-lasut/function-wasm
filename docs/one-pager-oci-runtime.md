# OCI runtime

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: draft (a second runtime that runs an OCI image inside a Firecracker microVM under the same manifest, policy layers and tooling as a WebAssembly module; the open points under Decisions wait for the owner)
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
Firecracker microVM. The runtime boots one VM per request from the image's
root filesystem. The VM has no network device. The process reads the
request from its standard input and writes the response to its standard
output. A small init program the runtime boots as the VM's first process
provides the services the WebAssembly guest gets through its world. The
manifest gains a `type` field, `wasm` or `oci`, and the artifact `guestfn
push` publishes gains a generic media type. Both runtimes then share the
manifest format, the artifact layout and the CLI. The operator installs
the runtime they need. The WebAssembly package remains small and
unprivileged. The OCI package needs `/dev/kvm` on its nodes.

## Goals

- **An OCI image as a module.** A Composition step references an image, or
  an artifact that wraps one, through the same `module` block. The runtime
  admits, fences, meters and audits it as it does a WebAssembly module. A
  reader of `docs/how-a-request-runs.md` finds the same nine steps, with
  step 8 running a VM instead of a component.
- **Hardware isolation by default.** The image's code runs under its own
  guest kernel under KVM. The guest can attack the hypervisor's surface,
  not the container runtime's.
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
  shares the host kernel. The record keeps a container sandbox for the
  development loop on machines without KVM, behind an explicit flag, and
  the runtime says so in its refusal when an operator uses it in a cluster.
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
new `crates/engine-oci` drives Firecracker behind the same trait. A Cargo
feature of the `function` crate selects the engine, and each package image
builds the binary with its own:

| package | engine | image contents beyond the binary | the node needs |
|---|---|---|---|
| `function-wasm` | Wasmtime | none | none |
| `function-oci` | Firecracker | the `firecracker` binary, the guest kernel with the init as its initramfs | `/dev/kvm` |

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
  under a generic media type `application/vnd.guestfn.manifest.v1+json`.
- The manifest's `image` field references the image by digest:
  `ghcr.io/acme/tagger@sha256:…`. `guestfn push` fills it in from the image
  it pushed or from the digest the user passed.

The Input pins the artifact's digest. The signature covers the artifact and
the artifact covers the manifest. The manifest pins the image, and the
runtime pulls the image by that digest with the step's pull credential. The
trust chain the trust model record describes extends to the image without
a second signature. An image index under the pinned image digest resolves
to the node's platform.

The runtime reads both manifest media types through 1.x. `guestfn push`
writes the generic one for `type: oci` artifacts from the first minor that
reads it, and for `type: wasm` artifacts from 2.0.0, which keeps an older
runtime reading a newer `guestfn`'s WebAssembly artifacts. When the Input
references a plain image, with no wrapping artifact, the runtime runs it as
a manifest-less module in the default sandbox, exactly as it runs a bare
`application/wasm` layer today.

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
`type: wasm` manifest. The file keeps its name, `wasmfn.yaml`, unless the
owner picks a generic one (Decisions).

### The sandbox

**What boots.** The runtime converts the image's layers into an ext4 block
image once per image digest. A tar-to-ext4 writer reads the layer tarballs
directly, applies whiteouts and preserves ownership and modes, all without
root. The content-addressed store keeps the block image beside the
compiled WebAssembly artifacts, under `engine::version()`, under the same
LRU sweep and the same per-process key. The key check refuses a planted
root filesystem before Firecracker sees it. The project builds its own guest kernel from a microVM
configuration with virtio block, virtio vsock, virtio RNG and the init
embedded as its initramfs. Renovate tracks the kernel and Firecracker
versions. `function_wasm_module_compile_duration_seconds` measures the
conversion.

**One VM per request.** The runtime starts Firecracker with the kernel, the
root filesystem read-only on one block device, a vsock device, memory sized
to the step's `limits.memory` under the operator's ceiling, one vCPU
(`--vm-vcpus`) and the deadline. The init mounts the root filesystem with
a tmpfs overlay and reads the run's description over vsock (env, user,
cwd, entrypoint and the proxy settings). It starts the process and pipes
the request to its stdin. It streams stderr to the host as it arrives and
collects stdout to end of file. After the exit it reports the status.
The runtime then kills the VM, which discards every change the run made,
and a fresh instance per request holds. A later revision may snapshot a booted VM per image digest
and restore it per request, which Firecracker does in tens of
milliseconds. The record puts it under Alternatives until measurements ask
for it.

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
proxy and the variables. The alternative, a plain CONNECT tunnel that
judges host and port only, is under Decisions.

**Credentials and env.** `requires.env` bindings become process environment
through the init. `requires.credentials` entries travel in the request
bytes, and `protowire::retain_credentials` withholds every other step
credential before the runtime sends the bytes to the VM, unchanged. The
runtime withholds the pull credential from the VM as it does from a
component.

**Time and randomness.** The guest's clock comes from the host through
kvm-clock, its randomness from virtio RNG. Both are what a container gets.

**Node requirements.** The pod needs `/dev/kvm`. The install page documents
a `DeploymentRuntimeConfig` with a `hostPath` of type `CharDevice` and the
`kvm` group among the container's supplemental groups. The runtime refuses
to serve with a sentence that states the device when it can't open it.
Firecracker runs as the pod's user with its own seccomp filter, and the
pod's cgroup is the jail, without `privileged` and without
`CAP_SYS_ADMIN`.

**The development loop.** A laptop without KVM runs the same binary with
`--sandbox=container`, which runs the image through the Docker daemon on
the socket, or a rootless `crun` where one is on `PATH`, with the same
init protocol over a Unix socket. The flag requires `--insecure` and logs
one warning line at startup, and `function validate` prints a warning
under every step when it runs under it. `make render` in an OCI example
uses it and nothing else does.

### Offline validation and the CLI

`function validate --resolve` pulls the artifact and reads the manifest. It
resolves the image digest to the node's platform and reads the image
config. It checks the contract's preconditions, that the config has an
entrypoint or a command, that the user resolves and that the platform
exists. It
prints the same admission verdicts as today.

| command | `type: oci` |
|---|---|
| `guestfn init --type oci --lang go\|python\|rust\|ts` | a project with a `Dockerfile`, a `main` that reads stdin and writes stdout around the language's function SDK (function-sdk-go, function-sdk-python, function-sdk-rust, the TypeScript codec the ts scaffold vendors), the same greeting behaviour as the WebAssembly scaffolds, unit tests that run natively, and a `wasmfn.yaml` with `type: oci` |
| `guestfn build` | `docker build` (or `buildx`) with the project's `Dockerfile`, then a contract check. The image runs once under the container sandbox with an empty request, and the verdict says whether stdout decoded as a response |
| `guestfn push` | pushes the image by digest when the project built it, or takes `--image <ref@sha256>`, then publishes the artifact with the manifest's `image` filled in, reproducible under `SOURCE_DATE_EPOCH` |
| `guestfn inspect` | the image config (entrypoint, user, platform, size), the manifest summary and the contract verdict |
| `guestfn scaffold composition` | unchanged |

The OCI scaffolds run the language's own SDK natively. An OCI guest is an
ordinary function with a twenty-line `main`. The guest behavioural suite
runs each OCI scaffold through the whole host under the container sandbox,
without KVM, with the same expectations the WebAssembly scaffolds meet.

### The Input

The `module` block, `compositionPolicy`, `limits` and `config` keep their
schema. The `apiVersion` the OCI package serves is under Decisions,
`wasm.fn.crossplane.io/v1` for both or `oci.fn.crossplane.io/v1` with the
same fields.

## Code shape

- `crates/function`: an `Engine` trait and a `Module` associated type in a
  new `engine.rs`. `runner.rs`, `cache.rs`, `validate.rs` and `ops.rs` take
  the trait, and the `wasm` and `oci` Cargo features select the engine.
  `manifest.rs` gains `type`, `image` and the per-type `abi` check.
  `oci.rs` reads both manifest media types and the wrapping artifact.
- `crates/engine`: the trait over Wasmtime, with its behaviour and its
  tests unchanged.
- `crates/engine-oci`: the Firecracker driver (its API socket, the machine
  config, the block and vsock devices and the lifecycle), the tar-to-ext4
  writer, the init protocol over vsock (a framed stream with the run
  description, stdin, stderr lines, stdout and the exit), the egress bridge
  onto `HttpRequester`, and the container sandbox behind the same protocol.
- `crates/init`: the guest init, a static binary that depends on nothing in
  the image. It mounts, overlays, runs the proxy with its authority, starts
  the process and relays the streams.
- `crates/guestfn`: `--type oci` through init, build, push and inspect, and
  the OCI template sets.
- `package/oci/`: `crossplane.yaml` and the shared Input CRD.
  `Dockerfile.oci` adds Firecracker and the kernel, and a `kernel/`
  directory holds the kernel configuration and the build the release
  workflow runs.
- `.github/workflows`: the kernel build, `build-oci` jobs in `e2e.yml`, an
  `oci` e2e profile on a KVM runner, and `publish-pkg.yml` publishing both
  packages.

## Tests

- Unit: the tar-to-ext4 writer against layer fixtures with whiteouts and
  ownership, read back with an ext4 parser. The init protocol framing. The
  manifest's `type`, `image` and per-type `abi` rules. The wrapping
  artifact in `push.rs`.
- `crates/engine-oci` integration: a static test image built in the test
  from a small binary that echoes a fixed response, converted and run under
  Firecracker on a KVM runner and under the container sandbox everywhere.
  The cases cover the deadline, the memory limit, a non-zero exit, stderr
  log lines and a proxied request against a fake requester.
- Conformance goldens: a new fixture family for the OCI refusals and the
  package mismatch sentence.
- Guest suite: the OCI scaffolds under the container sandbox.
- End-to-end: an `oci` profile with the `function-oci` package on kind with
  `/dev/kvm` mounted, on a KVM-capable runner, that exercises a committed
  test image through the same Guest XR.

## Documentation

An operators page for the OCI runtime (install, the device, the flags and
the container sandbox's warning), a guests page for the process contract
and the OCI scaffolds, the manifest page's `type` and `image` fields, the
artifact's media types on the trust model page, the Input reference's
`apiVersion` note, and the troubleshooting entries for the new refusals.

## Compatibility

The WebAssembly runtime's behaviour, wording, flags, metrics and artifacts
don't change in 1.x. The manifest's `type` defaults to `wasm`. The runtime
reads the generic manifest media type from the minor that adds it, and
`guestfn push` writes it for WebAssembly artifacts from 2.0.0. The
`function-oci` package starts alpha on the shared release train.

## Alternatives

- **gVisor as the sandbox.** A user-space kernel, with no KVM required on
  its ptrace platform, slower system calls and a software-only boundary.
  Rejected for the default. A later sandbox behind the same trait if a
  cluster can't offer KVM.
- **Kata Containers through a RuntimeClass.** The runtime would create a
  Pod per request and talk to it. Seconds of latency per request, RBAC to
  create Pods and no `crossplane render`. Rejected.
- **A plain OCI runtime (crun, youki) as the production sandbox.** Shares
  the host kernel. Kept for development only.
- **The image as a gRPC function server inside the VM.** Any existing
  function image would run unchanged, sandboxed, with the runtime as its
  proxy. It doubles the number of contracts and depends on each image's
  flags for an insecure listener. Under Decisions as a second contract.
- **The manifest as an OCI 1.1 referrer of the image.** Standard, but the
  signature on the image wouldn't cover the manifest, the manifest would
  need its own signature, and the Input would pin the image rather than
  the manifest. The wrapping artifact keeps one pinned digest and one
  signature.
- **The manifest as a file inside the image.** Reading it needs a pull, and
  admission couldn't refuse before the fetch. Rejected.
- **Snapshot and restore per image digest.** Firecracker restores a booted
  VM in tens of milliseconds. Deferred until cold boot measurements ask for
  it. The trait leaves room.
- **A separate repository.** The policy, proxy and tooling code is most of
  the product and changes together. Rejected.

## Decisions

The owner settles these before the work starts:

1. **Packaging.** Separate packages from one workspace, each built with
   its engine's Cargo feature (recommended), or one package that serves
   both types and includes Firecracker in every install.
2. **The contract.** The process contract alone (recommended for the first
   release), or the process contract plus a gRPC contract that wraps
   existing function images, declared by the manifest.
3. **Egress.** A TLS-decrypting proxy in the init with a per-run
   authority, for full rule parity on method and path (recommended), or a
   CONNECT tunnel that judges host and port only and leaves TLS to the
   guest.
4. **The manifest's placement.** A wrapping artifact whose manifest pins the
   image (recommended), or a referrer of the image.
5. **Names.** The manifest file keeps the name `wasmfn.yaml` for both
   types, or takes a generic name (`guestfn.yaml`, read beside
   `wasmfn.yaml` through 1.x). The generic media types under `vnd.guestfn`
   (recommended) or another namespace.
6. **The Input.** `wasm.fn.crossplane.io/v1` served by both packages, or
   `oci.fn.crossplane.io/v1` with the same fields (recommended).
7. **The development sandbox.** A container sandbox behind `--insecure`
   (recommended), or Firecracker only, with no loop on machines without
   KVM.
8. **Order.** The phases below, or the Firecracker sandbox before the
   tooling.

## Phases

1. The `Engine` trait: `crates/function` generic over the engine, Wasmtime
   behind it, with no behaviour change and the goldens unchanged.
2. The manifest's `type`, `image` and per-type `abi`, the generic media
   types read by the runtime, and `guestfn` init, build, push and inspect
   for `type: oci` with the contract check under the container sandbox.
3. `crates/engine-oci` with the container sandbox and the init protocol,
   the OCI scaffolds in the guest suite, and `function validate --resolve`
   for images.
4. The Firecracker sandbox, with the kernel build, the init as initramfs,
   the tar-to-ext4 writer, the VM lifecycle, the bounds and the `oci` e2e
   profile on a KVM runner.
5. The egress bridge, the env bindings and the credentials through the
   init, with audit and metrics parity.
6. The `function-oci` package, its install page and the publish workflow,
   plus one use-case example.
