# OCI modules

This page is for authors publishing an OCI image as a module for
`function-oci`, the OCI runtime ([design record](../one-pager-oci-runtime.md),
[the install page](install.md)). It covers the contract the image's
function follows, the manifest and the artifact
`guestfn push` publishes, and how the runtime runs the image. A
WebAssembly module follows [Write a module](../wasm/write-a-module.md) instead.

## The contract

An image is an ordinary Crossplane function: its command starts a
`FunctionRunnerService` gRPC server on port 9443, and a function built with a
Crossplane SDK doesn't need an adapter. The runtime starts the image's entrypoint
and command once per request, as the image's configured user, in its
configured working directory, with its configured environment, and calls
`RunFunction` once the port accepts a connection.

| part | content |
|---|---|
| the request | the bytes the runtime received, unchanged |
| the response | returned unchanged. The runtime appends `meta` when the response leaves it out |
| a status other than OK | a fatal result with the status's message |
| standard output and standard error | log lines. A line that parses as a JSON object with `level` and `msg` becomes a log line of that level with its other keys as fields. Any other line logs at info. The runtime attaches the module and its digest |
| the process ending before it answers | a fatal result with the exit status and the last log line |

The runtime keeps the last 256 KiB of what the process writes during a run.
When the process writes more, the runtime logs one warning with the number
of bytes it dropped from the start, then the lines it kept. The runtime
shortens a line longer than 16 KiB to that length, in the log and in a fatal
result, and marks it `[truncated: N more bytes]`.

The server listens without TLS, since the call stays inside the VM. A
function on a Crossplane SDK demands mTLS otherwise. Its manifest sets
`oci.args` to `["--insecure"]`, a flag Crossplane's design for functions
guarantees. The runtime resolves the step's `config.from` before the run, as
for a component: `input.config` in the request is the configuration object
from the composite resource. The call has the run's deadline. The
runtime sends the process no signal before it, and at the deadline it stops
the VM.

[`examples/function-oci-go`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-oci-go)
is function-sdk-go's server around the hello function, with a manifest
like the one below. Its `Dockerfile` puts a static binary
into an empty image under a numeric user. Its `Makefile` has a `push`
target that runs the two commands of [Build and publish](#build-and-publish)
against a local registry.

The process runs as the image's user, which can't be root, with no Linux
capability in any set and `no_new_privs` set, so a `setuid` binary or a file
capability in the image grants it nothing. It sees `/tmp` writable and
empty, and the rest of its root filesystem through an overlay the VM
discards on exit, writable where its user may write. It sees
its own kernel, a loopback interface and no other, the host's clock and randomness, and
the environment variables its manifest's `requires.env` bindings bound.
The step credentials its manifest's `requires.credentials` names travel in
the request bytes, as for a component.

The runtime unpacks the image's layers as a container runtime does:
whiteouts remove what the layers below them left, and an entry under a
symbolic link to a directory goes where the link leads inside the image.
It leaves out the image's device nodes, since `/dev` in the VM is the
kernel's own. It keeps `/.guestfn/config.json` for the image config it
resolved, so an image can't have a file there.

## The manifest

```yaml
contract: oci@0.1
name: tagger
version: 0.3.0
oci:
  args: ["--insecure"]
requires:
  credentials: [cmdb]
config:
  schema:
    type: object
```

`contract: oci@0.1` names the contract and its version, and
[Compatibility](../contract/compatibility.md#function-oci-at-0x) says what
the version means.
`oci.args` are the arguments for the function's binary: they replace the
image's command and keep its entrypoint, as a Kubernetes container's `args`
do. `oci.image` pins the image by digest; `guestfn push --image` writes it,
so the file may leave it out. The other fields keep their meaning from
[Module manifests](../guests/module-manifests.md). The runtime refuses
`requires.egress` for now: the VM has no network, and the proxy is a later
phase of the design.

## Build and publish

Build the image with your own `Dockerfile` and push it:

```sh
docker build -t ghcr.io/acme/tagger:v0.3.0 .
docker push ghcr.io/acme/tagger:v0.3.0
# the digest: docker inspect --format '{{index .RepoDigests 0}}' ghcr.io/acme/tagger:v0.3.0
```

Then publish the module, the image with the manifest appended as one more
layer, into the image's repository:

```sh
guestfn push ghcr.io/acme/tagger:v0.3.0-module --image ghcr.io/acme/tagger@sha256:…
```

`guestfn push` reads `guestfn.yaml` and records the image's digest in its
`oci.image` field. It appends a gzip tar layer holding the manifest at
`/.guestfn/manifest.json` to the image: the layer's `diff_id` goes into
the image config and the annotation `io.crossplane.guestfn: manifest` onto
its descriptor, the way Crossplane's `xpkg append` adds a package's
extensions. The result is an image Docker still runs, and the command
prints the `module:` block for the Composition, pinned to the new digest.
Sign that digest with cosign as you sign a WebAssembly module: one
signature covers the image and the manifest. An OCI module comes from a
registry alone: the step's `module.type` is `OCI`, and `function-oci`
refuses a `Path` or `HTTP` source by name.

`guestfn inspect <ref>` says that a module image is one and which image
the manifest records as its origin, and prints the manifest out of the
layer. For a multi-platform image it describes each platform of the index,
and `guestfn manifest show <ref>` prints the manifest the platforms share. `function
validate --resolve` pulls the image and prints the contract's verdict, as
it does for a component. From the image, `--resolve` needs the device:
`docker run --device /dev/kvm --group-add <gid> …`. A plain `validate`
doesn't. A plain image, with no manifest layer, runs too,
in the default sandbox, as a bare `application/wasm` layer does.

Images must use gzip or uncompressed layers (every builder's default) and
target the node's platform. A multi-platform index resolves to the node's.
The image's `User` resolves against the image's `/etc/passwd` and
`/etc/group`, as in a container runtime. A name missing from them is a
refusal. The runtime also refuses an image that runs as root: one that
doesn't set `User`, or whose user is UID 0, by number or through its
`/etc/passwd`. Set a numeric `USER` in the `Dockerfile`, as the example
does.

## Refusals

A refusal states the module. The common ones:

- A `Path` or `HTTP` source: `cannot resolve module: module.type Path is not
  served by function-oci: an OCI module is an image pulled from a registry,
  so module.type must be OCI`. Push the image to a registry; the development
  loop uses a local one ([Install function-oci](install.md)).
- `image has neither an entrypoint nor a command`: set `ENTRYPOINT` or
  `CMD` in the `Dockerfile`.
- `image runs as root`: set a non-root `USER` in the `Dockerfile`.
- `image is built for linux/arm64, and this runtime runs on linux/amd64`:
  build for the node's platform, or push a multi-platform index.
- `image unpacks to more than 1073741824 bytes, eight times the size limit
  (--module-size-limit)`: the layers unpack to more than eight times the
  runtime's `--module-size-limit` between them. Build a smaller image, or
  have the operator raise the flag.
- `image holds more than 1000000 entries, the most the OCI runtime
  unpacks`: the layers contain more than a million files, directories and
  links between them. Build a smaller image.
- `layer entry .guestfn/config.json is at /.guestfn/config.json, which the
  OCI runtime keeps for the image config it resolved`: remove the file from
  the image.
- `layer entry <path> has a .. segment, which could name a path outside the
  image`, or `layer entry <path> is under <parent>, which is not a
  directory`: a layer doesn't unpack into a root filesystem. Rebuild the
  image with a builder that writes ordinary layers.
- `process exited with status 1 before answering the call: <line>`: the
  process ended before it answered. The line is the last one it logged. An
  SDK server without `--insecure` ends this way: check the manifest's
  `oci.args`.
- `run returned an error: <message> (gRPC status <code>)`: the function
  answered the call with an error.
- `run failed: module exceeded its execution deadline (30s)`: the step's
  `limits.timeout` or the runtime's `--module-timeout` bounds the run,
  boot included.
- `run failed: the guest did not connect before the deadline: the VM booted
  but its init never reached the host`: the process never started; the
  runtime's `--debug` log has the VM's console, which says where the boot
  stopped. Not the module's doing.

[Install function-oci](install.md#refusals) lists the rest.
