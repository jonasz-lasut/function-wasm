# OCI runtime

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: draft (a second runtime that runs OCI images in a microVM behind the same manifest and policy layers as a WebAssembly module. The owner settled the packaging and the contract and the egress and the artifact and the names and the order on 2026-11-05. The renames and the Engine trait and the manifest's type merged on 2026-11-06 (#244, #246, #247). The first `function-oci` build runs a process under Firecracker behind the `Sandbox` seam. On 2026-11-09 the owner kept Firecracker for now and left scaffolding and building an image to its author, with `guestfn push` to publish it. The engines and the two functions became crates of their own the same day. Snapshots are in 0.1 as an alpha feature. The egress proxy gets a design of its own first, in [#280](https://github.com/jonasz-lasut/guestfn/issues/280), and [#281](https://github.com/jonasz-lasut/guestfn/issues/281) has the order to 0.1)
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
image's command starts the `FunctionRunnerService` gRPC server every
Crossplane function is, and the runtime calls it over vsock. A small init program the runtime boots as the VM's
first process provides the services the WebAssembly guest gets through its
world. The manifest's contract states the engine, `wasm@1.0` or `oci@0.1`,
and the artifact `guestfn push` publishes moves to generic media types. Both
runtimes then share the manifest format, the artifact layout and the CLI.
The operator installs the runtime they need. The WebAssembly package
remains small and unprivileged. The OCI package needs `/dev/kvm` on its
nodes.

The project is at 0.6 and the owner accepts breaking changes before 1.0.
The names tied to WebAssembly that the OCI runtime would inherit change
outright, with no alias: the manifest file becomes `guestfn.yaml`, the
artifact's media types move under `vnd.guestfn`, and both packages serve
the Input as `guestfn.crossplane.io/v1`. Firecracker is the sandbox for
now (Jonasz, 2026-11-09). The status stays `draft` until the first
`function-oci` package, which makes it `implemented, revision 1.0`.

## Goals

- **An OCI image as a module.** Composition steps reference an image, or
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
- **One manifest.** `requires`, `composes` and `config` keep
  their meaning. A platform team's Cedar policies and manifest reviews
  transfer without a second vocabulary.

## Non-goals

- **Running the image as a plain container.** A container shares the host
  kernel, and `function-oci` 0.1 runs on KVM only (decision 9).
- **A second ABI for WebAssembly.** The component world remains the one
  WebAssembly contract. The OCI contract is a gRPC server in the VM.
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
Wasmtime engine in `crates/engine-wasm` keeps its behaviour behind the
trait. `crates/engine-oci` runs the microVM behind the same trait, through
a `Sandbox` trait of its own with Firecracker behind it. Each package has
a binary crate, `crates/function-wasm` and `crates/function-oci`, that
hands the runtime its engine and the engine's flags:

| package | engine | image contents beyond the binary | the node needs |
|---|---|---|---|
| `function-wasm` | Wasmtime | none | none |
| `function-oci` | Firecracker | the Firecracker binary, the guest kernel, the init | `/dev/kvm` |

One binary never serves both. A `function-oci` pod that meets a `type:
wasm` artifact refuses it with one sentence that states the other package,
and the other way round. A step pointed at the wrong package fails at
admission with that sentence. `function validate` validates for the
runtime it runs in and prints the same sentence.

The contract of the OCI package is young. Its documentation labels it alpha
until the contract and the sandbox flags have stayed the same through two
minors. The package has versions, tags and releases of its own
(decision 15).

### The contract

A module of contract `oci@0.1` is an OCI image. The runtime runs the image's
configured entrypoint and command once per request, as the image's
configured user, in the image's configured working directory, with the
image's configured environment plus the bindings the manifest requested.
The command starts a `FunctionRunnerService` server on port 9443, and the
runtime calls `RunFunction` once that port accepts a connection:

| part | content |
|---|---|
| the request | the raw `RunFunctionRequest` bytes the proxy forwards, as the component's `run` receives them, through a pass-through codec |
| the response | the raw `RunFunctionResponse` bytes. The proxy returns them verbatim and appends `meta` at the wire level when the guest omitted it, as today |
| a status other than OK | `module <desc> failed: run returned an error: <message> (gRPC status <code>)`, a fatal result on a fresh response, as a component's `err(string)` is |
| standard output and standard error | log lines. A line that parses as a JSON object with `level` and `msg` becomes a typed log line with its other keys as fields. Every other line logs at info. `module` and `digest` attach as they do for the typed `log` import |
| the process ending before it answers | `module <desc> failed: process exited with status <n> before answering the call: <the last log line>` |

The process gets the image's own arguments, or the manifest's `oci.args` in
place of the image's command: a function on a Crossplane SDK demands mTLS
without `--insecure`, and the call stays inside the VM. The runtime
sends it no signal before the deadline. At the deadline the runtime kills
the VM and returns the deadline fatal in the wording family the engine uses
today. The process sees `/tmp` writable and the rest of its root
filesystem writable through an overlay the VM discards on exit.
`requires.filesystem.privateTmp` remains accepted and keeps its meaning, a
scratch space private to the run. The manifest's `contract` field states
this contract as `oci@0.1` and the component world as `wasm@1.0`: the
engine kind, then the version of that engine's contract. From a contract's
1.0 a later minor extends it with new fields. The runtime reads the engine kind from
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
name, since the package for the OCI runtime serves it.

The generic media type `application/vnd.guestfn.manifest.v1+json` replaced
`application/vnd.wasmfn.manifest.v1+json` outright for WebAssembly
artifacts, and the runtime reads the old one no longer. A WebAssembly
artifact pushed by an older `guestfn` runs again after a push by the new
one.

### The manifest

```yaml
contract: oci@0.1  # the engine and its contract's version; wasm@1.0 is the component world
name: tagger
version: 0.3.0
oci:
  image: ghcr.io/acme/tagger@sha256:…   # written by guestfn push
  args: ["--insecure"]
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
validate` holds `oci.image` to a digest-pinned reference and refuses it in a
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

Firecracker is the sandbox for now (decision 8).

**What boots.** The runtime unpacks the image's layers once per image
digest, applies the whiteouts and preserves ownership and modes without
root, into the form the sandbox boots from. For a VMM with block devices
only, that form is a squashfs image, which the runtime writes
straight from the layer tarballs with its blocks packed with zstd
(the packing of the root filesystem, after this paragraph); a VMM
that shares files through virtio-fs takes a directory instead. The image config, with the user
resolved against the image's `/etc/passwd` and `/etc/group`, goes
inside the squashfs, so a stored root filesystem describes itself. The
runtime keeps the unpacked files in memory until the squashfs exists, so it
bounds what an image unpacks to. The layers may unpack to eight times
`--module-size-limit` between them, the rule a WebAssembly module's tar
layer follows. They may contain a million entries. The runtime
stops reading a layer where the image passes either bound and refuses the
image. A whiteout or an opaque marker removes what the layers below left,
whatever its place among its own layer's entries. An entry under a symbolic
link to a directory goes where the link leads inside the root filesystem,
as a container runtime puts it. The runtime refuses a path with a `..`
segment, an entry under a file, and an entry at the path of its own config.
It leaves device nodes out, because the guest's `/dev` is the kernel's
`devtmpfs`. The
content-addressed store keeps the result beside the compiled WebAssembly
artifacts, under `engine::version()`, under the same LRU sweep and the
same per-process key. The key check refuses a planted root filesystem
before the VMM sees it. The guest kernel comes with the package, either
built by the project from a microVM configuration or supplied by the VMM,
and Renovate tracks it with the VMM. The first build takes the kernel
Firecracker's CI builds from Amazon Linux's microVM kernel (6.18, the
guest kernel Firecracker supports since 1.16.1, with squashfs, overlayfs,
cgroups, virtio and vsock built in), pinned by checksum with Firecracker's
release in `crates/function-oci/Dockerfile` and the Makefile. The artifacts
are at a dated prefix of Firecracker's bucket, whose retention the project doesn't
control. Building the kernel in the repository from that source and
config is what the record calls the project's own kernel build. Whether a
release may include the published kernel is open
([#281](https://github.com/jonasz-lasut/guestfn/issues/281)).
`guestfn_module_compile_duration_seconds` measures the unpacking.

**The packing of the root filesystem.** The squashfs blocks are zstd at
level 3.
The guest decompresses every block the process reads, at the start of
every run, so the packing is part of a run's latency. A CI runner measured
it on 2026-11-13, 30 runs per cell in one job, with the engine's squashfs
of each image written again with each of the other compressors:

| image | packing | squashfs | written in | a booted run, median | a restored run, median |
|---|---|---|---|---|---|
| `function-oci-go`, 37.4 MiB | gzip | 12.20 MiB | 1450 ms | 552 ms | 448 ms |
| | zstd 1 | 12.83 MiB | 137 ms | 434 ms | 333 ms |
| | zstd 3 | 12.22 MiB | 173 ms | 436 ms | 321 ms |
| | zstd 19 | 10.45 MiB | 13 263 ms | 444 ms | 329 ms |
| | stored | 37.39 MiB | | 402 ms | 292 ms |
| the fixture, 9.2 MiB | gzip | 3.35 MiB | 338 ms | 321 ms | 186 ms |
| | zstd 3 | 3.36 MiB | 42 ms | 294 ms | 170 ms |

Level 3 gives the size gzip gave. A run of the Go example reads it about
120 ms sooner, and the image's first load writes it in an eighth of the
time.
A higher level buys disk alone: level 19 is 14 % smaller, takes 77 times
as long to write and runs no faster. Level 1 saves 36 ms of a first load
for 5 % more disk. A squashfs that compresses nothing runs another 30 ms
faster at three times the disk. The guest kernel has to read zstd
(`CONFIG_SQUASHFS_ZSTD`), which the kernels Firecracker's CI builds do
on both architectures for 5.10, 6.1 and 6.18, the three lines the release
matrix tests. backhand's zstd is the C library, built into function-oci
alone: the dependency graph of function-wasm has none of it. The format is
part of `engine::version()`, so a runtime with another packing starts a
new directory of the compiled store and no VM snapshot taken over one
format meets a root filesystem written in the other.

**One VM per request.** The runtime starts the VM with the kernel, the
root filesystem read-only, a vsock device, memory sized to the step's
`limits.memory` plus the kernel's share, one vCPU (`--vm-vcpus`) and
the deadline. The init mounts the root filesystem with a tmpfs overlay,
both `nosuid` and `nodev` like the fresh `/tmp`,
brings up loopback and reads the run's description over vsock (env, user,
cwd, entrypoint and the proxy settings). It starts the process as the
image's user, never root, and streams its output to the host as it arrives.
The runtime refuses an image that runs as root at load, from the config
alone when the config doesn't set a user or sets the number 0, before it
unpacks a layer.
Before the exec the init empties the capability bounding set, clears the
ambient, permitted, effective and inheritable sets and sets `no_new_privs`.
The guest's only root is then the init. Dropping capabilities alone isn't
enough, because UID 0 can still write the cgroup file that caps the run's memory. Once the function's port accepts a
connection the init bridges it to a second vsock connection, and the host
calls `RunFunction` over that, bounded by the run's deadline and a limit on
the response's size. The runtime returns the response as soon as it has
it. A thread of its own ends the rest of the run. The init stops the
process and reports its exit, which flushes what it logged. The runtime
logs those lines and the VM's console. Then it kills and reaps the VM,
which discards every change the run made, and a fresh instance per
request holds. Until 2026-11-13 the caller waited for that work. A CI
runner measured its median at 24 ms of a booted run of 400 ms and at
19 ms of a restored run of 177 ms, 3 to 5 ms of it for the exit report
and the rest for the VMM's exit. When the function doesn't answer, the
run is over before its fatal result returns, because the result quotes
how the process ended and what it logged last. `--enable-vm-snapshots` restores a snapshot of the booted VM in place of
the boot ([Snapshots](#snapshots)).

**Bounds.** `limits.timeout` is the VM's wall clock, the boot included.
`limits.memory` is the process's memory. The init runs it in a cgroup
capped at the limit. The VM gets the limit plus a fixed share for the
kernel, the init and the overlay. The run slots, the fair scheduler by digest, the memory pool,
the step slots and the gRPC deadline cap apply unchanged, because they
wrap `Engine::run`. A run's slot and its memory belong to the VM. An
answered run keeps both after the response, until the runtime has reaped
the VMM. The next VM starts only once the one before it's gone.
`runs_in_flight` counts the run until then, and `run_duration` observes
it at the response, which is what a caller waits. The step slot is the
runtime's and ends with the call. While the runtime stops one VM of a
step at its `limits.concurrency`, the step may already run its next,
inside both bounds of the engine. The runtime drops its engine when it shuts down, and a dropped
engine waits until it has stopped every VM, which keeps a shutdown from
leaving a VMM running. The kernel kills a process past its cap. The
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
authority under `--enable-vm-snapshots` and the wire between the init and the
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
Firecracker has no macOS port, and a machine without KVM has no loop
(decision 9).

### Snapshots

`--enable-vm-snapshots` restores a snapshot of an image's booted VM in place of a
boot. It's a prototype, off by default and Firecracker's alone, and 0.1
includes it as an alpha feature (decision 13).
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

| command | `oci@0.1` |
|---|---|
| `guestfn build` | refuses an `oci@0.1` manifest: the command builds WebAssembly modules, and the author builds the image with `docker build` (or `buildx`) |
| `guestfn push --image <ref@sha256:…>` | appends the manifest to the image the author pushed, as one more layer, and writes the image's digest into the manifest's `oci.image`. It doesn't read a module file |
| `guestfn inspect` | says that a module image is one and which image the manifest records as its origin, with the manifest's summary. `--pull` refuses it |
| `guestfn scaffold composition` | unchanged |

An OCI guest is an ordinary function on the language's own SDK.
`examples/function-oci-go` is one in Go, with its `Dockerfile`.

### The Input

The `module` block, `compositionPolicy`, `limits` and `config` keep their
schema. Both packages serve the Input as `guestfn.crossplane.io/v1`, which
replaces `wasm.fn.crossplane.io/v1` outright: the WebAssembly runtime
refuses the old group with a sentence that states the new one, `function
validate` prints the same sentence, and the CRD under `package/wasm/input/`,
the scaffolds' compositions, the examples and the goldens move with it.
The `v1beta1` deprecation goes with the old group.

## Code shape

- `crates/function`: the runtime as a library, generic over the trait.
  `cli.rs` is the entry point the two binaries call. `manifest.rs` gains the
  `Contract` enum and `oci.image`. `oci.rs` reads the wrapping artifact, and
  `resolver.rs` fetches an image through it into a layout.
- `crates/engine`: the `Engine` trait and what the engines share. The writer for
  the image layout is here so `resolver.rs` can use it.
- `crates/engine-wasm`: the Wasmtime engine.
- `crates/engine-oci`: the `Sandbox` trait with Firecracker
  behind it (`firecracker.rs`: the machine config, the devices and the
  lifecycle), `rootfs.rs` unpacking the layers into the squashfs root
  filesystem, `layout.rs` reading the image layout the runtime hands over,
  `image.rs` holding the image config and the run description, `initrd.rs`
  packing the initramfs around the init, `run.rs` applying the bounds and
  wording the fatals, with the egress bridge onto `HttpRequester` to come.
- `crates/oci-init`, the guest init (`guestfn-init`, static for musl): it
  mounts, overlays, starts the process and relays the streams. The proxy
  with its authority comes with the egress phase. `crates/oci-fixture` is
  the function the VM tests run, and `crates/oci-protocol` is what the init
  and the host say to each other (`Description`, `Exit`, the frame kinds),
  framed over vsock.
- `crates/guestfn`: `push.rs` appends the manifest to an image under
  `--image`, and `inspect.rs` describes a module image. `buildcmd.rs`
  refuses an `oci@0.1` manifest.
- `package/oci/` beside `package/wasm/`: one package root per runtime, each
  with its `crossplane.yaml` and a copy of the Input CRD.
  `crates/function-oci/Dockerfile` adds the VMM and the kernel.
- `.github/workflows`: `build-oci` jobs in `e2e.yml`, an
  `oci` e2e profile on a KVM runner, and `publish-function.yml` publishing either
  package.

## Tests

- Unit: the layer unpacking against fixtures with whiteouts and
  ownership, read back from the form the sandbox boots. The init protocol
  framing. The manifest's `contract`, `oci.image` and per-engine rules. The
  wrapping artifact in `push.rs`.
- The engine's VM suite runs a static test image built in the test
  from a small binary that echoes a fixed response, unpacked and run under
  the sandbox on a KVM runner. The cases cover the deadline, the memory
  limit, a non-zero exit, stderr log lines and a proxied request against a
  fake requester.
- Conformance goldens: a new fixture family for the OCI refusals and the
  package mismatch sentence.
- The example: `examples/function-oci-go` tests its function natively,
  and the `build-oci-go` job runs those tests.
- End-to-end tests run an `oci` profile with the `function-oci` package on kind
  with `/dev/kvm` mounted, on a KVM-capable runner, exercising a committed test
  image through the same Guest XR.

## Documentation

An operators page for the OCI runtime (install, the device and the flags),
a guests page for the contract and for publishing an image as a
module, the manifest page's `contract` and `oci.image` fields, the artifact's
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
package starts alpha, on its own tags and releases (decision 15).

## Alternatives

- **gVisor as the sandbox.** A user-space kernel, with no KVM required on
  its systrap platform, slower system calls and a software-only boundary.
  A later sandbox behind the same `Sandbox` trait if a cluster can't offer
  KVM.
- **A libkrun prototype and a comparison.** A libkrun and a Firecracker
  prototype behind one `Sandbox` seam. Dropped from the path to 0.1
  (Jonasz, 2026-11-09), with Firecracker staying for now.
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
- **A process contract over the standard streams.** The first build ran the
  image's command once per request with the request on standard input and
  the response on standard output. An SDK image needed an adapter around its
  server. Replaced before any release (Jonasz, 2026-11-09,
  [#258](https://github.com/jonasz-lasut/guestfn/issues/258)).
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
  `contract: oci@0.1`. `guestfn build` would run `docker build` and then a
  contract check, which is one run of the image under the sandbox with an empty
  request and a verdict on whether stdout decoded as a response. The guest suite
  would run each scaffold through the whole host on a KVM runner. Dropped
  (Jonasz, 2026-11-09): scaffolding and building the image are the author's, and
  `guestfn push --image` wraps the result with the manifest layer.
- **A separate repository.** The policy, proxy and tooling code is most of
  the product and changes together. Rejected.

- **One engine crate with a Cargo feature per engine.** The first build's
  shape. With both features on `wasm` served, so the workspace's lint and
  tests never compiled the `function-oci` build (decision 1).
- **A container sandbox for machines without KVM.** A sandbox behind
  `--insecure` would run the image through the Docker daemon or a rootless
  `crun`, with the same init protocol over a Unix socket and a warning at
  startup and under every step `function validate` prints. It would give
  a machine without KVM a development loop, and 0.1 goes without it
  (decision 9).

## Decisions

The owner settled these on 2026-11-05:

1. **Packaging.** Separate packages from one workspace, each a binary crate
   over the generic runtime, with a crate per engine (Jonasz, 2026-11-09,
   after Cargo features that exclude each other). `function-wasm` keeps its
   size and runs without a device. `function-oci` needs `/dev/kvm`.
2. **The contract.** A gRPC server in the VM: the image is an ordinary
   Crossplane function, and the host calls it over vsock (Jonasz,
   2026-11-09, [#258](https://github.com/jonasz-lasut/guestfn/issues/258)).
   The manifest nests keys specific to a contract under its name:
   `oci.image` and `oci.args`.
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
    the component world, `oci@0.1` the contract of an image, and `type` goes,
    since the contract states the engine. From a contract's 1.0 a minor
    adds fields a module may use between versions and a major changes what
    the engine does with a module, and at 0.x a minor may change anything.
    The runtime refuses the old name as an unknown field.

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
    and the VM size as its key. Restores go through the API of the release
    binary. 0.1 includes snapshots as an alpha feature behind
    `--enable-vm-snapshots`.
    [Per-image microVM snapshots](one-pager-vm-snapshots.md) has the design and
    the image-independent snapshot it rejected.
14. **The egress design.** The proxy gets a design of its own first, in
    [#280](https://github.com/jonasz-lasut/guestfn/issues/280), before
    phase 5. Decision 3 remains its direction, and the design enforces the
    manifest's method and path rules. 0.1 doesn't wait for the proxy: it
    refuses a manifest that requires egress, by name.
15. **Tags and releases.** Each component has its own: git tags and GitHub
    releases named `function-wasm-vX.Y.Z`, `function-oci-vX.Y.Z`,
    `guestfn-vX.Y.Z` and `wit-vX.Y.Z`, and release branches named
    `release-<component>-X.Y`. The tag of an OCI artifact drops the prefix and
    keeps the version.
16. **Compatibility at 0.x.** Until function-oci's 1.0.0 a release may
    break anything the package has, and it announces a rename or a
    removal by refusing the old name by name. The wording of a fatal
    result is never a contract. The contract is `oci@0.1` while
    function-oci is 0.x: a minor version may break images built for the
    one before, and `oci@1.0` comes with function-oci's 1.0.0. Whether the
    Input, the manifest and the metric names are stable for function-oci
    waits for function-wasm's 1.0.0
    ([Compatibility](contract/compatibility.md#function-oci-at-0x)).

The owner decided this the same day:

9. **The development loop without KVM.** Firecracker has no macOS port,
   and a machine without KVM has no loop. 0.1 has no container fallback
   and runs on KVM only.

The owner decided this on 2026-11-12:

17. **An unprivileged process.** The runtime refuses an image that runs as
    root, and the init drops every capability and sets `no_new_privs`
    before it starts the process. The process then can't read or change what
    belongs to the init, which the egress proxy
    ([#280](https://github.com/jonasz-lasut/guestfn/issues/280)) depends
    on.

The owner decided these on 2026-11-13, with the measurements of
[#376](https://github.com/jonasz-lasut/guestfn/pull/376):

18. **zstd for the root filesystem.** The runtime packs the squashfs with
    zstd in place of gzip, and function-oci accepts the C library that
    comes with it. function-wasm doesn't get it.
19. **The response before the teardown.** A caller gets its response
    before the runtime has stopped the VM. The run's slot and its memory
    remain the VM's until the runtime has reaped the VMM.

## Phases

[#281](https://github.com/jonasz-lasut/guestfn/issues/281) has the
order from these phases to 0.1, the package's first release.

1. Done (#247). The `Engine` trait started in its own crate. On 2026-11-06 it
   moved back into `crates/engine` at the owner's ask, and each engine has had a
   crate of its own since 2026-11-09. `crates/function` is generic over it, with
   Wasmtime behind it and the behaviour and the goldens unchanged.
2. Done. #244 renamed the manifest file and the media type. #246
   moved the Input group and generated the CRD. #247 added the manifest's
   `type` with its per-type contract, with `type: oci` refused by name.
   The next PR made `abi` the `contract` enum and added the manifest's
   `oci.image` with `guestfn push --image`, which first published a wrapping
   artifact and now appends the manifest to the image as a layer (decision
   11), read back by `guestfn inspect`.
3. Done, on Firecracker, covering the OCI engine behind the
   `Sandbox` trait, the init and its protocol, the layer unpacking into
   squashfs, the VM lifecycle, the bounds, `function validate --resolve`
   for images, the `function-oci` binary with its
   flags, the image fetch of the resolver with the manifest read out of
   the image's annotated layer, and the env bindings and the credentials
   through the init.
4. Done, namely the `oci` e2e profiles on a GitHub runner with KVM, as the
   jobs of `e2e-oci.yml` beside the `wasm` ones (a build job and a cluster
   job per profile) and the engine's VM suite in `ci.yml`, with the published
   kernel. The kernel build, where the project builds its own, remains
   open.
5. The egress bridge (the TLS-decrypting proxy in the init) with audit and
   metrics parity, after its design in
   [#280](https://github.com/jonasz-lasut/guestfn/issues/280) (decision
   14). Until then `function-oci` refuses a manifest that requires egress,
   by name.
6. Done in part ([#263](https://github.com/jonasz-lasut/guestfn/issues/263)):
   `examples/function-oci-go`, the hello function as an image on the
   contract, published by `publish-examples.yml` through `docker
   buildx` and `guestfn push --image`, and the `oci` e2e case that pulls
   the published example from `ghcr.io`. The `function-oci` package's
   publish workflow (`Publish Function`, with `function-oci` chosen), its
   image (`crates/function-oci/Dockerfile`), its package root (`package/oci`) and its
   operators page are in the tree. Left: the package's first release,
   0.1.0.
7. A prototype ([#255](https://github.com/jonasz-lasut/guestfn/issues/255)):
   snapshots per image behind `--enable-vm-snapshots`
   ([Per-image microVM snapshots](one-pager-vm-snapshots.md)), in 0.1 as
   an alpha feature (decision 13).
