# Input reference

`apiVersion: wasm.fn.crossplane.io/v1`, `kind: Input`. A step that still
says `wasm.fn.crossplane.io/v1beta1` (the same fields) is accepted
throughout 1.x: `function validate` prints a warning under it and the
runtime logs one line per request naming `v1`; 2.0.0 removes it
([Compatibility](../contract/compatibility.md)).

```yaml
module:                        # required
  type: OCI                    # required: OCI | HTTP | Path — the Composition's choice
  oci:  {ref, credentials}     # exactly one of the object matching type …
  http: {url, digest}
  path: fn.wasm
  from: status.module          # … or the observed XR field holding it
  allowEmpty: true             # optional, with from: an unset field runs nothing
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
in its manifest (`requires` - see [Module manifests](../guests/module-manifests.md)),
and each is granted only when the Input's `compositionPolicy` and the
operator's Cedar `--sandbox-policy-file` both permit it
([docs/one-pager-three-layer-authz.md](../one-pager-three-layer-authz.md)).

| field | type | description |
|---|---|---|
| `module` | object | **required** — where the module comes from |
| `module.type` | string | **required** — `OCI`, `HTTP` or `Path`. Exactly one of the object it names (`oci`, `http`, `path`) or `module.from` is set, and no object of another type may be present. The runtime checks it on every request — the Input's CRD (with the same rules as CEL) is never installed by Crossplane; a function input is part of a Composition, not an object, so its schema only serves tooling that validates against it (`crossplane resource validate` with the package's `input/` directory) and IDEs |
| `module.oci.ref` | string | OCI artifact reference **pinned to its manifest digest**, `registry/repo@sha256:…`, as `guestfn push` prints it. The manifest digest pins the module (the manifest states its layer's digest; both are verified on fetch) and addresses the caches. A tag alone is not accepted (tags can be moved; the runtime resolves nothing at request time); `registry/repo:tag@sha256:…` is fine — the digest is what is fetched, the tag is human-readable context and may even no longer exist. The module is the `application/wasm` (or `vnd.wasm` content) layer, or the only layer; a tar layer (a `FROM scratch` image) must hold it at exactly `/fn.wasm` |
| `module.oci.credentials` | string | name of a pipeline-step credential (a Secret with `.dockerconfigjson`, or `username` and `password` keys) used to pull; it never reaches the module. Without it the runtime's Docker config (`DOCKER_CONFIG`) and anonymous access are tried. An object read through `module.from` may name one only where the `compositionPolicy` permits `spendCredential` for it on the ref's repository |
| `module.http.url` | string | download the module over HTTP(S) |
| `module.http.digest` | string | **required** — `sha256:<hex>` of the module; the download is verified against it |
| `module.http.manifestURL` | string | *optional* — a `wasmfn.yaml` served beside the module, its request layer (the three-layer model's manifest for a source that carries no OCI layer). Set with `module.http.manifestDigest`; without it an HTTP source carries no manifest and gets only the default sandbox. For a `module.from` http source it is fenced by `compositionPolicy` `pullModule` like the module URL |
| `module.http.manifestDigest` | string | `sha256:<hex>` of the manifest, verified against it; **required with** `module.http.manifestURL`, refused without it |
| `module.path` | string | a file relative to the runtime's `--module-dir`; refused unless that flag is set — local rendering and volume-mounted modules; carries no digest |
| `module.manifestPath` | string | *optional*, `type: Path` only — a `wasmfn.yaml` under `--module-dir`, the request layer for a Path module, so a local or volume-mounted module can declare the capabilities it needs. Read from the Input only (never through `module.from`) and re-read each request, so a local edit takes effect without a restart |
| `module.from` | string | a field of the observed composite resource, under `spec.` or `status.`, holding the source `module.type` names — an object `{ref, credentials}` for `OCI`, `{url, digest}` for `HTTP`, a string for `Path` — e.g. `status.module`; read on every request and decoded strictly (a typo or a wrong shape is a fatal result naming the field), so each XR can choose its module. What it may choose is fenced by `compositionPolicy` (`pullModule`, default-deny). A field the XR leaves unset is a fatal result naming it, unless `module.allowEmpty` says otherwise |
| `module.allowEmpty` | bool | *optional*, with `module.from` only - `true` lets the composite resource leave the field unset (absent, or `null`): the step then runs no module and returns the request's desired state and context unchanged, a no-op the pipeline continues past, instead of a fatal result. This is the reserved hook a platform team ships before any tenant fills it in, and the smoke test of a runtime with no module published yet. A field that is set is read, fenced and run exactly as without it. Read from the Input only, so an XR may leave a step empty only where the Composition allows it; refused without `module.from`. A skipped step is logged and counted as `requests_total{outcome="skipped"}` |
| `compositionPolicy` | string | the composition author's own Cedar policy layer, over the same schema as the operator's `--sandbox-policy-file` (actions `pullModule`, `spendCredential`, `grantEgress`, `usePrivateTmp`, `setEnv`; a `Request` principal carrying `namespace` and `xrKind`; `Repository`, `HostPattern`, `Capability` and `Credential` entities). AND-combined with the module's manifest and the operator's policy, so it can only narrow. Two regimes: a sandbox action it scopes no rule for is not narrowed (the operator and the manifest decide alone), while a module chosen through `module.from` is refused unless a `pullModule` permit matches its normalized location - matched over a boundary-correct `Repository` hierarchy, so `Repository::"ghcr.io/example-org"` admits `ghcr.io/example-org/mod` but never the sibling namespace `ghcr.io/example-org-other/...` - and may spend a step credential only where a `spendCredential` permit matches (`context.repository` carries the ref's location). **Required whenever `module.from` names an `OCI` or `HTTP` source**: an unfenced XR author could point the runtime at any host and read what its answer says. Read from the Input only; malformed Cedar is a fatal result at admission. The schema both layers share and how each decides are in the [Cedar policy reference](cedar-policy.md) |
| `limits.timeout` | duration | compute budget of one run, e.g. `5s`; at most `--module-timeout`, else a fatal result naming both (`limits.timeout 1m0s exceeds the runtime's --module-timeout of 30s`). Time the run spends waiting on the host's HTTP answers (`wasi:http`) is credited back, so a slow upstream does not spend the budget; the request's own gRPC deadline is the hard wall-clock cap and still applies if shorter |
| `limits.memory` | quantity | linear memory a run may use, e.g. `128Mi`; at most `--module-memory-limit`, else a fatal result naming both (`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`) |
| `limits.concurrency` | int32 | at most N runs of this step at once, across all requests, keyed by the module's content digest. A further request waits under its own context; when the deadline passes first, it is a fatal result that consumed nothing and is not counted as a run. A value above `--max-concurrent-runs` is silently capped. No ceiling flag: this only narrows |
| `config` | object | opaque, passed to the module untouched inside the request input; a Go guest reads it with `wasmfn.GetConfig`. Non-secret module configuration belongs here - the module's environment comes only from its manifest's `requires.env` credential bindings |

What a module gets of the sandbox, and which step credentials it sees, is
not an Input field: its manifest's `requires` (egress rules,
`filesystem.privateTmp`, `env` credential bindings, `credentials` read from
the request) is the request, and each requested capability is granted only
when the `compositionPolicy` and the operator's `--sandbox-policy-file`
both permit it - see [Module manifests](../guests/module-manifests.md) and
[HTTP egress](http-egress.md).

Letting each composite resource choose its module — the Composition names
the type and the XR field, the field holds the source, the
`compositionPolicy` says what it may hold:

```yaml
    input:
      apiVersion: wasm.fn.crossplane.io/v1
      kind: Input
      module:
        type: OCI
        from: spec.module           # spec.module: {ref: ghcr.io/example-org/greeter@sha256:…}
      compositionPolicy: |
        permit (principal, action == Action::"pullModule",
                resource in Repository::"ghcr.io/example-org");
```

Add `allowEmpty: true` under `module` to let a composite resource leave
the field unset: the step then runs nothing and returns the desired state
unchanged, so a Composition can reserve a hook before any module exists to
fill it. Without it, an unset field is a fatal result naming it.

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
    apiVersion: wasm.fn.crossplane.io/v1
    kind: Input
    module:
      type: OCI
      oci:
        ref: ghcr.io/example/private-fn@sha256:…
        credentials: registry
```

The pull credential never reaches the module. Of the step's other
credentials, the request the module receives carries only those its
manifest names - the credential a `requires.env` binding reads, or one it
reads whole from the request (`requires.credentials`, below) - and both
policy layers permit (`spendCredential`); the runtime edits every other one
out of the request before the module runs, so a module with no manifest,
or one requiring no credential, sees none. A credential the manifest
requires that the step does not declare is a fatal result before the run,
like an env binding's (`… requires.credentials[0]: the request carries no
credential "cmdb"; declare it on the pipeline step`). The Go runtime
forwarded every step credential but the pull credential: a module that
read one it did not declare must now declare it.

An XR-chosen module may spend the pull credential only if the
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
scratches in `/tmp`, reads `$DATABASE_URL` and picks a key of the step
credential `cmdb` out of its request declares, in its `wasmfn.yaml`:

```yaml
requires:
  filesystem: {privateTmp: true}
  env:
  - name: DATABASE_URL
    fromCredential:
      name: db                               # step credential "db", key "url"
      key: url
  credentials: [cmdb]                        # step credential "cmdb", whole, in the request
```

The private `/tmp` is granted where both Cedar layers permit
`usePrivateTmp`; an env binding needs `setEnv` and `spendCredential` in
both, and a credential read from the request `spendCredential` in both
(the composition layer sees no `context.repository` for either). Its
request then carries `db` and `cmdb`, whole, and no other step credential.
A requirement either layer does not permit (or any requirement on a
runtime with no `--sandbox-policy-file`) is a fatal result; the module
never runs. Non-secret configuration (`LOG_LEVEL: debug`) is not env - put
it in `config`, which the guest reads with `wasmfn.GetConfig`. The pull
credential (`module.oci.credentials`) is refused as a binding source and as
a required credential: the module must never see the secret that fetched
it. Host directories are never mountable into a module, whatever the
policy.
