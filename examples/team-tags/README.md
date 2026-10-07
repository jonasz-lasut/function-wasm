# team-tags

Who owns a resource and who pays for it, from the CMDB rather than from the
person who asked for it: a [Crossplane](https://crossplane.io) composition
function in Python, componentized with
[componentize-py](https://github.com/bytecodealliance/componentize-py) into
a WebAssembly **component** (ABI v2, about 21 MB with CPython embedded), run
by [function-wasm](https://github.com/jonasz-lasut/function-wasm).

A `Bucket` composite resource lives in its team's namespace and names a
region. The platform's first pipeline step,
[function-go-templating](https://github.com/crossplane-contrib/function-go-templating),
composes the S3 bucket; the module then reads the team's record from the
CMDB, `GET <cmdbUrl>/teams/<namespace>` with that team's token, and stamps
`team`, `owner` and `cost-center` tags on every managed resource composed
before it, keeping the tags already there. The record lands in
`status.cmdb`.

```yaml
apiVersion: storage.example.org/v1alpha1
kind: Bucket
metadata:
  name: invoices
  namespace: payments    # the team
spec:
  region: eu-central-1
  tags:                  # optional, the team's own
    environment: production
```

With the CMDB answering
`{"team":"payments","owner":"alice@example.com","costCenter":"cc-4711"}`,
the composed bucket carries `environment: production`, `team: payments`,
`owner: alice@example.com` and `cost-center: cc-4711`.

## Why a module

Tags every resource of a team must carry are a common ask: provider-upjet-aws
has no provider-level default tags
([crossplane-contrib/provider-upjet-aws#169](https://github.com/crossplane-contrib/provider-upjet-aws/issues/169)),
and data a composite resource must not set for itself has to come from
somewhere else at reconcile time
([crossplane/crossplane#2099](https://github.com/crossplane/crossplane/issues/2099)).
A Composition has no step that reads an authenticated API; a function can,
and here that function needs no package of its own, and no network beyond
the one request it declares:

- **One Secret holds every team's token, and the namespace picks the key.**
  The platform team keeps the CMDB tokens in one Secret, `cmdb-tokens`, one
  key per team namespace, and names it as the pipeline step's credential
  `cmdb`. The module reads the key named after the composite resource's
  namespace. Never a field of its spec: the composite resource's author
  writes the spec, so a team named there would let an author in one team's
  namespace spend another team's token - a confused deputy. In Crossplane
  v2 composite resources are namespaced and cluster RBAC decides who may
  create one where, so the namespace is the team. A step that does not
  pass the credential is refused before the module runs; no key for the
  namespace is a fatal result naming both.
- **The module sees the tokens only because it asks, and both policies
  agree.** The runtime forwards a module only the step credentials its
  manifest names, so `wasmfn.yaml` declares `requires.credentials: [cmdb]`,
  and the request carries `cmdb` only where the Composition's
  `compositionPolicy` and the operator's Cedar policy both permit
  `spendCredential` for it. Every other credential of the step - the one
  that pulls the module above all - is edited out of the request before the
  module runs. The tokens it does receive are every team's, though, so
  what keeps them in place is where it may send them: the module cannot
  open a socket, its manifest requests `GET` to `cmdb.example.org` under
  `/teams/`, and the runtime makes the request on its behalf only where the
  `compositionPolicy` and the operator's policy both permit it, after
  checking the resolved address against its block list, writing one audit
  line per request. The Composition, which hands the module the tokens,
  fences the credential, that host and that path in its own
  `compositionPolicy`, whatever a module's manifest asks for. Nothing but
  the module's code, pinned by digest, keeps a token out of the desired
  state it returns.
- **The CMDB is the record.** The composite resource's namespace names its
  team, never the team's owner or cost center: a value an earlier step set
  for one of the three keys gives way to the CMDB's, with a warning. The
  team is one path segment of the request, so it cannot point the token at
  another endpoint. An unknown team, a refused token or a failed request is
  a fatal result naming the team: Crossplane applies nothing from a fatal
  pipeline, so the resources keep the tags they had.

## Run it

The toolchain is `python3` alone: a venv with `componentize-py` and the
pure-Python `protobuf` runtime, both pinned in `requirements.txt` (the C
extension does not exist in wasm; the fallback engages by itself). A
render also needs the crossplane CLI and Docker, which runs
function-go-templating.

```shell
make test             # the function's unit tests, natively
make build            # fn.wasm, a component (makes the venv first)
make render           # serve it with function-wasm and crossplane render example/xr.yaml
make render-check     # the render test, example/xprin.yaml (needs xprin)
```

A local render needs no CMDB and no AWS account. `example/` holds:

- `definition.yaml`: the `Bucket` XRD (Crossplane v2, namespaced).
- `xr.yaml` (namespace `payments`) and `xr-own-tags.yaml` (namespace
  `search`, with tags of its own, one of them an `owner` the CMDB's
  replaces).
- `composition.yaml`: function-go-templating's bucket step, then the
  module, served from this directory under `example/wasmfn.local.yaml`, a
  manifest that asks for the fixture server instead of `cmdb.example.org`;
  the step names the credential `cmdb`, and its `compositionPolicy` permits
  the module that credential and fences the fixture host and path.
- `function-credentials.yaml`: `cmdb-tokens`, the shared Secret, with a
  fake token per namespace.
- `fixtures/teams/`: the two teams' records, which `../render.sh` serves on
  `127.0.0.1:9480`. The fixture server does not check the token; the unit
  tests assert which one goes out, and in which header.
- `policy.cedar`: the operator policy the runtime runs under, granting the
  credential `cmdb` and the fixture host and permitting loopback, which the
  egress block list refuses by default.
- `crds/`: provider-upjet-aws's namespaced S3 `Bucket` CRD, vendored, so
  the render test validates the composed bucket against the provider's own
  schema.
- `xprin.yaml`: the render test, one case per XR.

## In a cluster

Publish the module with its manifest, naming your CMDB's host in
`wasmfn.yaml` first, then reference the digest `guestfn push` prints:

```shell
guestfn push ghcr.io/example/team-tags:v0.1.0
```

The tokens, one key per team namespace:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: cmdb-tokens
  namespace: crossplane-system
stringData:
  payments: <the payments team's CMDB token>
  search: <the search team's CMDB token>
```

```yaml
  - step: team-tags
    functionRef:
      name: function-wasm
    credentials:
    - name: registry               # pulls the module, which never sees it
      source: Secret
      secretRef: {namespace: crossplane-system, name: ghcr-pull}
    - name: cmdb                   # every team's CMDB token
      source: Secret
      secretRef: {namespace: crossplane-system, name: cmdb-tokens}
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module:
        type: OCI
        oci:
          ref: ghcr.io/example/team-tags:v0.1.0@sha256:…
          credentials: registry
      compositionPolicy: |
        permit (principal, action == Action::"spendCredential", resource == Credential::"cmdb");
        permit (principal, action == Action::"grantEgress", resource == HostPattern::"cmdb.example.org")
        when { context.method == "GET" && context.path like "/teams/*" };
      config:
        cmdbUrl: https://cmdb.example.org
```

The operator grants the credential and the request in the runtime's
`--sandbox-policy-file`:

```cedar
permit (principal, action == Action::"spendCredential", resource == Credential::"cmdb");

permit (principal, action == Action::"grantEgress", resource == HostPattern::"cmdb.example.org")
when { context.method == "GET" && context.path like "/teams/*" };
```

`function validate --resolve` (which `make render` runs first) reports
`credentials: cmdb` for the step: what the module's request carries.

The tagged bucket needs a
[provider-upjet-aws](https://github.com/crossplane-contrib/provider-upjet-aws)
v2 with its namespaced `s3.aws.m.upbound.io` resources.

## Layout

- `src/fn.py`: `run_function` over the protobuf messages - the token by
  namespace, the lookup, the tag merge and the status - ordinary Python,
  natively testable with a CMDB double; `test/test_fn.py` covers it, every
  fatal path included.
- `src/app.py` (wasm only): the world wiring - the `run` export, the typed
  log adapter, and the CMDB request over `wasi:http@0.2`'s
  `outgoing-handler` on componentize-py's poll loop.
- `wit/world.wit` is this guest's own world, in a `local:guest` package:
  the [ABI v2 world](https://github.com/jonasz-lasut/function-wasm/blob/main/docs/abi-v2.md)
  restated with `run` declared **sync** (componentize-py takes the sync
  shape, and a sync-lifted function satisfies the runtime's async world),
  plus the `wasi:http` import the request goes through.
- `proto/run_function.proto`, `src/gen/run_function_pb2.py` and
  `wit/deps/` are the python scaffold's plumbing (`guestfn init --lang
  python`), kept identical to it: crossplane's `RunFunction` contract,
  protoc's Python codec for it (`make gen-proto` redoes it; the `protobuf`
  runtime refuses code from a newer protoc) and the wasi 0.2 WIT the
  `wasi:http` import needs.
