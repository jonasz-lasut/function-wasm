# OCI modules

This page is for authors publishing an OCI image as a module for
`function-oci`, the OCI runtime ([design record](../one-pager-oci-runtime.md),
[the operators' page](../operators/oci-runtime.md)). It covers the process
contract the image's command follows, the manifest and the artifact
`guestfn push` publishes, and how the runtime runs the image. A
WebAssembly module follows [Write a module](write-a-module.md) instead.

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
type: oci
contract: 1
name: tagger
version: 0.3.0
requires:
  credentials: [cmdb]
config:
  schema:
    type: object
```

`type: oci` selects the process contract, whose version is `contract: 1`.
`image` pins the image by digest; `guestfn push --image` writes it, so the
file may leave it out. The other fields keep their meaning from
[Module manifests](module-manifests.md). The runtime refuses
`requires.egress` for now: the VM has no network, and the proxy is a later
phase of the design.

## Build and publish

Build the image with your own `Dockerfile` and push it:

```sh
docker build -t ghcr.io/acme/tagger:v0.3.0 .
docker push ghcr.io/acme/tagger:v0.3.0
# the digest: docker inspect --format '{{index .RepoDigests 0}}' ghcr.io/acme/tagger:v0.3.0
```

Then publish the module, the artifact that wraps the image:

```sh
guestfn push ghcr.io/acme/tagger-module:v0.3.0 --image ghcr.io/acme/tagger@sha256:…
```

`guestfn push` reads `guestfn.yaml`, sets its `image`, and publishes the
manifest as the one layer of an artifact under the type
`application/vnd.guestfn.module.v1`. It prints the `module:` block for
the Composition, pinned to the artifact's digest. Sign the artifact with
cosign as you sign a WebAssembly module; the signature covers the manifest,
and the manifest pins the image.

`guestfn inspect <ref>` describes the artifact and prints the image it
pins. `function validate --resolve` pulls the image and prints the
contract's verdict, as it does for a component. A plain image, with no
wrapping artifact, runs too, in the default sandbox, as a bare
`application/wasm` layer does.

Images must use gzip or uncompressed layers (every builder's default) and
target the node's platform. A multi-platform index resolves to the node's.
The image's `User` resolves against the image's `/etc/passwd` and
`/etc/group`, as in a container runtime. A name missing from them is a
refusal.

## Refusals

A refusal states the module. The common ones:

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
