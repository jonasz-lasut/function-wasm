# Module source schema

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 2.2

The structure of the Input before `v0.1.0` freezes it - a discriminated
`module` instead of six sibling fields, with top-level siblings owned by
the Composition. The 2026-08-16 architecture review raised it. Revision 1.0
is what was released, revision 1.1 recorded the `sandbox` grants that were
added, and revision 2.0 records the three-layer authorization change
(docs/one-pager-three-layer-authz.md): `policy` and `sandbox` left the
Input, replaced by `compositionPolicy` (raw Cedar) and the module
manifest's `requires`. Revision 2.1 adds a manifest by reference for
manifest-less sources (`http.manifestURL`/`manifestDigest` and
`manifestPath`, in docs/one-pager-manifest-less-sources.md). Revision 2.2
adds `module.allowEmpty` (issue #82): a `from` field the composite
resource leaves unset makes the step a no-op instead of a fatal result.
This document is the authoritative structure and lists every Input field.
The trust-model one-pager describes what the `compositionPolicy` fence
means, and the resource-governance one-pager `limits`. The sandbox
one-pager describes the capabilities a manifest may request.

## The Input, in one place

```yaml
apiVersion: wasm.fn.crossplane.io/v1beta1
kind: Input
module:                                   # what runs - required
  type: OCI                               # OCI | HTTP | Path - always the Composition's choice
  oci:                                    # exactly one of the typed object …
    ref: ghcr.io/example/greeter:v1@sha256:…
    credentials: registry
  # http: {url, digest, manifestURL?, manifestDigest?}   # a manifest-less source may name its wasmfn.yaml
  # path: fn.wasm                          # with, optionally, manifestPath: fn-manifest.yaml (under --module-dir)
  # from: status.module                   # … or the XR field holding it (an object of `type`)
  # allowEmpty: true                      # with from: an unset field runs nothing (desired state unchanged)
compositionPolicy: |                      # the composition author's Cedar layer (three-layer-authz one-pager)
  permit (principal, action == Action::"pullModule",
          resource in Repository::"ghcr.io/example-org");
limits:                                   # what this step's run may consume, ≤ the runtime's ceilings
  timeout: 5s
  memory: 128Mi
config:                                   # opaque, forwarded to the module
  greeting: hi
```

`module` is required; `compositionPolicy`, `limits` and `config` are
optional. Everything but `module` (and within it, the object `from` names)
is read from the Input only - never through `module.from`: an XR author may
choose the module but may not widen its permissions, grants or budget.
The Input has no `sandbox` subtree: what a module may do beyond the default
sandbox is its manifest's `requires`, decided by the three AND-combined
layers (manifest ∧ `compositionPolicy` ∧ the operator's
`--sandbox-policy-file`).

## Today

The preceding structure is implemented in `input/v1beta1/input.go`, enforced by
`internal/module.Validate`/`FromComposite`, `internal/module/policy.go`
(the composition-layer fence), `internal/admission` (`runOptions`,
`CompileCompositionPolicy`), and described by the generated CRD under
`package/input/`.

- `module.type` is required (`OCI`, `HTTP`, `Path`). Exactly one of the
  typed object it names (`oci`, `http`, `path`) or `from` is set, and no
  object of another type may be present. The runtime checks it on every
  request; three CEL rules on the `module` object say the same for tooling
  that reads the schema (`crossplane resource validate`, IDEs) -
  `self.type == 'OCI' ? (has(self.oci) != has(self.from)) : !has(self.oci)`
  and its two siblings. Crossplane itself never installs a function's Input
  CRD (an Input is a fragment of a Composition rather than an object), which
  makes the runtime the only gate that always runs, and every marker on the
  types is mirrored in `Validate`/`ValidateFrom`/the admission checks.
- `module.from` names a field of the observed composite resource under
  `spec.` or `status.` (pattern-checked), read on every request and decoded
  (unknown fields refused) into the object `type` names - an
  `{ref, credentials}` object for `OCI`, `{url, digest}` for `HTTP`, a
  string for `Path` - then validated like a static source. An error identifies
  the field: `module.from: status.module of the composite resource is not a
  {ref, credentials} object: …`. A field the composite resource hasn't set
  is an error naming it (`cannot read status.module from the composite
  resource: module: no such field`) - unless `module.allowEmpty: true`, in
  which case the step doesn't run a module and returns the request's desired
  state and context unchanged (the SDK's `response.To` shape, built at the
  wire level from the caller's own bytes), logged and counted as
  `requests_total{outcome="skipped"}`. `allowEmpty` is read from the Input
  only and refused without `from`: the Composition author chooses whether a
  step may be left empty, the composite resource only whether that step is.
  A field that's set is read, fenced and run exactly as without it. This is
  the reserved hook of the first use case (`docs/use-cases.md`) before any
  tenant fills it in, and a smoke test of a deployed runtime that doesn't
  need a module built.
- `compositionPolicy` is raw Cedar over the same schema as the operator's
  policy, compiled at admission (content-hash cached; malformed Cedar is a
  fatal result). For an XR-chosen source it's a required fence,
  default-deny: the XR's `oci.ref` (or `http.url`), normalized, must be
  permitted by a `pullModule` rule over the boundary-correct `Repository`
  hierarchy; a named credential needs a `spendCredential` permit
  (`context.repository` contains the ref's location, so both halves
  co-locate). For sandbox actions it only narrows, and only when it scopes
  the action. A static source skips the fence: the Composition
  named it and is trusted with it.
- `limits.timeout` (a duration string, e.g. `5s`) and `limits.memory` (a
  quantity, e.g. `128Mi`) must each be at most the runtime's ceiling
  (`--module-timeout`, `--module-memory-limit`); asking for more is a fatal
  result that quotes both (`limits.memory 1Gi exceeds the runtime's
  --module-memory-limit of 512Mi`), asking for less narrows that run's
  epoch deadline and store limiter (`engine.RunOptions`, capped by the
  engine's `Config` whatever the caller passes).
- The sandbox isn't an Input subtree: a module's manifest contains
  `requires` (`filesystem.privateTmp`, `egress.http[] {host | hostPattern,
  methods, pathPrefix}`, `env[] {name, fromCredential}`), shape-checked by
  `internal/egress.ValidateRules` and `internal/sandbox.ValidateBindings`,
  and each `requires` entry is decided by `admission.AdmitRequires` under the
  `compositionPolicy` and the operator's `--sandbox-policy-file`
  (`usePrivateTmp`, `setEnv` ∧ `spendCredential`, `grantEgress` - also the
  host allowlist, with the CIDR rules `dialAddress`). Host directories
  aren't mountable, by design. The behaviour is the sandbox one-pager's.
- A manifest-less source may point to that manifest **by reference**
  (docs/one-pager-manifest-less-sources.md): `module.http.manifestURL` with
  `module.http.manifestDigest` (both set together, the manifest pinned like
  the module) names a `wasmfn.yaml` served beside the module.
  `module.manifestPath` (only with `type: Path`) names one under
  `--module-dir`. The runtime loads it as the request layer through the same
  `module.Ref.Manifest` seam an OCI manifest layer uses, so the three-layer
  decision is identical. For a `module.from` `http` source the `manifestURL` is
  fenced by `pullModule` like the module URL; `manifestPath` is read from the
  Input only.
- `guestfn push` prints the module block in this form (`type: OCI` +
  `oci.ref`); the scaffold templates and examples use `type: Path` for local
  rendering.

## Why this structure

The previous Input had six optional fields under `module` (`oci`, `http`,
`path`, `ociFrom`, `httpFrom`, `pathFrom`), an "exactly one" invariant, and
every source kind present twice - statically and as a `*From` pointer.
Rules that depend on *where* a source came from (an XR-chosen source may
not name credentials; a repository allowlist applies to XR-chosen sources
only) lived in code and prose, not in the schema. Adding a source kind added
two fields, and a policy field had no natural home.

The Crossplane idiom - a `type` discriminator next to a typed object - with
`from` as an orthogonal switch fixes those problems:

- `module.type` says what kind of source this step runs; the Composition
  author always chooses the kind, an XR author only the instance.
- Exactly one of the typed object or `from` is set, and it must match
  `type` - three CEL rules, all expressible.
- The Composition-owned rules are top-level siblings of `module`, never
  read from the XR. Originally `policy` (the allowlists) and `sandbox`;
  since the three-layer change the sibling is `compositionPolicy`, and the
  CRD's CEL requires it when `module.from` names an OCI or HTTP source.
- `limits` is the other top-level sibling - the resource-governance
  proposal (`timeout`, `memory`, each at most the runtime's ceiling).
- The runtime's `FromComposite` reads the XR field into the object
  `module.type` names; the credentials rule is "an object read through
  `from` may reference a credential only where the `compositionPolicy` permits
  `spendCredential`, and only for a ref a `pullModule` permit admits" -
  one rule, stated once, and structurally impossible to bypass from the XR.

## `compositionPolicy` - what an XR-chosen module may be and spend

Refusing credentials on XR-chosen modules is safe but blunt: a platform
team that wants tenants to pick modules *from the platform's private
registry* would otherwise have to mount a Docker config into the runtime.
`compositionPolicy` - read from the Input only, never through
`module.from` - fences what an XR-chosen module may be and spend:

```yaml
module:
  type: OCI
  from: status.module                       # the XR names {ref, credentials}
compositionPolicy: |
  permit (principal, action == Action::"pullModule",
          resource in Repository::"ghcr.io/example-org");
  permit (principal, action == Action::"spendCredential", resource == Credential::"registry")
  when { context.repository in Repository::"ghcr.io/example-org" };
```

The XR keeps choosing the artifact; the Composition keeps choosing where
from and with what. The same field serves signature-free deployments that
still need to fence tenants to a registry. The `Repository` hierarchy is
boundary-correct: `Repository::"ghcr.io/example-org"` admits
`ghcr.io/example-org/mod` but never the sibling namespace
`ghcr.io/example-org-other/…` (a trailing slash is optional and doesn't
change the boundary).

## `limits` - per-composition budgets

The ceilings are global - a trusted internal policy module and a tenant's
labelling module get the same 30 s / 512 MB. The sandbox one-pager's
principle applies (the operator sets the ceiling, the Composition requests
less), and so the Input has a top-level `limits`, read from the Input only
(never through `module.from`: an XR author must not widen a budget),
validated against the flags at request time and applied to that run's
store. Raising a limit past the ceiling remains operator-only. A
Composition author who sets them with little headroom gets fatal results
the operator didn't cause. The benefit is a shared runtime where the
loudest module can't spend the whole ceiling.

## Migration

None was needed for the discriminated structure: the API is `v1beta1` and
unreleased, and the change landed before `v0.1.0`. An Input in the old
form fails decoding (`ociFrom` and friends are unknown fields) or
validation (`module.type is required`); the fix is `type: OCI|HTTP|Path`
plus the matching object, or `type` + `from` for the former `*From`
fields. The three-layer change is breaking. An Input with the removed
`policy` or `sandbox` field is refused by `function validate` naming the
replacement (`the Input's policy field was removed: …`, `the Input's
sandbox field was removed: …`) - allowlists become `compositionPolicy`
Cedar, sandbox grants become the module manifest's `requires`.

## Alternatives considered

| option | verdict |
|---|---|
| keep the six fields, add a CEL rule that exactly one is set | works; policy fields would still have no home and each new kind adds two fields |
| `source: {kind, oci, http, path}` nested one level down | same as the chosen structure with an extra level; nothing gained |
| `type` + typed object, `from` inside the typed object (`oci: {from: status.module}`) | reads well for one kind, but `path: {from: …}` turns a string into an object and the credentials rule needs "if from is set" inside each object |
| `policy` and `limits` under `module` | ties Composition-owned policy to the thing an XR may choose; as top-level siblings they're visibly not part of what `from` reads |
| drop `*From` sources altogether | removes the whole class of XR-author decisions; loses the per-tenant use case `docs/use-cases.md` leads with |
