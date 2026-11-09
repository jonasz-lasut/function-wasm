# function-oci

function-oci is a [Crossplane](https://crossplane.io) composition function
that runs an OCI image in a [Firecracker](https://firecracker-microvm.github.io)
microVM, one VM per request. The image's process follows the process
contract: it reads a `RunFunctionRequest` from its standard input, writes a
`RunFunctionResponse` to its standard output and logs to its standard error.
You write the function in any language with a Crossplane function SDK, or
without one, build the image with your own `Dockerfile`, publish it with its
manifest and point a Composition step at it. It's one of the two functions
of the [guestfn](../../README.md) project, and shares the Input, the
manifest, the policy layers and the CLI with
[function-wasm](../wasm/README.md), whose modules are WebAssembly
components.

It fits where a step needs a toolchain or a library that has no WebAssembly
build: the process gets its own kernel, a writable root filesystem the VM
discards, and `/tmp`, with no network device.

> [!CAUTION]
> **Alpha.** function-oci is 0.x, and until 1.0.0 a release may break
> anything ([Compatibility](https://jonasz-lasut.github.io/guestfn/contract/compatibility.html#function-oci-at-0x)).
> The pod needs `/dev/kvm`, and the sandbox has had no independent security
> review.

```yaml
apiVersion: apiextensions.crossplane.io/v1
kind: Composition
metadata:
  name: example
spec:
  compositeTypeRef:
    apiVersion: example.crossplane.io/v1
    kind: XR
  mode: Pipeline
  pipeline:
  - step: greet
    functionRef:
      name: function-oci
    input:
      apiVersion: guestfn.crossplane.io/v1
      kind: Input
      module:
        type: OCI
        oci:
          ref: ghcr.io/example/greeter@sha256:4c9d…  # the image with its manifest layer, as guestfn push --image prints it
      config:
        from: spec.config    # the composite resource field holding the module's configuration
```

The runtime resolves the module as function-wasm does. The reference pins
a digest, or the composite resource chooses it through `module.from`. The
module's manifest requests what the process needs, both policy layers must
permit it, and the operator's policy can require a cosign signature. The
process gets the request with the configuration in place and without the
step credentials it wasn't granted. An OCI module comes from a registry
alone: `module.type` is `OCI`.

## Install

```yaml
apiVersion: pkg.crossplane.io/v1
kind: Function
metadata:
  name: function-oci
spec:
  package: ghcr.io/jonasz-lasut/function-oci:v<version>
```

The pod must open `/dev/kvm`, which a device plugin or a host path grants
it. The container's memory limit must cover one VM per concurrent run.
[Install function-oci](https://jonasz-lasut.github.io/guestfn/oci/install.html)
has the node requirements, the flags and the sizing.

## Build a module

```shell
docker build -t ghcr.io/example/greeter:v0.1.0 .
docker push ghcr.io/example/greeter:v0.1.0
guestfn push ghcr.io/example/greeter:v0.1.0-module --image ghcr.io/example/greeter@sha256:…   # appends guestfn.yaml as a layer; prints the module block
```

`guestfn push --image` appends the manifest to the image as one more layer
and records the image's digest in it; the result is an image Docker still
runs, which the Composition pins. The manifest declares `contract: oci@0.1`
and what the process needs, the way a WebAssembly module's does.
[`examples/function-oci-go`](../../examples/function-oci-go) is the hello
function built this way: a Go `main` around function-sdk-go, a
`Dockerfile` and the manifest. [OCI
modules](https://jonasz-lasut.github.io/guestfn/oci/modules.html) has the
process contract and the refusals.

## This directory

`crossplane.yaml` is the package's metadata and `input/` the Input's CRD,
held equal to function-wasm's. The package image is `Dockerfile.oci` at the
repository root (the runtime built with its `oci` engine, the init, the
pinned Firecracker and guest kernel), and `extensions/function-oci/` holds
the icon the marketplace shows beside this file.
