# function-oci-go

The hello function as an OCI module: a [Crossplane](https://crossplane.io)
composition function in Go
([function-sdk-go](https://github.com/crossplane/function-sdk-go)), built
into an image that [function-oci](https://github.com/jonasz-lasut/guestfn),
the OCI runtime, boots in a microVM once per request. It composes a
ConfigMap that greets the composite resource, with the greeting from the
step's `config`.

`fn.go` is an ordinary function-sdk-go `RunFunction`, the same code a gRPC
Function or a WebAssembly module would run. `main.go` is the process
contract around it
([OCI modules](https://jonasz-lasut.github.io/guestfn/oci/modules.html)):
the `RunFunctionRequest` bytes on standard input, the `RunFunctionResponse`
bytes on standard output, the logger's JSON lines on standard error, and
a non-zero status with one plain line on standard error for a request it
can't read, decode or answer, which the runtime quotes in its fatal result.
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
Linux machine with `/dev/kvm`; `example/` holds the Composition, an XR and
the Function on the Development runtime.

## In a cluster

`Publish Examples` publishes this example as
`ghcr.io/jonasz-lasut/wasmfn/examples/function-oci-go:v<version>` (the
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
        ref: ghcr.io/jonasz-lasut/wasmfn/examples/function-oci-go@sha256:<the digest guestfn push printed>
    config:
      greeting: hi
```

The manifest requires nothing: the process gets the request and nothing
else. The runtime holds `config` to the manifest's schema before the run.

## Layout

- `fn.go`: `RunFunction` and the `config` reader; `fn_test.go` covers the
  default and the configured greeting and a config that doesn't decode.
- `main.go`: the process contract; `main_test.go` runs it over a request
  and over bytes that aren't one.
- `Dockerfile`: the image. `guestfn.yaml`: the manifest `guestfn push
  --image` appends, with the config schema.
