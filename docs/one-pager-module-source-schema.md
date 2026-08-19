# Module Source Schema

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 2.0

The shape of the Input before `v0.1.0` freezes it: a discriminated `module`
instead of six sibling fields, with top-level siblings owned by the
Composition. Raised by the 2026-08-16 architecture review; revision 1.0 is
what shipped, revision 1.1 recorded the `sandbox` grants that landed, and
revision 2.0 records the three-layer authorization change
(docs/one-pager-three-layer-authz.md): `policy` and `sandbox` left the
Input, replaced by `compositionPolicy` (raw Cedar) and the module
manifest's `requires`. This document is the authoritative shape and holds
every Input field: the trust-model one-pager describes what the
`compositionPolicy` fence means, the resource-governance one-pager
`limits`, the sandbox one-pager the capabilities a manifest may request.

## The Input, in one place

```yaml
apiVersion: wasm.fn.crossplane.io/v1beta1
kind: Input
module:                                   # what runs — required
  type: OCI                               # OCI | HTTP | Path — always the Composition's choice
  oci:                                    # exactly one of the typed object …
    ref: ghcr.io/example/greeter:v1@sha256:…
    credentials: registry
  # http: {url, digest}
  # path: fn.wasm
  # from: status.module                   # … or the XR field holding it (an object of `type`)
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
is read from the Input only — never through `module.from`: an XR author may
choose the module, not widen its permissions, its grants or its budget.
There is no `sandbox` subtree: what a module may do beyond the default
sandbox is its manifest's `requires`, decided by the three AND-combined
layers (manifest ∧ `compositionPolicy` ∧ the operator's
`--sandbox-policy-file`).

## Today

The shape above is implemented in `input/v1beta1/input.go`, enforced by
`internal/module.Validate`/`FromComposite`, `internal/module/policy.go`
(the composition-layer fence), `internal/admission` (`runOptions`,
`CompileCompositionPolicy`), and described by the generated CRD under
`package/input/`.

- `module.type` is required (`OCI`, `HTTP`, `Path`). Exactly one of the
  typed object it names (`oci`, `http`, `path`) or `from` is set, and no
  object of another type may be present. The runtime checks it on every
  request; three CEL rules on the `module` object say the same for tooling
  that reads the schema (`crossplane resource validate`, IDEs) —
  `self.type == 'OCI' ? (has(self.oci) != has(self.from)) : !has(self.oci)`
  and its two siblings. Crossplane itself never installs a function's Input
  CRD — an Input is a fragment of a Composition, not an object — so the
  runtime is the only gate that always runs, and every marker on the types
  is mirrored in `Validate`/`ValidateFrom`/the admission checks.
- `module.from` names a field of the observed composite resource under
  `spec.` or `status.` (pattern-checked), read on every request and decoded
  strictly (unknown fields refused) into the object `type` names — an
  `{ref, credentials}` object for `OCI`, `{url, digest}` for `HTTP`, a
  string for `Path` — then validated like a static source. Errors name the
  field: `module.from: status.module of the composite resource is not a
  {ref, credentials} object: …`.
- `compositionPolicy` is raw Cedar over the same schema as the operator's
  policy, compiled at admission (content-hash cached; malformed Cedar is a
  fatal result). For an XR-chosen source it is a required fence,
  default-deny: the XR's `oci.ref` (or `http.url`), normalized, must be
  permitted by a `pullModule` rule over the boundary-correct `Repository`
  hierarchy; a named credential needs a `spendCredential` permit
  (`context.repository` carries the ref's location, so both halves
  co-locate). For sandbox actions it only narrows, and only when it scopes
  the action. A static source never reaches the fence: the Composition
  named it and is trusted with it.
- `limits.timeout` (a duration string, e.g. `5s`) and `limits.memory` (a
  quantity, e.g. `128Mi`) must each be at most the runtime's ceiling
  (`--module-timeout`, `--module-memory-limit`); asking for more is a fatal
  result naming both (`limits.memory 1Gi exceeds the runtime's
  --module-memory-limit of 512Mi`), asking for less narrows that run's
  epoch deadline and store limiter (`engine.RunOptions`, capped by the
  engine's `Config` whatever the caller passes).
- The sandbox is not an Input subtree: a module's manifest carries
  `requires` (`filesystem.privateTmp`, `egress.http[] {host | hostPattern,
  methods, pathPrefix}`, `env[] {name, fromCredential}`), shape-checked by
  `internal/egress.ValidateRules` and `internal/sandbox.ValidateBindings`,
  and each requirement is decided by `admission.AdmitRequires` under the
  `compositionPolicy` and the operator's `--sandbox-policy-file`
  (`usePrivateTmp`, `setEnv` ∧ `spendCredential`, `grantEgress` - also the
  host allowlist, with the CIDR rules `dialAddress`). Host directories are
  deliberately not mountable. The behaviour is the sandbox one-pager's.
- `guestfn push` prints the module block in this shape (`type: OCI` +
  `oci.ref`); the scaffold templates and examples use `type: Path` for local
  rendering.

## Why this shape

The previous Input had six optional fields under `module` (`oci`, `http`,
`path`, `ociFrom`, `httpFrom`, `pathFrom`), an "exactly one" invariant, and
every source kind present twice — statically and as a `*From` pointer.
Rules that depend on *where* a source came from (an XR-chosen source may
not name credentials; a repository allowlist applies to XR-chosen sources
only) lived in code and prose, not in the schema. Adding a source kind added
two fields; a policy field had no natural home.

The Crossplane idiom — a `type` discriminator next to a typed object — with
`from` as an orthogonal switch fixes all of that:

- `module.type` says what kind of source this step runs; the Composition
  author always chooses the kind, an XR author only the instance.
- Exactly one of the typed object or `from` is set, and it must match
  `type` — three CEL rules, all expressible.
- The Composition-owned rules are top-level siblings of `module`, never
  read from the XR. Originally `policy` (the allowlists) and `sandbox`;
  since the three-layer change the sibling is `compositionPolicy`, and the
  CRD's CEL requires it when `module.from` names an OCI or HTTP source.
- `limits` is the other top-level sibling — the resource-governance
  proposal (`timeout`, `memory`, each at most the runtime's ceiling).
- The runtime's `FromComposite` reads the XR field into the object
  `module.type` names; the credentials rule is "an object read through
  `from` may name a credential only where the `compositionPolicy` permits
  `spendCredential`, and only for a ref a `pullModule` permit admits" —
  one rule, stated once, and structurally impossible to bypass from the XR.

## `compositionPolicy`: what an XR-chosen module may be and spend

Refusing credentials on XR-chosen modules is safe but blunt: a platform
team that wants tenants to pick modules *from the platform's private
registry* would otherwise have to mount a Docker config into the runtime.
`compositionPolicy` — read from the Input only, never through
`module.from` — fences what an XR-chosen module may be and spend:

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
still want to fence tenants to a registry. The `Repository` hierarchy is
boundary-correct: `Repository::"ghcr.io/example-org"` admits
`ghcr.io/example-org/mod` but never the sibling namespace
`ghcr.io/example-org-other/…` (a trailing slash is optional and does not
change the boundary).

## `limits`: per-Composition budgets

The ceilings are global — a trusted internal policy module and a tenant's
labelling module get the same 30 s / 512 MB. The sandbox one-pager's
principle applies — the operator sets the ceiling, the Composition asks for
less — so the Input has a top-level `limits`, read from the Input only
(never through `module.from`: an XR author must not widen a budget),
validated against the flags at request time and applied to that run's
store. Raising above the ceiling stays operator-only. A Composition author
who sets them tightly gets fatal results the operator did not cause; the
benefit is a shared runtime where the loudest module cannot spend the whole
ceiling.

## Migration

None was needed for the discriminated shape: the API is `v1beta1` and
unreleased, and the change landed before `v0.1.0`. An Input in the old
shape fails decoding (`ociFrom` and friends are unknown fields) or
validation (`module.type is required`); the fix is `type: OCI|HTTP|Path`
plus the matching object, or `type` + `from` for the former `*From`
fields. The three-layer change is breaking: an Input carrying the removed
`policy` or `sandbox` field is refused by `function validate` naming the
replacement (`the Input's policy field was removed: …`, `the Input's
sandbox field was removed: …`) - allowlists become `compositionPolicy`
Cedar, sandbox grants become the module manifest's `requires`.

## Alternatives considered

| option | verdict |
|---|---|
| keep the six fields, add CEL for exactly-one | works; policy fields would still have no home and each new kind adds two fields |
| `source: {kind, oci, http, path}` nested one level down | same as the chosen shape with an extra level; nothing gained |
| `type` + typed object, `from` inside the typed object (`oci: {from: status.module}`) | reads well for one kind, but `path: {from: …}` turns a string into an object and the credentials rule needs "if from is set" inside each object |
| `policy` and `limits` under `module` | ties Composition-owned policy to the thing an XR may choose; as top-level siblings they are visibly not part of what `from` reads |
| drop `*From` sources altogether | removes the whole class of XR-author decisions; loses the per-tenant use case the README leads with |
