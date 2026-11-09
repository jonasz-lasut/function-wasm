# function-oci-go

The hello function as an OCI module: a [Crossplane](https://crossplane.io)
composition function in Go
([function-sdk-go](https://github.com/crossplane/function-sdk-go)), built
into an image that [function-oci](https://github.com/jonasz-lasut/guestfn),
the OCI runtime, boots in a microVM once per request. It composes a
ConfigMap that greets the composite resource, with the greeting from the
step's `config`.

`fn.go` is an ordinary function-sdk-go `RunFunction`, the same code a gRPC
Function or a WebAssembly module would run. `main.go` starts function-sdk-go's
server around it, as any function's `main` does, and the runtime calls it
over gRPC
([OCI modules](https://jonasz-lasut.github.io/guestfn/oci/modules.html)).
The manifest's `oci.args` pass `--insecure`, since the call stays inside
the VM.
The `Dockerfile` cross-compiles a static binary into an empty image with
a numeric user; the image has no shell, no libc and no `/tmp`, which the
runtime mounts fresh for every run.

## Run it

```shell
go test ./...        # the function and the contract, natively
make build           # the image, for this machine's platform
make push            # push the image, publish the module, print the step's module block
```

`make push` pushes the image to `REGISTRY` (`127.0.0.1:5000` by default,
the local `registry:3` of the
[development loop](https://jonasz-lasut.github.io/guestfn/oci/install.html#the-development-loop))
and runs `guestfn push --image` over it, which appends `guestfn.yaml` to
the image as one more layer and prints the digest-pinned reference for
`example/composition.yaml`. Rendering needs `function-oci` running on a
Linux machine with `/dev/kvm`; `example/` holds the XRD, the Composition, an XR
and the Function on the Development runtime.

## In a cluster

`Publish Examples` publishes this example as
`ghcr.io/jonasz-lasut/guestfn/examples/oci-go:v<version>` (the
version in `guestfn.yaml`), the image with its manifest layer, signed
keyless. The step is a `function-oci` step with the published reference:

```yaml
- step: greet
  functionRef:
    name: function-oci
  input:
    apiVersion: guestfn.crossplane.io/v1
    kind: Input
    module:
      type: OCI
      oci:
        ref: ghcr.io/jonasz-lasut/guestfn/examples/oci-go@sha256:<the digest guestfn inspect prints>
    config:
      from: spec.config        # the composite resource field holding {greeting: hi}
```

The digest is the one
`guestfn inspect ghcr.io/jonasz-lasut/guestfn/examples/oci-go:v<version>`
prints.

The manifest requires nothing: the process gets the request and nothing
else. The runtime reads the configuration from the composite resource,
holds it to the manifest's schema before the run and hands it to the
process as `input.config`.

## Layout

- `fn.go`: `RunFunction` and the `config` reader; `fn_test.go` covers the
  default and the configured greeting and a config that doesn't decode.
- `main.go`: the function's server.
- `Dockerfile`: the image. `guestfn.yaml`: the manifest `guestfn push
  --image` appends, with the config schema.
