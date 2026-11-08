# OCI modules

This page is for authors publishing an OCI image as a module for
`function-oci`, the OCI runtime ([design record](../one-pager-oci-runtime.md),
[the install page](install.md)). It covers the process
contract the image's command follows, the manifest and the artifact
`guestfn push` publishes, and how the runtime runs the image. A
WebAssembly module follows [Write a module](../wasm/write-a-module.md) instead.

## The process contract

The runtime runs the image's configured entrypoint and command once per
request, as the image's configured user, in its configured working
directory, with its configured environment. The contract is four streams:

| stream | content |
|---|---|
| standard input | the raw `RunFunctionRequest` bytes, then end of file |
| standard output | the raw `RunFunctionResponse` bytes. The runtime returns them unchanged, and appends `meta` when the response leaves it out |
| standard error | log lines. A line that parses as a JSON object with `level` and `msg` becomes a log line of that level with its other keys as fields. Any other line logs at info. The runtime attaches the module and its digest |
| exit status | zero with a response on standard output is a run. Anything else is a fatal result with the status and the last line of standard error |

A twenty-line `main` around the language's function SDK meets it. It
decodes the request from standard input and writes the function's encoded
response to standard output. The process gets the image's own arguments
and nothing more. The runtime sends it no signal before the deadline; at
the deadline it stops the VM.

The process sees `/tmp` writable and empty, and the rest of its root
filesystem writable through an overlay the VM discards on exit. It sees
its own kernel, no network interface, the host's clock and randomness, and
the environment variables its manifest's `requires.env` bindings bound.
The step credentials its manifest's `requires.credentials` names travel in
the request bytes, as for a component.

## The manifest

```yaml
contract: oci@1.0
name: tagger
version: 0.3.0
requires:
  credentials: [cmdb]
config:
  schema:
    type: object
```

`contract: oci@1.0` names the process contract and its version.
`image` pins the image by digest; `guestfn push --image` writes it, so the
file may leave it out. The other fields keep their meaning from
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
`image` field. It appends a gzip tar layer holding the manifest at
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
layer. `function
validate --resolve` pulls the image and prints the contract's verdict, as
it does for a component. A plain image, with no manifest layer, runs too,
in the default sandbox, as a bare `application/wasm` layer does.

Images must use gzip or uncompressed layers (every builder's default) and
target the node's platform. A multi-platform index resolves to the node's.
The image's `User` resolves against the image's `/etc/passwd` and
`/etc/group`, as in a container runtime. A name missing from them is a
refusal.

## Refusals

A refusal states the module. The common ones:

- A `Path` or `HTTP` source: `cannot resolve module: module.type Path is not
  served by function-oci: an OCI module is an image pulled from a registry,
  so module.type must be OCI`. Push the image to a registry; the development
  loop uses a local one ([Install function-oci](install.md)).
- `image has neither an entrypoint nor a command`: set `ENTRYPOINT` or
  `CMD` in the `Dockerfile`.
- `image is built for linux/arm64, and this runtime runs on linux/amd64`:
  build for the node's platform, or push a multi-platform index.
- `process exited with status 1: <line>`: the process failed; the line is
  the last one it wrote to standard error.
- `process exited with status 0 and wrote no response to its standard
  output`: the process must write the response bytes before it exits.
- `run failed: module exceeded its execution deadline (30s)`: the step's
  `limits.timeout`, or the runtime's `--module-timeout`, bounds the run,
  boot included.
