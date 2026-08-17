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
  a `policy` fencing which registries the team may pick from,
  `--cosign-key` so only modules signed with the organisation's key run, and
  the resource caps of the sandbox (`limits` per step, the runtime's flags
  as ceilings).
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
time beyond what a Composition explicitly grants and the operator allows. By
default a module gets no network, filesystem or environment; cluster state
comes in through the request (observed state, required resources), and the
module returns desired state. The sandbox opens selectively, one capability
per operator flag ([docs/one-pager-sandbox.md](docs/one-pager-sandbox.md)):
a Composition may grant its module a private `/tmp` for the request and
environment variables (`sandbox.filesystem`, `sandbox.env`, `sandbox.envFrom` - see the Input
reference; host directories are deliberately not mountable), and **HTTP
egress through the host** (`sandbox.egress`,
`--enable-sandbox-egress`) to call the APIs its Composition lists, with the
host resolving, filtering, budgeting and auditing every request — see
[HTTP egress](#http-egress).

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
guestfn build                                   # fn.wasm (wasip1); prints the ABI verdict and the manifest summary
guestfn inspect fn.wasm                         # size, ABI verdict, exports, imports, memory
guestfn push ghcr.io/example/greeter:v0.1.0     # OCI artifact with the manifest; prints the module and sandbox blocks for the Composition
```

`guestfn build` ends with the verdict the runtime reaches when it loads the
module — `Built fn.wasm (73.9 MB, ABI v1, imports wasmfn.http wasmfn.log)`
— and fails, in the runtime's words, on a module the runtime would refuse
(`module does not export "wasmfn_run"`); `guestfn push` refuses to publish
such a module for the same reason. The check is the runtime's own: `guestfn`
compiles the module with the same wasmtime engine (a couple of seconds for a
large Go module), so what it prints is what a load says — which also makes
`guestfn` a CGo binary like the runtime (`go install` needs a C compiler).
`guestfn inspect fn.wasm` shows what the runtime sees — size, verdict,
exports, imports with their types, memory limits — and `guestfn inspect
ghcr.io/example/greeter:v0.1.0` describes an artifact from its manifest
(media types, layer, annotations) without pulling, `--pull` reading the
module too; `--output json` for scripts.

The scaffold also has a **`wasmfn.yaml`** — the module's manifest: what it
declares about itself (`name`, `version`, `abi: 1`), the sandbox grants it
cannot run without (`requires`: egress rules in the Input's own shape,
`filesystem.privateTmp` — the scaffold requires nothing; environment
variables are values a Composition sets, not a capability, so they are not
a requirement) and the
JSON Schema of its `config` (the scaffold's covers `greeting` and
`greetingUrl`). `guestfn build` validates it and checks the example
Composition's `config` against the schema; `guestfn push` publishes it
beside the module (`--manifest` names another file, `--module-version` and
`--revision` override the version and set the revision annotation) and
prints, under the `module:` block, the `sandbox:` block a Composition needs
to satisfy it; the runtime then refuses a Composition that grants less than
the module requires, before the module runs (see [Module
manifests](#module-manifests)). `guestfn manifest validate` checks the
file, `guestfn manifest show ghcr.io/example/greeter:v0.1.0` prints what a
published module declares, and `guestfn scaffold composition --from
ghcr.io/example/greeter:v0.1.0` (or `--from fn.wasm`) writes a Composition
step — `module` pinned, `sandbox` from `requires`, a `config` skeleton from
the schema; `--full` for a whole Composition.

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
toolchain works, and `guestfn` scaffolds and builds three flavours — the
same greeting function each time:

| `guestfn init --lang` | example | toolchain | how it talks protobuf | module size |
|---|---|---|---|---|
| `go` (default) | [`examples/hello-go`](examples/hello-go) | Go + `wasmfn` + function-sdk-go | `request`/`response`/`resource` helpers | ~75 MB (13 MB compressed) |
| `tinygo` | [`examples/hello-tinygo`](examples/hello-tinygo) | [TinyGo](https://tinygo.org) | protobuf-go message types + [vtprotobuf](https://github.com/planetscale/vtprotobuf)'s reflection-free codecs, generated from the vendored proto (shipped pre-generated; `go generate` + protoc to redo) | ~1.8 MB |
| `rust` | [`examples/hello-rust`](examples/hello-rust) | Rust, `wasm32-wasip1` (`cargo`, `protoc`) | [prost](https://github.com/tokio-rs/prost) over the vendored proto | ~250 KB |

`guestfn build` picks the toolchain from the project (`Cargo.toml` → cargo;
a `go.mod` requiring vtprotobuf but not `wasmfn` → tinygo; otherwise go) or
takes `--lang`. The TinyGo and Rust flavours carry their ~40 lines of ABI glue
and a small HTTP helper over `wasmfn.http` in the open; each example has a
`make render-check` that runs it through the runtime, and the root tests run
all three through the host as well — with and without an egress grant.

### Render locally

Build the module, then run the runtime from a checkout serving your project
directory, and render with the Development runtime the scaffold's
`example/functions.yaml` declares:

```shell
guestfn build
go run github.com/jonasz-lasut/function-wasm/cmd/function@latest --insecure --debug --module-dir=.
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
(`sandbox` grants against `--enable-sandbox-*` and the egress policy,
`limits` against `--module-timeout`/`--module-memory-limit`, `module` and
`policy` shape) to every function-wasm step of the Compositions (or bare
`Input` documents) you give it, printing the runtime's own words:

```shell
go run github.com/jonasz-lasut/function-wasm/cmd/function@latest validate \
  example/composition.yaml --module-dir=. --resolve
# or, with the released image (its entrypoint is the runtime):
docker run --rm -v "$PWD:/w" ghcr.io/jonasz-lasut/function-wasm:<version> validate /w/composition.yaml \
  --enable-sandbox-egress --sandbox-egress-policy /w/egress-policy.yaml
```

```
composition.yaml: Composition/hello pipeline[0] hello: OK (oci ghcr.io/example/greeter:v1@sha256:3f2a…, limits timeout 5s memory 128Mi, egress api.example.com)
composition.yaml: Composition/hello pipeline[1] labeler: refused: sandbox.egress.http[0].host "evil.example.com" is outside the runtime's egress policy (allowed: api.example.com)
```

`--xr xr.yaml` materialises `module.from` sources against that composite
resource, as the observed XR would (without it a `from` source is checked
for the `policy.repositoryAllowList` it requires and reported as the XR's
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
policy:                        # optional; only fences a module chosen through module.from
  repositoryAllowList: ["ghcr.io/example-org/"]
  credentialsAllowList: ["registry"]
limits:                        # optional; each at most the runtime's ceiling
  timeout: 5s
  memory: 128Mi
sandbox:                       # optional; grants within the runtime's --enable-sandbox-* flags
  filesystem:
    privateTmp: true                                # an empty, writable /tmp per request; nothing else is mountable
  env: {LOG_LEVEL: debug}                           # non-secret configuration
  egress:
    http: [{host: api.example.com, methods: [GET]}] # HTTP through the host, within --sandbox-egress-policy
config: {...}                  # optional; opaque, forwarded to the module
```

Everything but the source is read from the Input: `policy`, `limits` and
`sandbox` are the Composition's, never the composite resource's.

| field | type | description |
|---|---|---|
| `module` | object | **required** — where the module comes from |
| `module.type` | string | **required** — `OCI`, `HTTP` or `Path`. Exactly one of the object it names (`oci`, `http`, `path`) or `module.from` is set, and no object of another type may be present. The runtime checks it on every request — the Input's CRD (with the same rules as CEL) is never installed by Crossplane; a function input is part of a Composition, not an object, so its schema only serves tooling that validates against it (`crossplane resource validate` with the package's `input/` directory) and IDEs |
| `module.oci.ref` | string | OCI artifact reference **pinned to its manifest digest**, `registry/repo@sha256:…`, as `guestfn push` prints it. The manifest digest pins the module (the manifest states its layer's digest; both are verified on fetch) and addresses the caches. A tag alone is not accepted (tags can be moved; the runtime resolves nothing at request time); `registry/repo:tag@sha256:…` is fine — the digest is what is fetched, the tag is human-readable context and may even no longer exist. The module is the `application/wasm` (or `vnd.wasm` content) layer, or the only layer; a tar layer (a `FROM scratch` image) must hold it at exactly `/fn.wasm` |
| `module.oci.credentials` | string | name of a pipeline-step credential (a Secret with `.dockerconfigjson`, or `username` and `password` keys) used to pull. Without it the runtime's Docker config (`DOCKER_CONFIG`) and anonymous access are tried. An object read through `module.from` may name one only when `policy.credentialsAllowList` lists it (see `policy`) |
| `module.http.url` | string | download the module over HTTP(S) |
| `module.http.digest` | string | **required** — `sha256:<hex>` of the module; the download is verified against it |
| `module.path` | string | a file relative to the runtime's `--module-dir`; refused unless that flag is set — local rendering and volume-mounted modules; carries no digest |
| `module.from` | string | a field of the observed composite resource, under `spec.` or `status.`, holding the source `module.type` names — an object `{ref, credentials}` for `OCI`, `{url, digest}` for `HTTP`, a string for `Path` — e.g. `status.module`; read on every request and decoded strictly (a typo or a wrong shape is a fatal result naming the field), so each XR can choose its module. What it may choose is fenced by `policy` |
| `policy` | object | fences a module chosen through `module.from`; ignored for a source the Composition names statically (that source is trusted as the Composition is) |
| `policy.repositoryAllowList` | []string | string prefixes an XR-chosen `oci.ref` (or `http.url`) must start with, e.g. `ghcr.io/example-org/` — the trailing slash matters. Matched against the normalized location (`registry/repository` for OCI, `scheme://host/path` for HTTP; a ref or URL with dot segments is refused). A ref outside every prefix is a fatal result naming the policy and the ref. **Required whenever `module.from` names an `OCI` or `HTTP` source** — an unfenced XR author could point the runtime at any host and read what its answer says |
| `policy.credentialsAllowList` | []string | step credentials an XR-chosen `oci` object may name, spent only on a ref `repositoryAllowList` admits — so it requires `repositoryAllowList` (`policy.credentialsAllowList requires policy.repositoryAllowList` otherwise). Absent or empty, an XR object naming credentials is refused: the XR author would otherwise choose the registry host the secret is sent to |
| `limits.timeout` | duration | wall-clock budget of one run, e.g. `5s`; at most `--module-timeout`, else a fatal result naming both (`limits.timeout 1m0s exceeds the runtime's --module-timeout of 30s`). The request deadline still applies if shorter |
| `limits.memory` | quantity | linear memory a run may use, e.g. `128Mi`; at most `--module-memory-limit`, else a fatal result naming both (`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`) |
| `limits.instructions` | int64 | wasm instructions one run may execute (wasmtime fuel), e.g. `100000000`; at most `--module-instruction-limit`, else a fatal result naming both. Deterministic across nodes and runs. Requires `--enable-fuel`; without it the field is refused |
| `limits.concurrency` | int32 | at most N runs of this step at once, across all requests, keyed by the module's content digest. A further request waits under its own context; when the deadline passes first, it is a fatal result that consumed nothing and is not counted as a run. A value above `--max-concurrent-runs` is silently capped. No ceiling flag: this only narrows |
| `sandbox` | object | grants beyond the default sandbox (nothing but the request), each within a ceiling the operator sets with an `--enable-sandbox-*` flag: a grant outside the ceiling is a fatal result naming the grant and the flag, before any module is resolved. Read from the Input only. Filesystem, environment and HTTP egress are implemented ([docs/one-pager-sandbox.md](docs/one-pager-sandbox.md)) |
| `sandbox.filesystem.privateTmp` | bool | a private, empty, writable `/tmp` for the duration of the request — created under the runtime's `$TMPDIR` before the module runs and removed afterwards, whatever the outcome (`--enable-sandbox-private-tmp`) |
| `sandbox.egress.http[]` | `{host \| hostPattern, methods, pathPrefix}` | HTTP(S) requests the host performs for the module (`wasmfn.HTTPClient()` in Go, the `wasmfn.http` import elsewhere): exactly one of `host` (exact name, port ignored) and `hostPattern` (`*.example.com`: every name under it, not the apex), at least one of `GET HEAD POST PUT PATCH DELETE OPTIONS`, and an optional `pathPrefix` the (normalized) path must start with. Rules for the same host add up. Needs `--enable-sandbox-egress`; each rule must fit `--sandbox-egress-policy`, else a fatal result names the rule and what the policy allows. See [HTTP egress](#http-egress) |
| `sandbox.env[]` | `{name, value \| valueFrom}` | environment variables the module sees, exactly these and nothing of the runtime's; names are identifiers, no duplicates. A literal `value` may not contain NUL. A `valueFrom.credential` reads a key of a step credential (`--enable-sandbox-env`) |
| `sandbox.env[].valueFrom.credential` | `{name, key}` | reads the value from step credential `name`, key `key`; the pull credential (`module.oci.credentials`) is refused as a source |
| `sandbox.envFrom[]` | `{credential, prefix}` | imports every key of a step credential as environment variables; `prefix` is prepended to each key. Keys that are not valid identifiers (after prefixing) refuse the run. A key colliding with an `env[]` name is refused. The pull credential is refused as a source (`--enable-sandbox-env`) |
| `config` | object | opaque, passed to the module untouched inside the request input; a Go guest reads it with `wasmfn.GetConfig` |

Letting each composite resource choose its module — the Composition names
the type and the XR field, the field holds the source, the policy says what
it may hold:

```yaml
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module:
        type: OCI
        from: spec.module           # spec.module: {ref: ghcr.io/example-org/greeter@sha256:…}
      policy:
        repositoryAllowList: ["ghcr.io/example-org/"]
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

An XR-chosen module may spend that credential only if the policy allows
it, and only within the repositories the policy admits:

```yaml
    module:
      type: OCI
      from: status.module           # status.module: {ref: ghcr.io/example-org/…@sha256:…, credentials: registry}
    policy:
      repositoryAllowList: ["ghcr.io/example-org/"]
      credentialsAllowList: ["registry"]
```

A step may ask for less than the runtime allows, never more:

```yaml
    limits:
      timeout: 5s        # ≤ --module-timeout
      memory: 128Mi      # ≤ --module-memory-limit
```

Opening the sandbox: the operator enables each capability, the Composition
asks for what its module needs, the module gets the intersection. With a
runtime started with `--enable-sandbox-private-tmp --enable-sandbox-env`,
this step's module scratches in an empty `/tmp` that is gone when the
request ends and sees environment variables - host directories are never
mountable into a module, whatever the flags:

```yaml
    sandbox:
      filesystem:
        privateTmp: true
      env:
      - name: LOG_LEVEL
        value: debug                           # literal: never a secret
      - name: AWS_ACCESS_KEY_ID
        valueFrom:
          credential:
            name: aws                          # step credential "aws", key "access_key_id"
            key: access_key_id
      envFrom:
      - credential:
          name: vault                          # every key of step credential "vault"
        prefix: VAULT_                         # becomes VAULT_TOKEN, VAULT_ADDR, ...
```

A `privateTmp`/`env`/`envFrom` grant without its `--enable-sandbox-*` flag is a
fatal result naming the grant and the flag; the module never runs. The pull
credential (`module.oci.credentials`) is refused as a source for `env` and
`envFrom`: the module must never see the secret that fetched it.

### Module manifests

A module published with `guestfn push` from a project that has a
`wasmfn.yaml` carries a **manifest** beside it in the same OCI artifact (a
second layer, `application/vnd.wasmfn.manifest.v1+json`, covered by the
manifest digest the Composition pins and by a cosign signature): the
sandbox grants it cannot run without (`requires`: egress rules in the
Input's own shape and `filesystem.privateTmp` — never `env`: variables are
values the Composition sets, not a capability), a JSON Schema for
its `config`, its ABI and the oldest runtime that serves it. The runtime
reads it once per digest (into `/tmp/function-wasm-cache/manifests`) and,
after admission and load, holds it against what the Composition granted —
**narrowing only**: a manifest can make a run fail earlier and say why, it
can never make a run possible or widen a grant. An unmet requirement is a
fatal result before the module runs — `module oci ghcr.io/example/greeter@sha256:… requires sandbox.egress.http host api.example.com methods [GET] pathPrefix /v1/, which the Composition does not grant`,
`… requires sandbox.filesystem.privateTmp, which the Composition does not grant`,
`… requires runtime v0.3.0 or newer, this is v0.2.1` — and so is a
`config` outside the schema: `… config does not match the module's schema:
/greeting: got number, want string`. A module without a manifest, and every
`path` or `http` source, runs as before. `guestfn push` prints the
`sandbox:` block a Composition needs under the `module:` block, `guestfn
inspect <ref>` shows what a module requires, and `function validate
--resolve` applies the same check offline.

## HTTP egress

A module can be granted HTTP(S) requests **through the host**: it never
opens a socket (wasip1 has none), it asks the runtime, and the runtime
resolves the name, refuses addresses on its block list, terminates TLS with
its own roots, checks the host, method and path against the Composition's
rules, follows redirects within them, enforces the operator's budgets,
counts and logs every request, and hands the response back. Three parties,
in the order they decide:

1. **The operator** turns the capability on and sets the ceiling:

   ```shell
   function --enable-sandbox-egress --sandbox-egress-policy /etc/function-wasm/egress.yaml
   ```

   ```yaml
   # /etc/function-wasm/egress.yaml — every field optional
   hosts: [api.example.com]              # exact hosts a Composition may grant
   hostPatterns: ["*.googleapis.com"]    # patterns it may grant; a Composition's pattern must sit under one
   blockedCIDRs: ["203.0.113.0/24"]      # refused whatever the grant, on top of the default block list
   allowedCIDRs: ["10.96.0.0/12"]        # exceptions to the default block list (a cluster service range, say)
   timeout: 10s                          # one request, name lookup to last body byte — a duration string
   maxRequests: 16                       # per run
   maxResponseBytes: 4194304             # a longer body is an error, not a truncated body (headers are capped separately, at 64 KiB)
   maxRedirects: 5                       # each hop checked like the first request; hops count here, not against maxRequests
   rateLimit:                             # process-wide token bucket per module digest; without it, no rate limit
     requestsPerMinute: 60                # sustained rate
     burst: 10                            # maximum tokens available at once (default: requestsPerMinute rounded down, at least 1)
   ```

   Without a file, any public host may be granted within the defaults shown.
   The default block list — loopback, link-local (the cloud metadata
   endpoint), RFC 1918, carrier-grade NAT (`100.64.0.0/10`, a common pod
   range), IPv6 unique-local, the NAT64 prefix, and the unspecified,
   multicast and reserved ranges — applies to **every address a name
   resolves to** (a zoned IPv6 literal such as `[::1%25lo]` is never
   dialled), and the host dials the address it checked, so a name cannot
   rebind between the check and the connection. `blockedCIDRs` add to it,
   `allowedCIDRs` punch holes in it, and an explicit `blockedCIDRs` entry
   wins over an `allowedCIDRs` one; to reach a loopback service in a local
   test both `127.0.0.0/8` and `::1/128` need allowing, since every resolved
   address is judged. `HTTP_PROXY` is not honoured: the host must see the
   destination address to judge it. What the guest is told about a refusal
   is only that the policy refused; the resolved address and the block-list
   entry stay in the runtime's audit line.

2. **The Composition** grants what its module needs, within the ceiling:

   ```yaml
       input:
         apiVersion: wasm.fn.crossplane.io/v1beta1
         kind: Input
         module:
           type: OCI
           oci: {ref: ghcr.io/example/pricing@sha256:…}
         sandbox:
           egress:
             http:
             - host: api.example.com
               methods: [GET]
               pathPrefix: /v1/prices/
             - hostPattern: "*.googleapis.com"
               methods: [GET, POST]
   ```

   A rule outside the ceiling is a fatal result before the module runs
   (`sandbox.egress.http[0].host "evil.example.com" is outside the runtime's
   egress policy (allowed: *.googleapis.com, api.example.com)`), and so is a
   grant on a runtime without `--enable-sandbox-egress`. `sandbox` is read
   from the Input only: an XR author who picks a module through
   `module.from` cannot widen its egress.

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
   settled: `sandbox` grants are checked against the runtime's
   `--enable-sandbox-*` flags (an egress grant against the operator's
   policy too), `limits` against the runtime's ceilings. A `module.from` source is then
   read from the observed composite resource (`type: OCI` with
   `from: status.module` expects `status.module` to be `{ref, credentials}`;
   a typo or a wrong shape is a fatal result naming the field) and checked
   against `policy`. Resolving does no I/O: the **digest** that pins the
   module comes from the Input — the manifest digest of an OCI reference,
   `http.digest` for a URL — or from hashing a served file when it changes.
2. The digest is looked up in the caches — compiled modules in memory (kept
   ten minutes after their last use), then wasmtime artifacts on disk, then
   fetched modules on disk under `/tmp/function-wasm-cache`. Only a module
   never seen by this node is fetched (verified against its digest, written
   to disk) and compiled — about two seconds for a 75 MB Go module; a
   module without the ABI's exports, or importing what the host does not
   provide, is refused here — and its artifact written to disk. Restarts and
   registry outages need no network. Details in
   [docs/one-pager-cache.md](docs/one-pager-cache.md). The module's
   [manifest](#module-manifests), if its artifact carries one, is then held
   against what step 1 granted: an unmet requirement or a `config` outside
   the module's schema is a fatal result before anything runs.
3. Every request gets a fresh instance (about ten milliseconds): WASI with no
   network access, and no filesystem or environment beyond what `sandbox`
   granted — a private `/tmp` created for this request and removed after
   it, exactly the listed environment variables; HTTP requests, if granted, go through the host
   ([HTTP egress](#http-egress)); guest logs flow into the runtime's logger
   with the module reference attached; stdout and stderr are the pod's, so a
   Go panic's stack shows up in `kubectl logs`.
4. The response is returned as the module produced it. A trap, timeout
   (`limits.timeout` or `--module-timeout`, or the request deadline if
   shorter), memory limit (`limits.memory` or `--module-memory-limit`) or an
   unusable module is a fatal result naming the module — never a crashed
   function pod. So is a request that, with `--max-concurrent-runs` set,
   reaches its deadline while waiting for a run slot (`waiting for a run
   slot: context deadline exceeded`): it never ran.

The full host/guest contract is in [docs/abi.md](docs/abi.md).

## Runtime flags

The binary has two subcommands: `serve` — the default, so `function
--insecure --module-dir=.` and a `DeploymentRuntimeConfig`'s `args` need no
subcommand — and [`validate`](#validate-a-composition), which takes the
ceiling flags below (`--module-dir`, `--max-module-size`,
`--module-timeout`, `--module-memory-limit`, `--cosign-key`, the
`--enable-sandbox-*` flags and `--sandbox-egress-policy`) with the same
defaults and environment variables, so a Composition is validated against
exactly what a runtime started with those flags would admit.

| flag | env | default | purpose |
|---|---|---|---|
| `--module-dir` | `MODULE_DIR` | unset | serve `path` sources from this directory |
| `--max-module-size` | | `128` MB | largest module accepted |
| `--module-timeout` | | `30s` | wall-clock budget of one run; the ceiling for `limits.timeout` |
| `--module-memory-limit` | | `512` MB | linear memory a run may use; the ceiling for `limits.memory` |
| `--enable-memory-cache` | `ENABLE_MEMORY_CACHE` | `true` | keep compiled modules in memory between requests. With `--enable-memory-cache=false` (or `--no-enable-memory-cache`) each request maps the module's compiled artifact from disk (6–8 ms for a large Go module) and releases it afterwards |
| `--max-cached-modules` | `MAX_CACHED_MODULES` | `0` (unbounded) | most compiled modules resident at once; the least recently used is dropped beyond it (freed once its last run ends). Artifacts are mapped from disk, so a resident Go module costs ~90 MB of file-backed memory |
| `--max-concurrent-compiles` | `MAX_CONCURRENT_COMPILES` | `1` | modules compiled at once. One compile already uses every core (~25 CPU-seconds and ~1 GB for a large Go module); further first requests wait their turn instead of multiplying that |
| `--max-cache-size` | `MAX_CACHE_SIZE` | `0` (unbounded) | MB the two on-disk caches may hold together; past it the least recently used entries (fetched modules and artifacts alike, ~230 MB per Go module version) are removed, at startup and every ten minutes. Size the volume, or set this below its size |
| `--cosign-key` | `COSIGN_KEY` | unset | PEM file of cosign public key(s); when set only OCI modules with a matching `cosign sign --key` signature run, and `http`/`path` sources are refused |
| `--max-concurrent-runs` | `MAX_CONCURRENT_RUNS` | `0` (unbounded) | module runs executing at once; a further request waits for a slot under its own deadline and, if that passes first, is a fatal result (`waiting for a run slot: context deadline exceeded`) without having run. Unbounded, concurrency is the caller's — Crossplane's reconcile workers |
| `--max-total-run-memory` | `MAX_TOTAL_RUN_MEMORY` | `0` (unbounded) | total linear-memory budget in MB across all running modules; a run reserves its effective limit (`limits.memory` or `--module-memory-limit`) from the pool before it starts and waits under its deadline when the pool is full. A step that states a small `limits.memory` gets more parallelism |
| `--warm-modules` | `WARM_MODULES` | unset | modules loaded before the health service reports Serving — resolved, verified (`--cosign-key` applies), then compiled or mapped through the same caches a request uses: OCI references pinned to their manifest digest (`repo[:tag]@sha256:…`, pulled with the runtime's Docker config) and, with `--module-dir`, `path:<file>` entries. Repeatable or comma-separated. An entry that fails to load is logged with the reason and does not stop the pod from serving; that module is loaded on its first request as usual |
| `--enable-fuel` | `ENABLE_FUEL` | `false` | count wasm instructions per run (wasmtime fuel). When on the `run_instructions` histogram is populated, `limits.instructions` is admitted and the compiled artifact cache gains a separate namespace (fuel changes wasmtime's code generation). A fuel-exhausted run is a fatal result (`module exceeded its instruction budget`), with outcome `fuel` in the run-duration histogram |
| `--module-instruction-limit` | `MODULE_INSTRUCTION_LIMIT` | `0` (unbounded) | ceiling for `limits.instructions`; zero means metered but unbounded (the histogram observes, nothing is capped). Only meaningful with `--enable-fuel` |
| `--enable-sandbox-private-tmp` | `ENABLE_SANDBOX_PRIVATE_TMP` | `false` | let Compositions give a module a private, empty, writable `/tmp` per request (`sandbox.filesystem.privateTmp`), created under the runtime's `$TMPDIR` (probed at startup) and removed when the run ends. There is no byte quota: to bound what a module may write, point `TMPDIR` at a tmpfs `emptyDir` with a `sizeLimit` through a `DeploymentRuntimeConfig` |
| `--enable-sandbox-env` | `ENABLE_SANDBOX_ENV` | `false` | let Compositions set the environment variables a module sees (`sandbox.env`, `sandbox.envFrom`); the runtime's own environment is never passed on |
| `--enable-sandbox-egress` | `ENABLE_SANDBOX_EGRESS` | `false` | let Compositions grant modules HTTP(S) egress through the host (`sandbox.egress`); off, any such grant is a fatal result naming the flag. See [HTTP egress](#http-egress) |
| `--sandbox-egress-policy` | `SANDBOX_EGRESS_POLICY` | unset | YAML/JSON file with the egress ceiling: `hosts`, `hostPatterns` (any host when both are empty), `blockedCIDRs`/`allowedCIDRs` on top of the default block list, and the per-run budgets `timeout` (10s), `maxRequests` (16), `maxResponseBytes` (4 MiB), `maxRedirects` (5) |
| `--health-address` | `HEALTH_ADDRESS` | `:8081` | plain-HTTP `/livez` (the process is up) and `/readyz` (200 once the caches are open and `--warm-modules` are loaded, 503 while warming) — what a Kubernetes probe can reach, since the function port speaks mTLS; empty disables them |
| `--ttl` | | `60s` | TTL of responses the runtime itself produces (fatal results); a module sets its own |

The usual function-sdk-go flags (`--insecure`, `--debug`, `--tls-certs-dir`,
`--address`, `--max-recv-message-size`) apply too. The caches live under
`/tmp/function-wasm-cache` (not configurable); back it with a volume through a
`DeploymentRuntimeConfig` to keep them across pod restarts, and mount an
emptyDir there if the pod's root filesystem is read-only. A volume shared
between pods is safe: entries are content-addressed and written atomically,
and artifacts of another wasmtime version are only removed once nothing has
written them for a day, so a rolling upgrade does not thrash.

Opening the sandbox is the same `DeploymentRuntimeConfig`: the flags go in
`args`, and a tmpfs behind `TMPDIR` bounds the private `/tmp`:

```yaml
spec:
  deploymentTemplate:
    spec:
      template:
        spec:
          containers:
          - name: package-runtime
            args:
            - --enable-sandbox-private-tmp
            - --enable-sandbox-env
            env:
            - name: TMPDIR
              value: /scratch
            volumeMounts:
            - {name: scratch, mountPath: /scratch}
          volumes:
          - name: scratch
            emptyDir: {medium: Memory, sizeLimit: 64Mi}
```

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
only `wasmfn`, TinyGo, Rust):

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

The runtime serves Prometheus metrics where function-sdk-go puts them
(`:8080/metrics`), next to the gRPC server metrics:

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

No metric carries a module identity: the set of digests a Function serves is
unbounded. Logs carry the module reference and digest.

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
with `policy.repositoryAllowList` (required for `OCI` and `HTTP` sources:
without it the XR author would point the runtime at any host and read what
its answer says), and restrict it to signed code with `--cosign-key`. A source read from the XR may name a step credential only
when `policy.credentialsAllowList` lists it, and only for a repository
`policy.repositoryAllowList` admits: otherwise the XR author would pick the
registry host the secret is sent to, and a registry that answers with a
`Basic` challenge receives it — without such a policy an XR-chosen module is
pulled with the runtime's own Docker config (mount one through a
`DeploymentRuntimeConfig` and set `DOCKER_CONFIG`; credentials there are
bound to their registry host) or anonymously. `policy`, `limits` and
`sandbox` are read from the Input only, so an XR author can choose code, not
widen its permissions, grants or budget; every one of these rules is
enforced by the runtime on every request (Crossplane never installs the
Input CRD), and [`function validate`](#validate-a-composition) runs the
same code over a Composition offline. The sandbox protects the runtime
process — and with it every other Composition sharing the Function — from a
crashing, looping or memory-hungry module, and gives a module no filesystem,
environment or network beyond what the Composition granted within the
operator's `--enable-sandbox-*` flags: a private `/tmp` that exists for one
request (host directories are never mountable — the request is a module's
only view of the world beyond what it writes for itself), listed
environment variables (non-secret
by convention — the values are visible in the Composition), and HTTP
requests through the host to the hosts, methods and paths it lists within
the operator's policy (block list, budgets). A module granted egress can
send whatever its request carries — step credentials included — to those
hosts, which is why the grant is the Composition's alone (never readable
through `module.from`), every request leaves an audit line with the module
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

## Development

```shell
go build ./... && go vet ./... && go test -race ./...    # host, CLI, engine (needs a C compiler: wasmtime-go is CGo)
(cd pkg/wasmfn && go test ./... && GOOS=wasip1 GOARCH=wasm go vet ./...)
(cd examples/hello-go && go test ./...)
golangci-lint run ./...
go generate ./...                                        # Input CRD, guestfn scaffold golden
make -C examples/hello-go render-check                      # function validate + crossplane render through the real runtime
```

The root tests build `examples/hello-go` to WebAssembly and run it through the
host; `go test -short` skips that. See [AGENTS.md](AGENTS.md) for the layout
and conventions.

Design documents live under `docs/` as one-pagers: the implemented ones
(cache, module source schema, trust model, resource governance, sandbox)
and the drafts of what comes next — admission and inspection tooling, the
module manifest, the local loop, request-sourced secrets, governance and
performance phases, guest language support, a Nix development environment.
[AGENTS.md](AGENTS.md#key-reference-documents) lists them.
