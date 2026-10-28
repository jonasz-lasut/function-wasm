# Input reference

This page is for composition authors writing a function-wasm step and for
operators who need to predict what the runtime admits. It lists every field
of the step's Input and what the runtime enforces for each. Crossplane never
installs the Input's CRD, so the runtime is the only enforcement: it checks
every rule on every request, and `function validate` applies the same checks
offline. The CRD under `package/input/` is for tooling and IDEs only.

The Input is `apiVersion: wasm.fn.crossplane.io/v1`, `kind: Input`. A step
that still says `wasm.fn.crossplane.io/v1beta1` (the same fields) is accepted
throughout 1.x: `function validate` prints a warning under it, the runtime
logs one line per request naming `v1`, and 2.0.0 removes it
([Compatibility](../contract/compatibility.md)).

```yaml
module:                        # required
  type: OCI                    # required: OCI | HTTP | Path - the Composition's choice
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
`limits` are the Composition's, never the composite resource's. The Input
has no `sandbox` block. Capabilities come from the module's manifest and
two Cedar layers, described under [Capabilities](#capabilities).

The fields are:

| field | type | description |
|---|---|---|
| `module` | object | Required. Where the module comes from |
| `module.type` | string | Required. `OCI`, `HTTP` or `Path`. Exactly one of the object it names (`oci`, `http`, `path`) or `module.from` is set, and no object of another type may be present |
| `module.oci.ref` | string | OCI artifact reference pinned to its manifest digest, `registry/repo@sha256:…`, as `guestfn push` prints it. See [OCI references](#oci-references) |
| `module.oci.credentials` | string | name of the pipeline-step credential used to pull. See [Pull credentials](#pull-credentials) |
| `module.http.url` | string | the HTTP or HTTPS URL the module is downloaded from |
| `module.http.digest` | string | Required. `sha256:<hex>` of the module, which the runtime verifies the download against. A digest the runtime already caches is served without a download |
| `module.http.manifestURL` | string | Optional. A `wasmfn.yaml` served beside the module. Set with `module.http.manifestDigest`. Without it an HTTP source has no manifest and gets only the default sandbox. For a `module.from` `http` source, `compositionPolicy` `pullModule` fences it like the module URL |
| `module.http.manifestDigest` | string | `sha256:<hex>` of the manifest, verified against it. Required with `module.http.manifestURL`, refused without it |
| `module.path` | string | a file relative to the runtime's `--module-dir`. Refused unless that flag is set. A `Path` source has no digest |
| `module.manifestPath` | string | Optional, `type: Path` only. A `wasmfn.yaml` under `--module-dir`, the manifest of a local or volume-mounted module. Never set through `module.from`, and re-read on each request. A local edit takes effect without a restart |
| `module.from` | string | a field of the observed composite resource, under `spec.` or `status.`, that holds the source. See [Modules chosen by the XR](#modules-chosen-by-the-xr) |
| `module.allowEmpty` | boolean | Optional, with `module.from` only. See [Skipping a step](#skipping-a-step) |
| `compositionPolicy` | string | the composition author's Cedar layer. See [The composition policy](#the-composition-policy) |
| `limits.timeout` | duration | compute budget of one run, e.g. `5s`. See [Limits](#limits) |
| `limits.memory` | quantity | memory a run may use, each linear memory and each table against it, e.g. `128Mi`. See [Limits](#limits) |
| `limits.concurrency` | int32 | at most N runs of this step at once. See [Limits](#limits) |
| `config` | object | opaque, passed to the module untouched inside the request input. Non-secret module configuration belongs here: the module's environment comes only from its manifest's `requires.env` credential bindings |

## OCI references

`module.oci.ref` must include a manifest digest. A tag alone is refused, and
the runtime doesn't resolve tags at request time. With
`registry/repo:tag@sha256:…` only the digest is fetched. The manifest states
its layer's digest, and the runtime verifies both on fetch.

The runtime takes the first layer whose media type is `application/wasm`,
`application/vnd.wasm.content.layer.v1+wasm` or
`application/vnd.module.wasm.content.layer.v1+wasm`. Failing that, it takes
the only layer that isn't the manifest layer. A tar layer (a `FROM scratch`
image) must hold the module at exactly `/fn.wasm`.

## Pull credentials

`module.oci.credentials` names a pipeline-step credential: a Secret with
`.dockerconfigjson`, or with `username` and `password` keys. It's declared
on the step:

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

Without `credentials`, the runtime tries its Docker config (`DOCKER_CONFIG`),
then anonymous access. The module never receives the pull credential, and a
manifest that names it as an env binding source or a required credential is
refused.

## Modules chosen by the XR

With `module.from`, each composite resource chooses its module. The
Composition states the type and the XR field, and the field contains the source:
an object `{ref, credentials}` for `OCI`, `{url, digest}` for `HTTP`, a
string for `Path`. The runtime reads it on every request and rejects unknown fields. A typo or a wrong structure is a fatal result that states the
field. An unset field is a fatal result naming it, unless `module.allowEmpty`
says otherwise.

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

A source chosen this way is fenced by the `compositionPolicy`, which is
required for `OCI` and `HTTP` sources. See
[The composition policy](#the-composition-policy).

### Skipping a step

`module.allowEmpty: true` lets the composite resource leave the field unset
(absent, or `null`). The step then runs nothing and returns the request's
desired state and context unchanged. The pipeline continues. A set field
is handled as any static source is. The Input alone decides this, so an XR
can leave a step empty only where the Composition allows it, and
`allowEmpty` without `module.from` is refused. The runtime logs a skipped
step and counts it as `requests_total{outcome="skipped"}`.

### Spending the pull credential

An XR-chosen module may specify a pull credential only where the
`compositionPolicy` permits `spendCredential` for it, and only for a
repository that a `pullModule` permit admits. The pull check runs first. The
`spendCredential` rule tests `context has repository`, because the context
carries `repository` only for this pull credential:

```yaml
    module:
      type: OCI
      from: status.module           # status.module: {ref: ghcr.io/example-org/…@sha256:…, credentials: registry}
    compositionPolicy: |
      permit (principal, action == Action::"pullModule",
              resource in Repository::"ghcr.io/example-org");
      permit (principal, action == Action::"spendCredential",
              resource == Credential::"registry")
      when { context has repository &&
             context.repository in Repository::"ghcr.io/example-org" };
```

## The composition policy

`compositionPolicy` is Cedar text. Malformed Cedar is a fatal result at admission. It's required whenever
`module.from` names an `OCI` or `HTTP` source: without it the XR author could
point the runtime at any host.

For a `module.from` source, a `pullModule` permit must match the module's
normalized location, and a step credential may be spent only where a
`spendCredential` permit matches. For sandbox actions the layer can only
narrow what the manifest and the operator grant. The actions, entities,
evaluation rules and refusal sentences are in the
[Cedar policy reference](cedar-policy.md).

## Limits

`limits` can only narrow what the runtime allows, never widen it.

```yaml
    limits:
      timeout: 5s        # ≤ --module-timeout
      memory: 128Mi      # ≤ --module-memory-limit
```

- `limits.timeout` is at most `--module-timeout`, otherwise a fatal result
  states both: `limits.timeout 1m0s exceeds the runtime's --module-timeout of 30s`.
  Time spent waiting on the host's HTTP answers is credited back (see
  [HTTP egress](http-egress.md)), and the request's own gRPC deadline still
  caps the run if shorter.
- `limits.memory` is at most `--module-memory-limit`, otherwise a fatal result
  states both: `limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`.
- `limits.concurrency` caps runs of this step at once, across all requests,
  keyed by the module's content digest. A further request waits under its own
  deadline. If the deadline passes first, the result is fatal and isn't
  counted as a run. A value over `--max-concurrent-runs` is capped to it
  without an error.

## Capabilities

A module declares the capabilities it can't run without in its manifest, as
`requires` ([Module manifests](../guests/module-manifests.md)). The runtime
grants each one only when the Input's `compositionPolicy` and the operator's
`--sandbox-policy-file` both permit it
([three-layer authorization](../one-pager-three-layer-authz.md)). The same
two layers may fence what the module composes with `composeResource`: see the
[output fence](cedar-policy.md#the-output-fence). Egress is covered on
[HTTP egress](http-egress.md).

### Step credentials

Of the step's credentials, the request a module receives contains only those
its manifest names and both policy layers permit with `spendCredential`: the
credential a `requires.env` binding reads, or one it reads whole with
`requires.credentials`. The runtime edits every other credential out before
the module runs, so a module with no manifest sees none.

A required credential that the step doesn't declare is a fatal result before
the run: `… requires.credentials[0]: the request carries no
credential "cmdb"; declare it on the pipeline step`.

### Private `/tmp` and environment

The private `/tmp` needs `usePrivateTmp` permitted in both Cedar layers. An
env binding needs `setEnv` and `spendCredential` in both. A required
capability that either layer doesn't permit, or any on a runtime with no
`--sandbox-policy-file`, is a fatal result before the module runs. The
manifest syntax is on [Module manifests](../guests/module-manifests.md).
