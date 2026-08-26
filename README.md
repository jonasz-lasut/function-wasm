# function-wasm

[![CI](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml/badge.svg)](https://github.com/jonasz-lasut/function-wasm/actions/workflows/ci.yml)

> [!CAUTION]
> **Highly experimental.** function-wasm is a pre-1.0 project exploring
> WebAssembly as a composition function runtime. The Input schema, the guest
> ABI and the runtime flags can still change between minor releases without
> a deprecation period, the sandbox has not had an independent security
> review, and nothing here has run in production yet. Try it, break it and
> [open an issue](https://github.com/jonasz-lasut/function-wasm/issues), but
> do not build a platform on it.

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
        type: OCI
        oci:
          ref: ghcr.io/example/greeter:v1@sha256:4c9d…  # printed by guestfn push; the digest pins the module, the tag is for humans
      config:
        greeting: hi
```

The whole `RunFunctionRequest` reaches the module and its whole
`RunFunctionResponse` goes back to Crossplane, so desired resources, results,
conditions, requirements (extra resources), context and TTL all work exactly
as they do for a native function.

## Use cases

function-wasm is the right tool when the *logic* of a pipeline step should be
replaceable without rebuilding, publishing and installing a Function package,
or when the person who owns that logic is not the person who owns the
Composition.

- **A customization hook at the end of a pipeline.** The platform team ships
  the Composition and reserves its last step for a module the consuming team
  provides — chosen per Composition, or per composite resource with
  `module.from: spec.hooks.module`. The team adjusts labels, annotations, sizing
  or values, adds a sidecar resource, or reshapes the desired state to its
  conventions, in the language it prefers, without a change request against
  the Composition. The platform team keeps the guardrails: digest pinning,
  a `compositionPolicy` (Cedar) fencing which registries the team may pick
  from, `--cosign-key` so only modules signed with the organisation's key
  run, and the resource caps of the sandbox (`limits` per step, the
  runtime's flags as ceilings).
- **Audit and compliance trails.** A module that runs last sees the complete
  observed and desired state. It can log a structured record of every
  reconcile — who requested what, which resources are about to change and how
  — through the runtime's logger (`wasmfn.NewLogger`, straight into the pod
  log and whatever ships it), emit results that become events on the XR, or
  compose an audit resource next to the workload. Nothing else in the
  pipeline changes.
- **Policy gates.** Validate the desired composed resources against
  organisational rules — naming, mandatory tags, allowed regions, cost
  guardrails — and return a fatal result with a precise message to stop the
  reconcile, or a warning to let it through with a trace. Per-tenant policy
  is a per-tenant module; the pipeline stays one.
- **Per-tenant, per-environment or per-region behaviour.** One Composition,
  and a module picked from the XR (`module.from`, a field under `spec.` or
  `status.` holding the source) or from a per-environment Composition — the
  same step running different logic for dev and prod, or for `eu` and `us`.
- **Fast iteration and safe rollback.** Write the function, `guestfn build`,
  render locally against the runtime, `guestfn push`, reference the digest.
  Rolling forward or back is a digest change in the Composition; nothing is
  reinstalled and every other Composition on the same runtime is untouched.
- **Many small functions, one runtime.** Teams keep their own modules,
  possibly in their own languages (Go, TinyGo, Rust — see the
  [size table](#other-languages)); the cluster runs one function-wasm, which
  compiles each module once per digest and caches it.
- **Restricted networks.** Modules can come from an internal registry, an
  internal HTTP server (`http`) or a volume mounted into the runtime
  (`path`), and the on-disk caches keep fetched modules and compiled code
  across restarts, so a registry outage does not stop reconciling.

What it is not for: anything that needs to reach out of the sandbox at run
time beyond what its manifest declares and the policy layers permit. By
default a module gets no network, filesystem or environment; cluster state
comes in through the request (observed state, required resources), and the
module returns desired state. The sandbox opens selectively, per capability
([docs/one-pager-three-layer-authz.md](docs/one-pager-three-layer-authz.md),
[docs/one-pager-sandbox.md](docs/one-pager-sandbox.md)): a module declares
what it cannot run without in its manifest (`requires`) - a private `/tmp`
for the request, environment variables bound to step credentials (host
directories are deliberately not mountable), and **HTTP egress through the
host** to call the APIs it lists, with the host resolving, filtering,
budgeting and auditing every request - see [HTTP egress](#http-egress).
Each capability is granted only when the Input's `compositionPolicy` and
the operator's Cedar `--sandbox-policy-file` both permit it.

## Install

```yaml
apiVersion: pkg.crossplane.io/v1
kind: Function
metadata:
  name: function-wasm
spec:
  package: ghcr.io/jonasz-lasut/function-wasm:v0.2.0
```

Also mirrored to `xpkg.upbound.io/jonasz-lasut/function-wasm`.

## Write a module

Install the CLI and scaffold a project:

```shell
cargo install --git https://github.com/jonasz-lasut/function-wasm guestfn

guestfn init greeter --module github.com/example/greeter   # --lang go (default), tinygo, rust, zig or c
cd greeter
```

You get `fn.go` — a plain function-sdk-go `RunFunction` that composes a
ConfigMap greeting the composite resource — `fn_test.go`, and a three-line
`main.go`:

```go
func init() { wasmfn.Register(&Function{log: wasmfn.NewLogger()}) }

func main() {}
```

The scaffold also vendors a small `internal/wasmfn` package the project owns
(there is no external SDK to depend on): it exports the entry points the
runtime calls, decodes the request, calls your `RunFunction`, encodes the
response, and gives you a `logging.Logger` that logs through the runtime. Your
function knows nothing about WebAssembly, and `wasmfn.GetConfig(req, &cfg)`
hands you the Input's `config` block. Edit it if you need to; it is yours, like
the ABI glue the TinyGo and Rust scaffolds carry.

```shell
go test ./...                                   # unit tests run natively
guestfn build                                   # fn.wasm (wasip1); prints the ABI verdict and the manifest summary
guestfn inspect fn.wasm                         # size, ABI verdict, exports, imports, memory
guestfn push ghcr.io/example/greeter:v0.1.0     # OCI artifact with the manifest; prints the module block and what the module requires
```

`guestfn build` ends with the verdict the runtime reaches when it loads the
module — `Built fn.wasm (73.9 MB, ABI v1, imports wasmfn.http wasmfn.log)`
— and fails, in the runtime's words, on a module the runtime would refuse
(`module does not export "wasmfn_run"`); `guestfn push` refuses to publish
such a module for the same reason. The check is the runtime's own: `guestfn`
compiles the module with the same wasmtime engine (a couple of seconds for a
large Go module), so what it prints is what a load says.
`guestfn inspect fn.wasm` shows what the runtime sees — size, verdict,
exports, imports with their types, memory limits — and `guestfn inspect
ghcr.io/example/greeter:v0.1.0` describes an artifact from its manifest
(media types, layer, annotations) without pulling, `--pull` reading the
module too; `--output json` for scripts.

The scaffold also has a **`wasmfn.yaml`** — the module's manifest: what it
declares about itself (`name`, `version`, `abi: 1`), the sandbox
capabilities it cannot run without (`requires`: egress rules,
`filesystem.privateTmp`, `env` credential bindings - the scaffold requires
nothing; non-secret configuration belongs in `config`, not env) and the
JSON Schema of its `config` (the scaffold's covers `greeting` and
`greetingUrl`). `guestfn build` validates it and checks the example
Composition's `config` against the schema; `guestfn push` publishes it
beside the module (`--manifest` names another file, `--module-version` and
`--revision` override the version and set the revision annotation) and
prints, under the `module:` block, the `requires:` block so a Composition
author knows what the policy layers must permit; the runtime then refuses
a module whose requirements the Input's `compositionPolicy` or the
operator's `--sandbox-policy-file` does not permit, before the module runs
(see [Module manifests](#module-manifests)). `guestfn manifest validate`
checks the file, `guestfn manifest show ghcr.io/example/greeter:v0.1.0`
prints what a published module declares, and `guestfn scaffold composition
--from ghcr.io/example/greeter:v0.1.0` (or `--from fn.wasm`) writes a
Composition step — `module` pinned, a `config` skeleton from the schema, and
a commented `compositionPolicy` skeleton derived from the manifest's
`requires` (the `grantEgress`/`usePrivateTmp`/`setEnv` permits it would need,
and a `pullModule` permit for its repository for a `module.from` source);
`--full` for a whole Composition. The `compositionPolicy` block is commented:
it is a starting point for narrowing, never an active grant.

`guestfn push` produces a CNCF wasm OCI artifact: one `application/wasm`
layer, the manifest as an `application/vnd.wasmfn.manifest.v1+json` layer
when the project has one, a wasm config naming both in `layerDigests`, and
the standard `org.opencontainers.image.*` annotations from the manifest.
`oras push ghcr.io/example/greeter:v0.1.0 fn.wasm:application/wasm
wasmfn.json:application/vnd.wasmfn.manifest.v1+json` gives the same result,
and a `FROM scratch` image whose only layer `COPY`s the module to `/fn.wasm`
— that exact path, nothing is guessed from the archive — works too (without
a manifest). Any language with a wasip1 toolchain can target the
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
toolchain works, and `guestfn` scaffolds and builds five flavours — the
same greeting function each time:

| `guestfn init --lang` | example | toolchain | how it talks protobuf | module size |
|---|---|---|---|---|
| `go` (default) | [`examples/hello-go`](examples/hello-go) | Go + function-sdk-go (vendored `internal/wasmfn` glue) | `request`/`response`/`resource` helpers | ~75 MB (13 MB compressed) |
| `tinygo` | [`examples/hello-tinygo`](examples/hello-tinygo) | [TinyGo](https://tinygo.org) | protobuf-go message types + [vtprotobuf](https://github.com/planetscale/vtprotobuf)'s reflection-free codecs, generated from the vendored proto (shipped pre-generated; `go generate` + protoc to redo) | ~1.8 MB |
| `rust` | [`examples/hello-rust`](examples/hello-rust) | Rust, `wasm32-wasip1` (`cargo`, `protoc`) | [prost](https://github.com/tokio-rs/prost) over the vendored proto | ~250 KB |
| `zig` | [`examples/hello-zig`](examples/hello-zig) | [Zig](https://ziglang.org) 0.16 (a single binary; `protoc` only to regenerate) | [zig-protobuf](https://github.com/Arwalk/zig-protobuf) over the vendored proto, generated codec checked in | ~95 KB |
| `c` | [`examples/hello-c`](examples/hello-c) | C via `zig cc` (the same zig binary, no wasi-sdk; `nanopb_generator` only to regenerate) | [nanopb](https://jpa.kapsi.fi/nanopb/) over the vendored proto (heap-allocated fields, generated codec checked in), [cJSON](https://github.com/DaveGamble/cJSON) for the host payloads | ~70 KB |

An **AssemblyScript** flavour exists as an example only for now
([`examples/hello-assemblyscript`](examples/hello-assemblyscript), ~30 KB — the
smallest guest; `npm ci && make build`): it passes the same behaviour tests as
the scaffolded flavours, but `guestfn init` cannot scaffold it yet.

`guestfn build` picks the toolchain from the project (`Cargo.toml` → cargo;
a `build.zig` → zig, for the zig and c guests alike; a `go.mod` requiring
vtprotobuf → tinygo; otherwise go) or takes `--lang`. Every flavour carries
its ABI glue in the open — the Go scaffold vendors it under `internal/wasmfn`,
TinyGo, Rust, Zig and C carry theirs beside the module — with a small HTTP
helper over `wasmfn.http`; each example
has a `make render-check` that runs it through the runtime, and the root tests
run them all through the host as well — with and without an egress grant.

### Render locally

Build the module, then run the runtime from a checkout serving your project
directory, and render with the Development runtime the scaffold's
`example/functions.yaml` declares:

```shell
guestfn build
cargo run --release -p function-wasm -- --insecure --debug --module-dir=.   # from a checkout
crossplane render example/xr.yaml example/composition.yaml example/functions.yaml
```

The example Composition uses `module.type: Path` with `path: fn.wasm`; swap
in `type: OCI` and the `oci` reference for a cluster. In this repository `make -C examples/hello-go render`
does all of the above for the example guest (`render-check` asserts the
output; CI runs it).

### Validate a Composition

Crossplane never installs a function's Input CRD, so the runtime is the only
gate a Composition's Input passes — and until now it was reached only by
reconciling. `function validate` runs that gate offline: the runtime binary
takes the same ceiling flags as when it serves and applies the same checks
(the Input's `compositionPolicy` compiled, `limits` against
`--module-timeout`/`--module-memory-limit`, the `module` source's shape)
to every function-wasm step of the Compositions (or bare `Input`
documents) you give it, printing the runtime's own words:

```shell
cargo run --release -p function-wasm -- validate \
  example/composition.yaml --module-dir=. --resolve
# or, with the released image (its entrypoint is the runtime):
docker run --rm -v "$PWD:/w" ghcr.io/jonasz-lasut/function-wasm:<version> validate /w/composition.yaml \
  --sandbox-policy-file /w/policy.cedar
```

```
composition.yaml: Composition/hello pipeline[0] hello: OK (oci ghcr.io/example/greeter:v1@sha256:3f2a…, limits timeout 5s memory 128Mi, compositionPolicy)
composition.yaml: Composition/hello pipeline[1] labeler: refused: module oci ghcr.io/example/labeler@sha256:9d1c… requires egress GET to host "evil.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit
```

`--xr xr.yaml` materialises `module.from` sources against that composite
resource, as the observed XR would (without it a `from` source is checked
for the `compositionPolicy` it requires and reported as the XR's
choice); `--resolve` goes on to resolve, verify (`--cosign-key`) and fetch
each module — OCI pulls use the local Docker config, never a step
credential — and compiles it with wasmtime for the runtime's own verdict
(size, ABI, host imports; a compile is seconds and about a gigabyte for a
large Go module); `--function-name` keeps only the steps of one function; `--output json` prints one JSON object per step for
CI annotations; `-` reads stdin. Warnings (a `Path` source in a
Composition, egress granted without `--cosign-key`, a limit equal to its
ceiling, a field the runtime would silently ignore) are printed under the
step and never change the exit code: 0 when every step is admitted, 1 when
at least one is refused, 2 when the tool itself failed (unreadable file,
unparsable YAML, a bad flag). `make -C examples/hello-go render` runs it
over the example first.

## Input reference

`apiVersion: wasm.fn.crossplane.io/v1beta1`, `kind: Input`.

```yaml
module:                        # required
  type: OCI                    # required: OCI | HTTP | Path — the Composition's choice
  oci:  {ref, credentials}     # exactly one of the object matching type …
  http: {url, digest}
  path: fn.wasm
  from: status.module          # … or the observed XR field holding it
compositionPolicy: |           # optional; the composition author's own Cedar layer
  permit (principal, action == Action::"pullModule",
          resource in Repository::"ghcr.io/example-org");
limits:                        # optional; each at most the runtime's ceiling
  timeout: 5s
  memory: 128Mi
config: {...}                  # optional; opaque, forwarded to the module
```

Everything but the source is read from the Input: `compositionPolicy` and
`limits` are the Composition's, never the composite resource's. There is no
`sandbox` block: a module declares the capabilities it cannot run without
in its manifest (`requires` - see [Module manifests](#module-manifests)),
and each is granted only when the Input's `compositionPolicy` and the
operator's Cedar `--sandbox-policy-file` both permit it
([docs/one-pager-three-layer-authz.md](docs/one-pager-three-layer-authz.md)).

| field | type | description |
|---|---|---|
| `module` | object | **required** — where the module comes from |
| `module.type` | string | **required** — `OCI`, `HTTP` or `Path`. Exactly one of the object it names (`oci`, `http`, `path`) or `module.from` is set, and no object of another type may be present. The runtime checks it on every request — the Input's CRD (with the same rules as CEL) is never installed by Crossplane; a function input is part of a Composition, not an object, so its schema only serves tooling that validates against it (`crossplane resource validate` with the package's `input/` directory) and IDEs |
| `module.oci.ref` | string | OCI artifact reference **pinned to its manifest digest**, `registry/repo@sha256:…`, as `guestfn push` prints it. The manifest digest pins the module (the manifest states its layer's digest; both are verified on fetch) and addresses the caches. A tag alone is not accepted (tags can be moved; the runtime resolves nothing at request time); `registry/repo:tag@sha256:…` is fine — the digest is what is fetched, the tag is human-readable context and may even no longer exist. The module is the `application/wasm` (or `vnd.wasm` content) layer, or the only layer; a tar layer (a `FROM scratch` image) must hold it at exactly `/fn.wasm` |
| `module.oci.credentials` | string | name of a pipeline-step credential (a Secret with `.dockerconfigjson`, or `username` and `password` keys) used to pull. Without it the runtime's Docker config (`DOCKER_CONFIG`) and anonymous access are tried. An object read through `module.from` may name one only where the `compositionPolicy` permits `spendCredential` for it on the ref's repository |
| `module.http.url` | string | download the module over HTTP(S) |
| `module.http.digest` | string | **required** — `sha256:<hex>` of the module; the download is verified against it |
| `module.http.manifestURL` | string | *optional* — a `wasmfn.yaml` served beside the module, its request layer (the three-layer model's manifest for a source that carries no OCI layer). Set with `module.http.manifestDigest`; without it an HTTP source carries no manifest and gets only the default sandbox. For a `module.from` http source it is fenced by `compositionPolicy` `pullModule` like the module URL |
| `module.http.manifestDigest` | string | `sha256:<hex>` of the manifest, verified against it; **required with** `module.http.manifestURL`, refused without it |
| `module.path` | string | a file relative to the runtime's `--module-dir`; refused unless that flag is set — local rendering and volume-mounted modules; carries no digest |
| `module.manifestPath` | string | *optional*, `type: Path` only — a `wasmfn.yaml` under `--module-dir`, the request layer for a Path module, so a local or volume-mounted module can declare the capabilities it needs. Read from the Input only (never through `module.from`) and re-read each request, so a local edit takes effect without a restart |
| `module.from` | string | a field of the observed composite resource, under `spec.` or `status.`, holding the source `module.type` names — an object `{ref, credentials}` for `OCI`, `{url, digest}` for `HTTP`, a string for `Path` — e.g. `status.module`; read on every request and decoded strictly (a typo or a wrong shape is a fatal result naming the field), so each XR can choose its module. What it may choose is fenced by `compositionPolicy` (`pullModule`, default-deny) |
| `compositionPolicy` | string | the composition author's own Cedar policy layer, over the same schema as the operator's `--sandbox-policy-file` (actions `pullModule`, `spendCredential`, `grantEgress`, `usePrivateTmp`, `setEnv`; a `Request` principal carrying `namespace` and `xrKind`; `Repository`, `HostPattern`, `Capability` and `Credential` entities). AND-combined with the module's manifest and the operator's policy, so it can only narrow. Two regimes: a sandbox action it scopes no rule for is not narrowed (the operator and the manifest decide alone), while a module chosen through `module.from` is refused unless a `pullModule` permit matches its normalized location - matched over a boundary-correct `Repository` hierarchy, so `Repository::"ghcr.io/example-org"` admits `ghcr.io/example-org/mod` but never the sibling namespace `ghcr.io/example-org-other/...` - and may spend a step credential only where a `spendCredential` permit matches (`context.repository` carries the ref's location). **Required whenever `module.from` names an `OCI` or `HTTP` source** — an unfenced XR author could point the runtime at any host and read what its answer says. Read from the Input only; malformed Cedar is a fatal result at admission |
| `limits.timeout` | duration | compute budget of one run, e.g. `5s`; at most `--module-timeout`, else a fatal result naming both (`limits.timeout 1m0s exceeds the runtime's --module-timeout of 30s`). Time the run spends waiting on `wasmfn.http` answers is credited back, so a slow upstream does not spend the budget; the request's own gRPC deadline is the hard wall-clock cap and still applies if shorter |
| `limits.memory` | quantity | linear memory a run may use, e.g. `128Mi`; at most `--module-memory-limit`, else a fatal result naming both (`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`) |
| `limits.concurrency` | int32 | at most N runs of this step at once, across all requests, keyed by the module's content digest. A further request waits under its own context; when the deadline passes first, it is a fatal result that consumed nothing and is not counted as a run. A value above `--max-concurrent-runs` is silently capped. No ceiling flag: this only narrows |
| `config` | object | opaque, passed to the module untouched inside the request input; a Go guest reads it with `wasmfn.GetConfig`. Non-secret module configuration belongs here - the module's environment comes only from its manifest's `requires.env` credential bindings |

What a module gets of the sandbox is not an Input field: its manifest's
`requires` (egress rules, `filesystem.privateTmp`, `env` credential
bindings) is the request, and each requested capability is granted only
when the `compositionPolicy` and the operator's `--sandbox-policy-file`
both permit it - see [Module manifests](#module-manifests) and
[HTTP egress](#http-egress).

Letting each composite resource choose its module — the Composition names
the type and the XR field, the field holds the source, the
`compositionPolicy` says what it may hold:

```yaml
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module:
        type: OCI
        from: spec.module           # spec.module: {ref: ghcr.io/example-org/greeter@sha256:…}
      compositionPolicy: |
        permit (principal, action == Action::"pullModule",
                resource in Repository::"ghcr.io/example-org");
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
      type: OCI
      oci:
        ref: ghcr.io/example/private-fn@sha256:…
        credentials: registry
```

An XR-chosen module may spend that credential only if the
`compositionPolicy` permits it (`spendCredential`), and only for a
repository a `pullModule` permit admits (the pull check runs first, and
`context.repository` carries the ref's location):

```yaml
    module:
      type: OCI
      from: status.module           # status.module: {ref: ghcr.io/example-org/…@sha256:…, credentials: registry}
    compositionPolicy: |
      permit (principal, action == Action::"pullModule",
              resource in Repository::"ghcr.io/example-org");
      permit (principal, action == Action::"spendCredential", resource == Credential::"registry")
      when { context.repository in Repository::"ghcr.io/example-org" };
```

A step may ask for less than the runtime allows, never more:

```yaml
    limits:
      timeout: 5s        # ≤ --module-timeout
      memory: 128Mi      # ≤ --module-memory-limit
```

Opening the sandbox: the module's manifest asks, the Input's
`compositionPolicy` and the operator's Cedar `--sandbox-policy-file` must
both permit, and the module gets exactly its request. A module that
scratches in `/tmp` and reads `$DATABASE_URL` declares, in its
`wasmfn.yaml`:

```yaml
requires:
  filesystem: {privateTmp: true}
  env:
  - name: DATABASE_URL
    fromCredential:
      name: db                               # step credential "db", key "url"
      key: url
```

The private `/tmp` is granted where both Cedar layers permit
`usePrivateTmp`; an env binding needs `setEnv` and `spendCredential` in
both. A requirement either layer does not permit (or any requirement on a
runtime with no `--sandbox-policy-file`) is a fatal result; the module
never runs. Non-secret configuration (`LOG_LEVEL: debug`) is not env - put
it in `config`, which the guest reads with `wasmfn.GetConfig`. The pull
credential (`module.oci.credentials`) is refused as a binding source: the
module must never see the secret that fetched it. Host directories are
never mountable into a module, whatever the policy.

### Module manifests

A module published with `guestfn push` from a project that has a
`wasmfn.yaml` carries a **manifest** beside it in the same OCI artifact (a
second layer, `application/vnd.wasmfn.manifest.v1+json`, covered by the
manifest digest the Composition pins and by a cosign signature): the
sandbox capabilities it cannot run without (`requires`: egress rules,
`filesystem.privateTmp`, `env` credential bindings - non-secret
configuration is the Input's `config`), a JSON Schema for its `config`,
its ABI and the oldest runtime that serves it. The runtime reads it once
per digest (into `/tmp/function-wasm-cache/manifests`) and, after
admission and load, decides each requirement by the three-layer rule —
the manifest requests, the `compositionPolicy` and the operator's
`--sandbox-policy-file` permit - **narrowing only**: a manifest can make a
run fail earlier and say why, it can never make a run possible or widen a
grant. A requirement a layer does not permit is a fatal result before the
module runs — `module oci ghcr.io/example/greeter@sha256:… requires egress GET to host "api.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit`,
`… requires a private /tmp (requires.filesystem.privateTmp), which the compositionPolicy does not permit for this request`,
`… requires runtime v0.3.0 or newer, this is v0.2.1` — and so is a
`config` outside the schema: `… config does not match the module's schema:
/greeting: got number, want string`. A module without a manifest gets the
default sandbox (nothing but the request); a `path` or `http` source has no
OCI manifest layer but may name its `wasmfn.yaml` by reference
(`module.manifestPath`, `module.http.manifestURL`/`manifestDigest`) to carry
one too. `guestfn push` prints the `requires:` block under the `module:`
block, `guestfn inspect <ref>` shows what a module requires, and `function
validate --resolve` applies the same check offline.

## HTTP egress

A module can be granted HTTP(S) requests **through the host**: it never
opens a socket (wasip1 has none), it asks the runtime, and the runtime
resolves the name, refuses addresses on its block list, terminates TLS with
its own roots, checks the host, method and path against the rules its
manifest declared and the policy layers granted, follows redirects within
them, enforces the operator's budgets, counts and logs every request, and
hands the response back. Three parties, in the order they decide:

1. **The operator** turns the capability on. Egress is enabled, and its host
   allowlist and SSRF CIDR rules are authored, in the Cedar `--sandbox-policy-file`
   (see [operator grant policy](#operator-grant-policy)); the per-run budgets are
   fixed defaults, and the one tunable budget - the rate limit - is a pair of
   flags:

   ```shell
   function --sandbox-policy-file /etc/function-wasm/policy.cedar \
     --egress-rate-limit-per-minute 60 --egress-rate-limit-burst 10
   ```

   With no `--sandbox-policy-file`, egress is not grantable at all: a
   module that requires it is a fatal result before it runs. A
   `grantEgress` permit that matches any host opens egress to any public host
   within the fixed budgets (timeout 10s, maxRequests 16, maxResponseBytes 4 MiB,
   maxRedirects 5; response headers are capped separately at 64 KiB) and the
   default block list. The **host allowlist** is the Cedar `grantEgress` action -
   which callers (`principal.namespace`, `principal.xrKind`) may grant which
   hosts, methods and paths, default-deny - and the **CIDR block/allow list** is
   the `Action::"dialAddress"` action over the `context.ip` extension:

   ```cedar
   // Only team-a may grant egress, and only to *.googleapis.com over GET/HEAD.
   permit (principal, action == Action::"grantEgress", resource)
   when { principal.namespace == "team-a" &&
          resource in HostPattern::"googleapis.com" &&
          ["GET", "HEAD"].contains(context.method) };

   // Block an internal range, then open one service inside it.
   forbid (principal, action == Action::"dialAddress", resource)
   when { context.ip.isInRange(ip("10.0.0.0/8")) };
   permit (principal, action == Action::"dialAddress", resource)
   when { context.ip.isInRange(ip("10.96.0.0/12")) };
   ```

   Each `dialAddress` condition is one ip test - `context.ip.isInRange(ip("CIDR"))`,
   `context.ip.isLoopback()`, or a `||` of those - and compiles **at load** into
   an ordered prefix list, so the dial path stays a few `Prefix.Contains` and
   **Cedar never runs per resolved IP**. A malformed rule is refused at startup,
   so `function validate` reports it too.

   The default block list - loopback, link-local (the cloud metadata endpoint),
   RFC 1918, carrier-grade NAT (`100.64.0.0/10`, a common pod range), IPv6
   unique-local, the NAT64 and IPv4-compatible prefixes, and the unspecified,
   multicast and reserved ranges - applies to **every address a name resolves
   to** (a zoned IPv6 literal such as `[::1%25lo]` is never dialled), and the
   host dials the address it checked, so a name cannot rebind between the check
   and the connection. A `dialAddress` `forbid` adds to it, a `permit` punches a
   hole in it, and a `forbid` wins; to reach a loopback service in a local test
   both `127.0.0.0/8` and `::1/128` need a permit, since every resolved address
   is judged. `HTTP_PROXY` is not honoured: the host must see the destination
   address to judge it. What the guest is told about a refusal is only that the
   policy refused; the resolved address and the block-list entry stay in the
   runtime's audit line.

2. **The module** declares what it needs in its manifest, and the
   Composition may narrow it. The manifest's `requires.egress.http` rules -
   exactly one of `host` (exact name) and `hostPattern` (`*.example.com`:
   every name under it, not the apex), at least one of
   `GET HEAD POST PUT PATCH DELETE OPTIONS`, an optional `pathPrefix` the
   (normalized) path must start with - are the module's ask:

   ```yaml
   # wasmfn.yaml
   requires:
     egress:
       http:
       - host: api.example.com
         methods: [GET]
         pathPrefix: /v1/prices/
       - hostPattern: "*.googleapis.com"
         methods: [GET, POST]
   ```

   The Input's `compositionPolicy` narrows only when it scopes
   `grantEgress`: then a rule no permit matches is refused. A rule the
   operator's Cedar policy does not permit is a fatal result before the
   module runs (`module oci … requires egress GET to host
   "evil.example.com" (requires.egress.http[0]), which the operator policy
   (--sandbox-policy-file) does not permit`), and so is any egress
   requirement on a runtime with no `--sandbox-policy-file` at all (`…
   requires egress (requires.egress.http), but the runtime has no
   --sandbox-policy-file, which is required to grant egress (grantEgress)`).
   An XR author who picks a module through `module.from` picks its manifest
   with it, but both Cedar layers still gate every rule it declares.

3. **The module** makes requests. In Go, `wasmfn.HTTPClient()` is an
   `*http.Client` whose transport is the host, so anything that takes a
   client — cloud SDKs, generated API clients — works unchanged:

   ```go
   func init() { wasmfn.Register(&Function{log: wasmfn.NewLogger(), http: wasmfn.HTTPClient()}) }

   func (f *Function) RunFunction(ctx context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
       r, err := f.http.Get("https://api.example.com/v1/prices/eu")   // refused → *wasmfn.HTTPError with the host's reason
       …
   }
   ```

   Inject the client into your function so native tests can substitute an
   `httptest` server; outside a wasip1 build the transport fails with
   `wasmfn.ErrNoHostHTTP`. The TinyGo and Rust scaffolds ship the same in
   about a hundred lines each — `HTTPGet`/`HTTPDo` in `http.go`, `http::get`/
   `http::send` in `src/http.rs` — over the `wasmfn.http` import (its JSON
   payload is in [docs/abi.md](docs/abi.md#http-egress)), with a swappable
   host so native tests can fake it.
   A request the host does not perform — no grant, host or method or path
   outside it, a blocked address, a budget, a transport failure — is a
   transport error naming the reason, never a trap; a status from the
   server, whatever it is, is a response. Calling `HTTPClient()` adds about
   3 MB to a raw-proto guest; a function-sdk-go guest already links what it
   needs.

Every request costs one line in the runtime's log with the module reference
and digest — method, host, path (never the query, headers or body), status
or the reason it was refused (plus, for a blocked address, the resolved
address and the block-list entry the guest is not told), response bytes,
duration, outcome — one more line per redirect hop, and one increment of
`function_wasm_module_http_requests_total{outcome}` (`ok`, `refused`,
`budget`, `error`; no host label). A guest that keeps calling without a
grant or past `maxRequests` gets one info line, then debug lines. A request
never outlives its run: it is cut short at the run's deadline
(`limits.timeout` or `--module-timeout`) if that comes before the policy's
`timeout` — and the run then ends as a timeout, so the guest does not get to
handle that error. With
egress on, a module can send whatever its request carries — step
credentials included — to any host it is granted: grant narrowly, prefer
`pathPrefix`, and pair the capability with `--cosign-key` so only modules
your organisation signed run.

## How a request runs

1. The Input is decoded, and what the Composition asks of the runtime is
   settled: the `compositionPolicy` is compiled (content-hash cached;
   malformed Cedar is a fatal result), `limits` are checked against the
   runtime's ceilings. A `module.from` source is then read from the
   observed composite resource (`type: OCI` with `from: status.module`
   expects `status.module` to be `{ref, credentials}`; a typo or a wrong
   shape is a fatal result naming the field) and fenced by the
   `compositionPolicy` (`pullModule` for its repository, `spendCredential`
   for a named credential - default-deny for XR-chosen sources). Resolving
   does no I/O: the **digest** that pins the module comes from the Input —
   the manifest digest of an OCI reference, `http.digest` for a URL — or
   from hashing a served file when it changes.
2. The digest is looked up in the caches — compiled modules in memory (kept
   ten minutes after their last use), then wasmtime artifacts on disk, then
   fetched modules on disk under `/tmp/function-wasm-cache`. Only a module
   never seen by this node is fetched (verified against its digest, written
   to disk) and compiled — about two seconds for a 75 MB Go module; a
   module without the ABI's exports, or importing what the host does not
   provide, is refused here — and its artifact written to disk. Restarts and
   registry outages need no network. Details in
   [docs/one-pager-cache.md](docs/one-pager-cache.md). The module's
   [manifest](#module-manifests), if its artifact carries one, is then the
   module's ask: each `requires` capability must be permitted by the
   Input's `compositionPolicy` and the operator's `--sandbox-policy-file`
   (AND-combined - a manifest can only make a run fail earlier), and a
   `config` outside the module's schema is a fatal result before anything
   runs.
3. Every request gets a fresh instance (about ten milliseconds): WASI with no
   network access, and no filesystem or environment beyond what the three
   layers granted - a private `/tmp` created for this request and removed
   after it, exactly the environment variables its manifest binds to step
   credentials; HTTP requests, if granted, go through the host
   ([HTTP egress](#http-egress)); guest logs flow into the runtime's logger
   with the module reference attached; stdout and stderr are the pod's, so a
   Go panic's stack shows up in `kubectl logs`.
4. The response is returned as the module produced it. A trap, timeout
   (`limits.timeout` or `--module-timeout` - guest compute, with time spent
   waiting on `wasmfn.http` credited back - or the request deadline if
   sooner), memory limit (`limits.memory` or `--module-memory-limit`) or an
   unusable module is a fatal result naming the module — never a crashed
   function pod. So is a request that, with `--max-concurrent-runs` set,
   reaches its deadline while waiting for a run slot (`waiting for a run
   slot: context deadline exceeded`): it never ran.

The full host/guest contract is in [docs/abi.md](docs/abi.md).

## Runtime flags

The binary has two subcommands: `serve` - the default, so `function
--insecure --module-dir=.` and a `DeploymentRuntimeConfig`'s `args` need no
subcommand - and [`validate`](#validate-a-composition), which takes the
ceiling flags below with the same defaults and environment variables, so a
Composition is validated against exactly what a runtime started with those
flags would admit.

| flag | env | default | purpose |
|---|---|---|---|
| `--module-dir` | `MODULE_DIR` | unset | serve `path` sources from this directory |
| `--max-module-size` | | `128` MB | largest module accepted |
| `--module-timeout` | | `30s` | wall-clock budget of one run; the ceiling for `limits.timeout` |
| `--module-memory-limit` | | `512` MB | linear memory a run may use; the ceiling for `limits.memory` |
| `--module-stack-limit` | `MODULE_STACK_LIMIT` | `512` KB | call stack a run may use (wasmtime's own default); past it the run fails with `trap: call stack exhausted`. Engine-wide - no Input field narrows it |
| `--enable-memory-cache` | `ENABLE_MEMORY_CACHE` | `true` | keep compiled modules in memory between requests. With `--enable-memory-cache=false` (or `--no-enable-memory-cache`) each request maps the module's compiled artifact from disk (6–8 ms for a large Go module) and releases it afterwards |
| `--max-cached-modules` | `MAX_CACHED_MODULES` | `0` (unbounded) | most compiled modules resident at once; the least recently used is dropped beyond it (freed once its last run ends). Artifacts are mapped from disk, so a resident Go module costs ~90 MB of file-backed memory |
| `--max-concurrent-compiles` | `MAX_CONCURRENT_COMPILES` | `1` | modules compiled at once. One compile already uses every core (~25 CPU-seconds and ~1 GB for a large Go module); further first requests wait their turn instead of multiplying that |
| `--max-cache-size` | `MAX_CACHE_SIZE` | `0` (unbounded) | MB the two on-disk caches may hold together; past it the least recently used entries (fetched modules and artifacts alike, ~230 MB per Go module version) are removed, at startup and every ten minutes. Size the volume, or set this below its size |
| `--cosign-key` | `COSIGN_KEY` | unset | PEM file of cosign public key(s); on its own, all-or-nothing — only OCI modules with a matching `cosign sign --key` signature run and `http`/`path` sources are refused. With a `--sandbox-policy-file`, it supplies the keys while the policy's `requireSignature` rules decide which repositories must be signed (a repository no rule names runs unsigned) |
| `--max-concurrent-runs` | `MAX_CONCURRENT_RUNS` | `0` (unbounded) | module runs executing at once; a further request waits for a slot under its own deadline and, if that passes first, is a fatal result (`waiting for a run slot: context deadline exceeded`) without having run. Unbounded, concurrency is the caller's — Crossplane's reconcile workers |
| `--max-total-run-memory` | `MAX_TOTAL_RUN_MEMORY` | `0` (unbounded) | total linear-memory budget in MB across all running modules; a run reserves its module's initial linear memory from the pool before it starts (waiting under its deadline when the pool is full) and each growth beyond it as its guest actually grows - so the pool holds what runs use, not their worst-case ceilings. A growth the pool cannot serve before the run's deadline is denied: the guest sees `memory.grow` fail, counted in `function_wasm_module_memory_denials_total` |
| `--warm-modules` | `WARM_MODULES` | unset | modules loaded before the health service reports Serving — resolved, verified (`--cosign-key` applies), then compiled or mapped through the same caches a request uses: OCI references pinned to their manifest digest (`repo[:tag]@sha256:…`, pulled with the runtime's Docker config) and, with `--module-dir`, `path:<file>` entries. Repeatable or comma-separated. An entry that fails to load is logged with the reason and does not stop the pod from serving; that module is loaded on its first request as usual |
| `--egress-rate-limit-per-minute` | `EGRESS_RATE_LIMIT_PER_MINUTE` | `0` (off) | Sustained egress requests per minute per module digest (a process-wide token bucket). The one tunable egress budget; the rest are fixed (timeout 10s, maxRequests 16, maxResponseBytes 4 MiB, maxRedirects 5). Enablement and the host allowlist and CIDR rules live in `--sandbox-policy-file` |
| `--egress-rate-limit-burst` | `EGRESS_RATE_LIMIT_BURST` | `0` (derived) | Burst tokens for `--egress-rate-limit-per-minute`; `0` derives `max(1, requestsPerMinute)` |
| `--sandbox-policy-file` | `SANDBOX_POLICY_FILE` | unset | [Cedar](https://www.cedarpolicy.com) document with the operator's grant policy - the operator layer of the three-layer capability decision and **the sole authority that enables a sandbox capability**: which callers (by `principal.namespace`, `principal.xrKind`) a module's manifest may be granted a private `/tmp` (`usePrivateTmp`), environment bound to step credentials (`setEnv`, `spendCredential`) or egress (`grantEgress`, also the host allowlist) for. It may also carry the SSRF CIDR block/allow rules (`forbid`/`permit` on `Action::"dialAddress"` with `context.ip.isInRange(ip(…))`/`isLoopback()`), which compile at load into the egress block list (with the built-in default block list) - Cedar never runs on the dial path. Evaluated **default-deny** (a `forbid` wins): a capability no permit matches is refused. Unset, no sandbox capability is grantable and a runtime offers only the default sandbox. A mounted ConfigMap satisfies it; it is compiled once and immutable for the process (restart to reload). See [operator grant policy](#operator-grant-policy) |
| `--health-address` | `HEALTH_ADDRESS` | `:8081` | plain-HTTP `/livez` (the process is up) and `/readyz` (200 once the caches are open and `--warm-modules` are loaded, 503 while warming) - what a Kubernetes probe can reach, since the function port speaks mTLS; empty disables them |
| `--metrics-address` | `METRICS_ADDRESS` | `:8080` | plain-HTTP Prometheus `/metrics` endpoint (see [Metrics](#metrics)) - the port function-sdk-go serves for the Go runtime; empty disables it |
| `--ttl` | | `60s` | TTL of responses the runtime itself produces (fatal results); a module sets its own |
| `--profile-guests` | `PROFILE_GUESTS` | unset | directory to write a per-run guest profile into, as [Firefox-profiler](https://profiler.firefox.com) JSON named `<digest>-<millis>.json` - the guest sampled every 10 ms, host imports marked. Debug tooling: it requires `--debug`, costs symbolication per run and writes a file per request, so never leave it set in production |

The usual function-sdk-go flags (`--insecure`, `--debug`, `--tls-certs-dir`,
`--address`, `--max-recv-message-size`) apply too. The caches live under
`/tmp/function-wasm-cache` (not configurable); back it with a volume through a
`DeploymentRuntimeConfig` to keep them across pod restarts, and mount an
emptyDir there if the pod's root filesystem is read-only. A volume shared
between pods is safe: entries are content-addressed and written atomically,
and artifacts of another wasmtime version are only removed once nothing has
written them for a day, so a rolling upgrade does not thrash.

Opening the sandbox is the same `DeploymentRuntimeConfig`: mount the Cedar
`--sandbox-policy-file`, and a tmpfs behind `TMPDIR` bounds the private `/tmp`:

```yaml
spec:
  deploymentTemplate:
    spec:
      template:
        spec:
          containers:
          - name: package-runtime
            args:
            - --sandbox-policy-file=/etc/function-wasm/policy.cedar
            env:
            - name: TMPDIR
              value: /scratch
            volumeMounts:
            - {name: policy, mountPath: /etc/function-wasm, readOnly: true}
            - {name: scratch, mountPath: /scratch}
          volumes:
          - name: policy
            configMap: {name: function-wasm-policy}
          - name: scratch
            emptyDir: {medium: Memory, sizeLimit: 64Mi}
```

A fuller example with every operator-authorable option is in
[`examples/deployment-runtime-config-cedar.yaml`](examples/deployment-runtime-config-cedar.yaml).

### Operator grant policy

`--sandbox-policy-file` is the **sole authority that enables a sandbox
capability**: a [Cedar](https://www.cedarpolicy.com) document, the operator's
grant policy and the top layer of the three-layer decision, that decides
*which callers* a module's manifest may be granted a private `/tmp`
(`usePrivateTmp`), environment bound to step credentials (`setEnv`,
`spendCredential`) or egress (`grantEgress`, which is also the host
allowlist) for. It is evaluated **default-deny** (a `forbid` overrides a
`permit`): a capability no permit matches is refused. Without a
`--sandbox-policy-file` no sandbox capability is grantable at all and a runtime
offers only the default sandbox (nothing but the request). The document lives
on the operator boundary alone - a module's manifest can only request, and
the Input's `compositionPolicy` can only narrow, so neither can widen past
it.

The principal every rule sees is the caller: `principal.namespace` and
`principal.xrKind` come from the observed composite resource (a
`RunFunctionRequest` carries no Composition name, so `principal.composition`
is presently always empty). The actions are `usePrivateTmp`, `setEnv` and
`grantEgress`; for egress the resource is the host or pattern within a
boundary-correct `HostPattern` hierarchy, and the context carries the method
and path. A separate, caller-independent action `requireSignature` (over the
`Repository` hierarchy) decides which repositories must carry a cosign signature
- see [signing](#trust-model) below. A policy that lets only `team-a` use a
private `/tmp` and lets any namespace reach `*.example.com`:

```cedar
permit (principal, action == Action::"usePrivateTmp", resource)
when { principal.namespace == "team-a" };

permit (principal, action == Action::"grantEgress", resource)
when { resource in HostPattern::"example.com" && context.method == "GET" };
```

Mount it and point the flag at it:

```yaml
spec:
  deploymentTemplate:
    spec:
      template:
        spec:
          containers:
          - name: package-runtime
            args:
            - --sandbox-policy-file=/etc/function-wasm/policy.cedar
            volumeMounts:
            - {name: policy, mountPath: /etc/function-wasm, readOnly: true}
          volumes:
          - name: policy
            configMap: {name: function-wasm-policy}
```

Because the policy is default-deny, a document that governs one capability
refuses every *other* capability a module requires unless it also
permits it: with a `--sandbox-policy-file` set, permit every capability you mean to
allow. `function validate --sandbox-policy-file …` reports the same verdicts offline
(the principal comes from `--xr`, else a zero principal that matches no
per-tenant condition).

The runtime reports readiness — caches open, engine up, the modules named
by `--warm-modules` (none by default) loaded — in two places: the gRPC
health service (`grpc.health.v1.Health`) on the function port, and plain
HTTP `/readyz` (plus `/livez`) on `--health-address` (`:8081`). Use the
HTTP one for Kubernetes probes: Crossplane runs functions with mTLS on the
function port, and kubelet's gRPC probe dials without credentials, so a
`grpc:` probe on `9443` never succeeds outside `--insecure`.

```yaml
apiVersion: pkg.crossplane.io/v1beta1
kind: DeploymentRuntimeConfig
metadata:
  name: function-wasm
spec:
  deploymentTemplate:
    spec:
      template:
        spec:
          containers:
          - name: package-runtime
            readinessProbe:
              httpGet:
                path: /readyz
                port: 8081
            livenessProbe:
              httpGet:
                path: /livez
                port: 8081
```

Ready is not warm: the first request for a module on a pod pays a
deserialize (with a warm volume) or a compile (~2 s for a large Go module).
`--warm-modules` moves that ahead of readiness: the runtime listens at
once but reports Not Serving while it loads the listed modules — a warm
volume makes that milliseconds, a cold cache one compile per module,
`--max-concurrent-compiles` at a time — and Serving when every entry is
loaded or has failed. Failures are logged (`Cannot warm module` with the
entry and the reason) and never hold readiness back: a wrong entry or an
unreachable registry costs that module its first request, not the pod its
traffic. `/livez` answers throughout, so a liveness probe is unaffected;
only `/readyz` (and the gRPC status) waits for warm-up. Warm-up runs the same
path a request does, so a warmed module is a memory hit for its first
request; with `--enable-memory-cache=false` it leaves the artifact on disk,
which is the point.

## Sizing

What a request and a module cost depends almost entirely on the guest's
toolchain — the host adds about 60 µs. Measured on linux/arm64 with the
example guests (a `function-sdk-go` Go guest, a raw-proto Go guest using
only the vendored glue, TinyGo, Rust):

| | Go (~75 MB) | Go, raw proto (~20 MB) | TinyGo (~1.4 MB) | Rust (~150 KB) |
|---|---|---|---|---|
| one request (CPU) | 8–11 ms, 91 % of it the Go runtime's own init | 1.2 ms | 0.4 ms | 0.05 ms |
| first compile of a module | 23–28 CPU-seconds, ~1 GB peak | 6 CPU-s | 1 CPU-s | 0.1 CPU-s |
| resident in memory | ~90 MB, file-backed | ~40 MB | 3.5 MB | 0.7 MB |
| per in-flight run | 11–16 MB | 4–8 MB | < 1 MB | < 1 MB |
| on disk per module version | ~230 MB (module + artifact) | ~60 MB | 5 MB | 0.9 MB |
| load from a warm volume | 6–8 ms | 2 ms | 0.2 ms | 0.05 ms |

Rules of thumb: memory ≈ resident modules + `--max-concurrent-compiles` × 1 GB
+ concurrent runs × 16 MB (`--max-concurrent-runs` caps that last term when
the caller's concurrency is not the number you want to size for); a cold
start compiles every module once, so ten Go modules on two cores take about
two minutes and a warm volume under `/tmp/function-wasm-cache` turns that
into a second, `--warm-modules` moves either ahead of readiness; disk grows
by one module + artifact per digest ever served unless `--max-cache-size`
bounds it. Requests scale linearly with cores — nothing in the run path is
serialised unless `--max-concurrent-runs` says so. Large observed state
costs on every request regardless of the guest (about 20 ms per MB of
composite resource, four protobuf passes). The full model of bounds and
budgets is
[docs/one-pager-resource-governance.md](docs/one-pager-resource-governance.md).

## Metrics

The runtime serves metrics where function-sdk-go puts them
(`:8080/metrics`), next to the [gRPC server series](#grpc-server-metrics).
The main exposition format is [OpenMetrics](https://prometheus.io/docs/specs/om/open_metrics_spec/)
1.0 (`application/openmetrics-text; version=1.0.0`), readable by any
OpenMetrics-capable collector, not only Prometheus; a scraper whose
`Accept` header asks for the classic Prometheus text format without
accepting OpenMetrics gets `text/plain; version=0.0.4` instead. The two
renderings carry identical series - same names, labels and values - so the
format never changes what a dashboard sees:

| metric | labels | meaning |
|---|---|---|
| `function_wasm_module_compile_duration_seconds` | | histogram of wasmtime compile time (compiled-cache misses) |
| `function_wasm_module_fetch_duration_seconds` | `source` = oci, http, path | histogram of fetch + verify time (blob-cache hits and served-file reads included) |
| `function_wasm_module_requests_total` | `outcome` = ok, refused, error | requests by outcome: refused = declined before the module ran (input, policy, grants, limits, resolution, verification — each also logged as `Request ended with a fatal result` with the reason), error = the load or the run failed |
| `function_wasm_module_run_duration_seconds` | `outcome` = ok, error, timeout | histogram of one guest run, instantiate to response (a wait for a run slot is not part of it, and a request that never got one is not counted) |
| `function_wasm_module_runs_in_flight` | | gauge of guest runs executing right now; pinned at `--max-concurrent-runs`, the bound is what requests wait on |
| `function_wasm_module_cache_events_total` | `cache` = compiled (memory), compiled-disk, blob; `event` = hit, miss, stale (compiled-disk only: an artifact wasmtime refused) | cache lookups |
| `function_wasm_module_cache_bytes` | `cache` = compiled-disk, blob | bytes on disk per store, measured every ten minutes |
| `function_wasm_module_http_requests_total` | `outcome` = ok, refused, budget, error | HTTP requests modules made through the host (`sandbox.egress`): the server answered; refused by the grant or the egress policy; a per-run budget or the timeout was hit; the request failed. No host label — the audit log line names it |
| `function_wasm_module_hostcall_duration_seconds` | | histogram of the slice of a run spent inside host imports (`wasmfn.log`, `wasmfn.http` and WASI); the rest of `run_duration_seconds` is guest compute. A run that is slow here is waiting on the host - usually an upstream `wasmfn.http` talks to |
| `function_wasm_module_memory_denials_total` | `reason` = limit, pool | guest memory growths denied - at the run's ceiling (`limits.memory` or `--module-memory-limit`) or because `--max-total-run-memory` could not serve the growth before the run's deadline. The guest sees `memory.grow` fail |

No metric carries a module identity: the set of digests a Function serves is
unbounded. Logs carry the module reference and digest.

### gRPC server metrics

The transport also serves the gRPC server series the Go runtime got from
function-sdk-go's grpc-prometheus interceptor, with the same names, labels
and meanings - dashboards and alerts built on them keep working:
`grpc_server_started_total`, `grpc_server_handled_total` (with a
`grpc_code` label carrying the gRPC code name, `OK` … `Unauthenticated`),
`grpc_server_msg_received_total` and `grpc_server_msg_sent_total`, each
labelled `grpc_type`/`grpc_service`/`grpc_method`. As under the Go
runtime - whose interceptor was unary-only - unary calls (`RunFunction`,
health `Check`) are counted and streaming methods (reflection, health
`Watch`) exist as permanently zero series; every method the server carries
is pre-created at zero on startup, so a scrape sees the full set before
the first request. There is no `grpc_server_handling_seconds`:
function-sdk-go never enabled the histogram, and
`function_wasm_module_run_duration_seconds` covers latency.

### Profiling a module

To see where a module's milliseconds go, start the runtime with `--debug
--profile-guests=<dir>`: every run then writes one profile into that
directory, named `<digest>-<millis>.json` - load it at
[profiler.firefox.com](https://profiler.firefox.com). The guest is sampled
every 10 ms with full wasm stacks (build the module with DWARF, i.e.
without stripping, for file-and-line frames), and every `wasmfn.log`,
`wasmfn.http` and WASI call appears as a marker, so time spent in guest
compute, in the runtime's host imports and waiting on an upstream server
are distinguishable at a glance.

The natural place to use it is the [local render loop](#render-locally):

```bash
cargo run --release -p function-wasm -- --insecure --debug --module-dir=. --profile-guests=/tmp/profiles
```

Profiling is debug tooling: the flag refuses to start without `--debug`,
each profiled run pays symbol-table setup (noticeable for a large Go
guest), and every request writes a file. Do not leave it set in
production - there, the `run_duration_seconds` /
`hostcall_duration_seconds` split answers the coarse version of the same
question continuously.

## Trust model

The complete model — parties, what pins the code, credentials, what the
guest sees, threats considered — is
[docs/one-pager-trust-model.md](docs/one-pager-trust-model.md); this is the
short version.

A module runs with the privileges of the Composition that references it: it
sees the request's observed and desired state, context and the step
credentials, exactly as a native function would — except the credential that
pulled it (`module.oci.credentials`), which is the host's and never reaches
the guest. With `module.from` the **composite resource's author** picks the
module — use it where XR authors are trusted to, fence what they can pick
with the `compositionPolicy`'s `pullModule` permits (required for `OCI` and
`HTTP` sources: without them the XR author would point the runtime at any
host and read what its answer says), and restrict it to signed code with
`--cosign-key`. A source read from the XR may name a step credential only
where a `spendCredential` permit matches it, and only for a repository a
`pullModule` permit admits: otherwise the XR author would pick the
registry host the secret is sent to, and a registry that answers with a
`Basic` challenge receives it — without such a permit an XR-chosen module is
pulled with the runtime's own Docker config (mount one through a
`DeploymentRuntimeConfig` and set `DOCKER_CONFIG`; credentials there are
bound to their registry host) or anonymously. `compositionPolicy` and
`limits` are read from the Input only, so an XR author can choose code, not
widen its permissions, grants or budget; every one of these rules is
enforced by the runtime on every request (Crossplane never installs the
Input CRD), and [`function validate`](#validate-a-composition) runs the
same code over a Composition offline. The sandbox protects the runtime
process — and with it every other Composition sharing the Function — from a
crashing, looping or memory-hungry module, and gives a module no filesystem,
environment or network beyond what its manifest requires and both Cedar
layers (the Input's `compositionPolicy` and the operator's
`--sandbox-policy-file`) permit: a private `/tmp` that exists for one
request (host directories are never mountable — the request is a module's
only view of the world beyond what it writes for itself), exactly the
environment variables its manifest binds to step credentials (non-secret
configuration travels in `config`), and HTTP requests through the host to
the hosts, methods and paths its manifest lists within both policies
(block list, budgets). A module granted egress can send whatever its
request carries — step credentials included — to those hosts, which is why
the grant is the policy layers' alone (a manifest can only ask, and an XR
author widens nothing), every request leaves an audit line with the module
digest, and `--cosign-key` is strongly recommended wherever egress is
granted. Every remote module is pinned by a digest the Composition states —
the OCI reference's manifest digest (the manifest names the layer's digest,
and both are verified on fetch) or `http.digest` — so nothing that runs can
change without the Composition changing.

To restrict a Function to modules your organisation signed, sign them with
`cosign sign --key cosign.key <ref>` and start the runtime with
`--cosign-key cosign.pub` (a `DeploymentRuntimeConfig` mounts the key and
sets the flag or `COSIGN_KEY`). Every OCI module is then verified once per
manifest digest per process before it is run — before any cache is
consulted, so an artifact left on a persisted volume by a runtime without
the key is not served by one with it — and unsigned sources are refused.
Keyless (Fulcio/Rekor) signatures are not verified.

`--cosign-key` on its own is all-or-nothing: with it, every module must be
signed. An operator grant policy (`--sandbox-policy-file`) can instead require a
signature **per repository** through a `requireSignature` rule over the same
boundary-correct `Repository` hierarchy the module fence uses, so only the
repositories you name must be signed while others run unsigned:

```cedar
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/acme/prod" };
```

The crypto is unchanged: `--cosign-key` still provides the keys and performs
the check; Cedar only decides *whether* a given repository must be signed, and
the refusal (a required module that is unsigned, or that the runtime has no
`--cosign-key` to verify) happens before any cache, exactly as the
all-or-nothing check does. Precedence: **without** `--sandbox-policy-file`, `--cosign-key`
keeps its all-or-nothing meaning unchanged; **with** a `--sandbox-policy-file`, the
per-repository `requireSignature` decision governs which modules must be signed
(a repository no rule names is not required), and `--cosign-key` supplies the
keys. A signature no key can check is refused, so the requirement is
fail-closed.

## Development

```shell
cargo build --workspace && cargo test --workspace   # engine, runtime, guestfn - conformance goldens and scaffold goldens included
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
(cd examples/hello-go && go test ./...)             # the example guest and its vendored internal/wasmfn glue
make -C examples/hello-go render-check              # function validate + crossplane render through the real runtime
```

The workspace tests build the example guests to WebAssembly and run all five
through the host when their toolchains (go, tinygo, cargo + wasm32-wasip1,
zig) are on PATH, and skip the ones that are not. See
[AGENTS.md](AGENTS.md) for the layout and conventions.

Design documents live under `docs/` as one-pagers: the implemented ones
(cache, module source schema, trust model, resource governance, sandbox,
admission and inspection tooling, the module manifest, the three-layer
authorization model, governance and performance phases) and the drafts of
what comes next (guest language support, sandbox requests for
manifest-less sources, a Nix development environment).
[AGENTS.md](AGENTS.md#key-reference-documents) lists them.
