# function-wasm

[![CI](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml/badge.svg)](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml)

A [Crossplane](https://crossplane.io) composition function that runs a
WebAssembly module in a [wasmtime](https://wasmtime.dev) sandbox. The module
implements the same contract as a native composition function —
`RunFunction(RunFunctionRequest) → RunFunctionResponse` — so you write an
ordinary [function-sdk-go](https://github.com/crossplane/function-sdk-go)
function, compile it to WebAssembly, publish the module, and point a
Composition step at it. One installed `function-wasm` serves any number of
modules; changing composition logic never means building, publishing and
installing another Function package.

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
      name: function-wasm
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module:
        oci:
          ref: ghcr.io/example/greeter@sha256:4c9d…
      config:
        greeting: hi
```

The whole `RunFunctionRequest` reaches the module and its whole
`RunFunctionResponse` goes back to Crossplane, so desired resources, results,
conditions, requirements (extra resources), context and TTL all work exactly
as they do for a native function.

## Install

```yaml
apiVersion: pkg.crossplane.io/v1
kind: Function
metadata:
  name: function-wasm
spec:
  package: ghcr.io/jonasz-lasut/function-wasm:v0.1.0
```

Also mirrored to `xpkg.upbound.io/jonasz-lasut/function-wasm`.

## Write a module

Install the CLI and scaffold a project:

```shell
go install github.com/jonasz-lasut/function-wasm/cmd/guestfn@latest

guestfn init greeter --module github.com/example/greeter   # --lang go (default), tinygo or rust
cd greeter
```

You get `fn.go` — a plain function-sdk-go `RunFunction` that composes a
ConfigMap greeting the composite resource — `fn_test.go`, and a three-line
`main.go`:

```go
func init() { wasmfn.Register(&Function{log: wasmfn.NewLogger()}) }

func main() {}
```

`wasmfn` (`github.com/jonasz-lasut/function-wasm/pkg/wasmfn`) is the guest SDK: it
exports the entry points the runtime calls, decodes the request, calls your
`RunFunction`, encodes the response, and gives you a `logging.Logger` that
logs through the runtime. Your function knows nothing about WebAssembly, and
`wasmfn.GetConfig(req, &cfg)` hands you the Input's `config` block.

```shell
go test ./...                                   # unit tests run natively
guestfn build                                   # fn.wasm (wasip1)
guestfn push ghcr.io/example/greeter:v0.1.0     # OCI artifact; prints the digest to pin
```

`guestfn push` produces a CNCF wasm OCI artifact (one `application/wasm`
layer); `oras push ghcr.io/example/greeter:v0.1.0 fn.wasm:application/wasm`
gives the same result, and a `FROM scratch` image that `COPY`s the module
works too. Any language with a wasip1 toolchain can target the
[ABI](docs/abi.md).

**Module size.** A guest using function-sdk-go's `request`, `response` and
`resource` packages is about 75 MB (13 MB compressed) — those packages bring
crossplane-runtime and Kubernetes apimachinery, as they do into a native
function binary. `wasmfn` itself adds nothing beyond the protobuf types, so
a guest that works on the raw `RunFunctionRequest`/`RunFunctionResponse`
(`fnv1` and `structpb`) is about 20 MB. Either way the runtime compiles a
module once per digest and caches it.

[`examples/hello-go`](examples/hello-go) is exactly what `guestfn init` produces.

### Other languages

The [ABI](docs/abi.md) is two exports and protobuf bytes, so any wasip1
toolchain works, and `guestfn` scaffolds and builds three flavours — the
same greeting function each time:

| `guestfn init --lang` | example | toolchain | how it talks protobuf | module size |
|---|---|---|---|---|
| `go` (default) | [`examples/hello-go`](examples/hello-go) | Go + `wasmfn` + function-sdk-go | `request`/`response`/`resource` helpers | ~75 MB (13 MB compressed) |
| `tinygo` | [`examples/hello-tinygo`](examples/hello-tinygo) | [TinyGo](https://tinygo.org) | protobuf-go message types + [vtprotobuf](https://github.com/planetscale/vtprotobuf)'s reflection-free codecs, generated from the vendored proto (shipped pre-generated; `go generate` + protoc to redo) | ~1.3 MB |
| `rust` | [`examples/hello-rust`](examples/hello-rust) | Rust, `wasm32-wasip1` (`cargo`, `protoc`) | [prost](https://github.com/tokio-rs/prost) over the vendored proto | ~150 KB |

`guestfn build` picks the toolchain from the project (`Cargo.toml` → cargo;
a `go.mod` requiring vtprotobuf but not `wasmfn` → tinygo; otherwise go) or
takes `--lang`. The TinyGo and Rust flavours carry their ~40 lines of ABI glue
in the open; each example has a `make render-check` that runs it through the
runtime, and the root tests run all three through the host as well.

### Render locally

Build the module, then run the runtime from a checkout serving your project
directory, and render with the Development runtime the scaffold's
`example/functions.yaml` declares:

```shell
guestfn build
go run github.com/jonasz-lasut/function-wasm/cmd/function@latest --insecure --debug --module-dir=.
crossplane render example/xr.yaml example/composition.yaml example/functions.yaml
```

The example Composition uses `module.path: fn.wasm`; swap in the `oci`
reference for a cluster. In this repository `make -C examples/hello-go render`
does all of the above for the example guest (`render-check` asserts the
output; CI runs it).

## Input reference

`apiVersion: wasm.fn.crossplane.io/v1beta1`, `kind: Input`.

| field | type | description |
|---|---|---|
| `module` | object | where the module comes from — exactly one of `oci`, `http`, `path`, `ociFrom`, `httpFrom`, `pathFrom` |
| `module.oci.ref` | string | OCI artifact reference `registry/repo[:tag][@sha256:…]`; the module is the `application/wasm` (or `vnd.wasm` content) layer, the only layer, or the first `.wasm` file of a single tar layer. Prefer a digest reference; a tag is re-resolved every `--module-tag-ttl` |
| `module.oci.credentials` | string | name of a pipeline-step credential (a Secret with `.dockerconfigjson`, or `username` and `password` keys) used to pull. Without it the runtime's Docker config (`DOCKER_CONFIG`) and anonymous access are tried |
| `module.http.url` | string | download the module over HTTP(S); `module.digest` is required |
| `module.path` | string | a file relative to the runtime's `--module-dir`; refused unless that flag is set — local rendering and volume-mounted modules |
| `module.ociFrom` | string | a field of the observed composite resource, under `spec.` or `status.`, holding an OCI source object `{ref, credentials}` — e.g. `status.module`; read on every request, so each XR can choose its module |
| `module.httpFrom` | string | likewise, holding an HTTP source object `{url}`; `module.digest` is still required |
| `module.pathFrom` | string | likewise, holding a module path (a string) relative to `--module-dir` |
| `module.digest` | string | `sha256:<hex>` the fetched module must match; required for `http`/`httpFrom`, checked against the source's digest otherwise |
| `config` | object | opaque, passed to the module untouched inside the request input; a Go guest reads it with `wasmfn.GetConfig` |

Letting each composite resource choose its module — the Composition names
the XR field, the field holds the source:

```yaml
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module:
        ociFrom: spec.module        # spec.module: {ref: ghcr.io/example/greeter@sha256:…}
```

Credentials for a step are declared on the pipeline step:

```yaml
- step: greet
  functionRef:
    name: function-wasm
  credentials:
  - name: registry
    source: Secret
    secretRef:
      namespace: crossplane-system
      name: ghcr-pull
  input:
    apiVersion: wasm.fn.crossplane.io/v1beta1
    kind: Input
    module:
      oci:
        ref: ghcr.io/example/private-fn@sha256:…
        credentials: registry
```

## How a request runs

1. The Input is decoded. A `*From` source is read from the observed
   composite resource first (`ociFrom: status.module` expects
   `status.module` to be `{ref, credentials}`; a typo or a wrong shape is a
   fatal result naming the field). The source is then resolved to a
   **content digest** (a digest reference is its own answer; a tag costs one
   registry `HEAD` per `--module-tag-ttl`; a served file is hashed when it
   changes).
2. On the first request for a digest the module is fetched, verified against
   the digest and compiled — about two seconds for a 75 MB Go module. Compiled
   modules stay in memory (`--module-cache-size`); with `--cache-dir` the
   fetched bytes and wasmtime's compiled code are also kept on disk, so
   restarts and registry outages need no network.
3. Every request gets a fresh instance (about ten milliseconds): WASI with no
   filesystem, environment or network access; guest logs flow into the
   runtime's logger with the module reference attached; stdout and stderr are
   the pod's, so a Go panic's stack shows up in `kubectl logs`.
4. The response is returned as the module produced it. A trap, timeout
   (`--module-timeout`, or the request deadline if shorter), memory limit
   (`--module-memory-limit`) or an unusable module is a fatal result naming
   the module — never a crashed function pod.

The full host/guest contract is in [docs/abi.md](docs/abi.md).

## Runtime flags

| flag | env | default | purpose |
|---|---|---|---|
| `--module-dir` | `MODULE_DIR` | unset | serve `path` sources from this directory |
| `--cache-dir` | `CACHE_DIR` | unset | on-disk caches: `<dir>/modules` for fetched modules, `<dir>/wasmtime` for compiled code; back it with a volume through a `DeploymentRuntimeConfig` |
| `--module-cache-size` | | `8` | compiled modules kept in memory |
| `--module-tag-ttl` | | `5m` | how long an OCI tag's resolution is reused |
| `--max-module-size` | | `128` MB | largest module accepted |
| `--module-timeout` | | `30s` | wall-clock budget of one run |
| `--module-memory-limit` | | `512` MB | linear memory a run may use |
| `--cosign-key` | `COSIGN_KEY` | unset | PEM file of cosign public key(s); when set only OCI modules with a matching `cosign sign --key` signature run, and `http`/`path` sources are refused |
| `--ttl` | | `60s` | TTL of responses the runtime itself produces (fatal results); a module sets its own |

The usual function-sdk-go flags (`--insecure`, `--debug`, `--tls-server-certs-dir`,
`--address`, `--max-recv-message-size`) apply too.

## Metrics

The runtime serves Prometheus metrics where function-sdk-go puts them
(`:8080/metrics`), next to the gRPC server metrics:

| metric | labels | meaning |
|---|---|---|
| `function_wasm_module_compile_duration_seconds` | | histogram of wasmtime compile time (compiled-cache misses) |
| `function_wasm_module_fetch_duration_seconds` | `source` = oci, http, path | histogram of fetch + verify time (blob-cache hits included) |
| `function_wasm_module_run_duration_seconds` | `outcome` = ok, error, timeout | histogram of one guest run, instantiate to response |
| `function_wasm_module_cache_events_total` | `cache` = compiled, blob; `event` = hit, miss | cache lookups |

No metric carries a module identity: the set of digests a Function serves is
unbounded. Logs carry the module reference and digest.

## Trust model

A module runs with the privileges of the Composition that references it: it
sees the request's observed and desired state, context and any step
credentials, exactly as a native function would. With a `*From` source the
**composite resource's author** picks the module — use it where XR authors
are trusted to, restrict what they can pick with `--cosign-key`, and note
that a credentials name read from the XR can only select among the
credentials the Composition step already declares. The sandbox protects the
runtime process — and with it every other Composition sharing the Function —
from a crashing, looping or memory-hungry module, and gives a module no
filesystem, environment or network. Pin `oci` references to digests; `http`
sources require one.

To restrict a Function to modules your organisation signed, sign them with
`cosign sign --key cosign.key <ref>` and start the runtime with
`--cosign-key cosign.pub` (a `DeploymentRuntimeConfig` mounts the key and
sets the flag or `COSIGN_KEY`). Every OCI module is then verified once per
manifest digest before it is fetched, and unsigned sources are refused.
Keyless (Fulcio/Rekor) signatures are not verified.

## Development

```shell
go build ./... && go vet ./... && go test -race ./...    # host, CLI, engine (needs a C compiler: wasmtime-go is CGo)
(cd pkg/wasmfn && go test ./... && GOOS=wasip1 GOARCH=wasm go vet ./...)
(cd examples/hello-go && go test ./...)
golangci-lint run ./...
go generate ./...                                        # Input CRD, guestfn scaffold golden
make -C examples/hello-go render-check                      # crossplane render through the real runtime
```

The root tests build `examples/hello-go` to WebAssembly and run it through the
host; `go test -short` skips that. See [AGENTS.md](AGENTS.md) for the layout
and conventions.
