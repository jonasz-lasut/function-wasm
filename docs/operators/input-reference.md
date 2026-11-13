# Input reference

This page is for composition authors writing a function-wasm step and for
operators who need to predict what the runtime admits. It lists every field
of the step's Input and what the runtime enforces for each. Crossplane never
installs the Input's CRD, so the runtime is the only enforcement: it checks
every rule on every request, and `function validate` applies the same checks
offline. The CRD under `package/wasm/input/` is for tooling and IDEs only, and
the runtime generates it from its own types.

The Input is `apiVersion: guestfn.crossplane.io/v1`, `kind: Input`. A step
that still says `wasm.fn.crossplane.io/v1` or `v1beta1`, the group of 0.6
and earlier with the same fields, is refused with a sentence that states
the new group ([Compatibility](../contract/compatibility.md)).

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
config:                        # optional; the module's configuration, read from the XR
  from: spec.config            # the observed XR field holding it, an object
  allowEmpty: true             # optional: an unset field runs the module unconfigured
```

Everything but the source and the configuration is read from the Input:
`compositionPolicy` and `limits` are the Composition's, never the composite
resource's. The Input
has no `sandbox` block. Capabilities come from the module's manifest and
two Cedar layers, described under [Capabilities](#capabilities).

The fields are:

| field | type | description |
|---|---|---|
| `module` | object | Required. Where the module comes from |
| `module.type` | string | Required. `OCI`, `HTTP` or `Path`. Exactly one of the object it names (`oci`, `http`, `path`) or `module.from` is set, and no object of another type may be present. Under `function-oci` only `OCI`: an OCI module is an image pulled from a registry, never a file or a URL |
| `module.oci.ref` | string | OCI artifact reference pinned to its manifest digest, `registry/repo@sha256:…`, as `guestfn push` prints it. See [OCI references](#oci-references) |
| `module.oci.credentials` | string | name of the pipeline-step credential used to pull. See [Pull credentials](#pull-credentials) |
| `module.http.url` | string | the HTTP or HTTPS URL the module is downloaded from |
| `module.http.digest` | string | Required. `sha256:<hex>` of the module, which the runtime verifies the download against. A digest the runtime already caches is served without a download |
| `module.http.manifestURL` | string | Optional. A `guestfn.yaml` served beside the module. Set with `module.http.manifestDigest`. Without it an HTTP source has no manifest and gets only the default sandbox. For a `module.from` `http` source, `compositionPolicy` `pullModule` fences it like the module URL |
| `module.http.manifestDigest` | string | `sha256:<hex>` of the manifest, verified against it. Required with `module.http.manifestURL`, refused without it |
| `module.path` | string | a file relative to the runtime's `--module-dir`. Refused unless that flag is set. A `Path` source has no digest |
| `module.manifestPath` | string | Optional, `type: Path` only. A `guestfn.yaml` under `--module-dir`, the manifest of a local or volume-mounted module. Never set through `module.from`, and re-read on each request. A local edit takes effect without a restart |
| `module.from` | string | a field of the observed composite resource, under `spec.` or `status.`, that holds the source. See [Modules chosen by the XR](#modules-chosen-by-the-xr) |
| `module.allowEmpty` | boolean | Optional, with `module.from` only. See [Skipping a step](#skipping-a-step) |
| `compositionPolicy` | string | the composition author's Cedar layer. See [The composition policy](#the-composition-policy) |
| `limits.timeout` | duration | compute budget of one run, e.g. `5s`. See [Limits](#limits) |
| `limits.memory` | quantity | memory a run may use, its linear memories and tables together, e.g. `128Mi`. See [Limits](#limits) |
| `limits.concurrency` | int32 | at most N runs of this step at once. See [Limits](#limits) |
| `config` | object | Optional. Where the module's configuration comes from. See [Configuration from the XR](#configuration-from-the-xr) |
| `config.from` | string | Required within `config`. A field of the observed composite resource, under `spec.` or `status.`, that holds the configuration as an object |
| `config.allowEmpty` | boolean | Optional, with `config.from`. An unset field (absent or `null`) runs the module unconfigured instead of a fatal result |

## OCI references

`module.oci.ref` must include a manifest digest. A tag alone is refused, and
the runtime doesn't resolve tags at request time. With
`registry/repo:tag@sha256:…` only the digest is fetched. The manifest states
its layer's digest, and the runtime verifies both on fetch.

The runtime reads the registry part into one canonical form, and the
policies, the signature check and every message use that form. The host is
lowercase and has no trailing dot. The default port goes, so `ghcr.io:443` is
`ghcr.io`, and any other port stays. Docker Hub is `index.docker.io`, also when
the reference says `docker.io`. The runtime refuses a registry part with user
information, or one that isn't a host name or an IP address with an optional
port. [The canonical registry](cedar-policy.md#the-canonical-registry) has
the rules.

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
    apiVersion: guestfn.crossplane.io/v1
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
      apiVersion: guestfn.crossplane.io/v1
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

## Configuration from the XR

The module's configuration is a field of the composite resource, which the
step names in `config.from`. The module's author publishes its structure as
the manifest's `config.schema`; the composite resource definition declares
the field, with that schema transplanted, so the API server checks and
defaults the values; the runtime reads the field on every request, holds it
against the manifest's schema as the backstop and hands it to the module as
`input.config`, where every guest reads its configuration. A module chosen
through `module.from` is configured by the same author who chose it, and a
fixed module exposes what the definition declares. Secrets don't belong
there: a module reads a step credential through its manifest's
`requires.env` or `requires.credentials`.

```yaml
    input:
      apiVersion: guestfn.crossplane.io/v1
      kind: Input
      module:
        type: OCI
        from: spec.addOn
      config:
        from: spec.addOnConfig      # spec.addOnConfig: {maxUnavailable: 2}
        allowEmpty: true            # a WebApp that sets none runs the add-on unconfigured
```

The field's value must be an object. A step without `config` runs the
module unconfigured. A value written under `config` itself, as 0.6 took it, is
refused at admission with a sentence that states where it belongs:

```text
config.greeting is not a field of config, which has from and allowEmpty only: the module's configuration is written in the composite resource, in the field config.from names
```

A request without an observed composite resource, a field the resource
doesn't have or a value that isn't an object is a fatal result worded as
`module.from`'s:

```text
config.from spec.addOnConfig: no observed composite resource to read it from
config.from: cannot read spec.addOnConfig from the composite resource: addOnConfig: no such field
config.from: spec.addOnConfig of the composite resource is not an object
```

`config.allowEmpty: true` makes an unset field (absent or `null`) run the
module unconfigured, the state of a step without `config`; a scalar or a
list is refused either way. `function validate --xr` reads the field from the
composite resource given and prints the same sentences; without `--xr` it
reports the reference and leaves the value unchecked.

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
two layers may fence what the module composes with `composeResource` and what
it asks Crossplane for with `requireResource`: see the
[output fence](cedar-policy.md#the-output-fence) and the
[read fence](cedar-policy.md#the-read-fence). Egress is covered on
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
