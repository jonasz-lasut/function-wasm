# Module Source Schema

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.1

The shape of the Input before `v0.1.0` freezes it: a discriminated `module`
instead of six sibling fields, with `policy`, `limits` and `sandbox` as
top-level siblings. Raised by the 2026-08-16 architecture review; revision
1.0 is what shipped, revision 1.1 records the `sandbox` grants that landed.
This document is the authoritative shape and holds every Input field: the
trust-model and resource-governance one-pagers describe what `policy` and
`limits` mean, the sandbox one-pager what the `sandbox` subtree does.

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
policy:                                   # what an XR-chosen module may be and spend
  repositoryAllowList: ["ghcr.io/example-org/"]   # prefixes the XR's ref (or url) must match
  credentialsAllowList: ["registry"]              # step credentials the XR object may name
limits:                                   # what this step's run may consume, ≤ the runtime's ceilings
  timeout: 5s
  memory: 128Mi
sandbox:                                  # grants (sandbox one-pager) within the --enable-sandbox-* flags
  filesystem: {privateTmp: true}          # a private /tmp; host directories are never mountable
  egress: {http: [{host | hostPattern, methods, pathPrefix}]}
  env: {KEY: value}
config:                                   # opaque, forwarded to the module
  greeting: hi
```

`module` is required; `policy`, `limits`, `sandbox` and `config` are
optional. Everything but `module` (and within it, the object `from` names)
is read from the Input only — never through `module.from`: an XR author may
choose the module, not widen its permissions, its grants or its budget.

## Today

The shape above is implemented in `input/v1beta1/input.go`, enforced by
`internal/module.Validate`/`FromComposite`, `internal/module/policy.go`,
`cmd/function/fn.go` (`runOptions`) and `internal/sandbox`, and described
by the generated CRD under `package/input/`.

- `module.type` is required (`OCI`, `HTTP`, `Path`). Exactly one of the
  typed object it names (`oci`, `http`, `path`) or `from` is set, and no
  object of another type may be present. The runtime checks it on every
  request; three CEL rules on the `module` object say the same for tooling
  that reads the schema (`crossplane resource validate`, IDEs) —
  `self.type == 'OCI' ? (has(self.oci) != has(self.from)) : !has(self.oci)`
  and its two siblings. Crossplane itself never installs a function's Input
  CRD — an Input is a fragment of a Composition, not an object — so the
  runtime is the only gate that always runs, and every marker on the types
  is mirrored in `Validate`/`ValidatePolicy`/`sandbox.Validate`.
- `module.from` names a field of the observed composite resource under
  `spec.` or `status.` (pattern-checked), read on every request and decoded
  strictly (unknown fields refused) into the object `type` names — an
  `{ref, credentials}` object for `OCI`, `{url, digest}` for `HTTP`, a
  string for `Path` — then validated like a static source. Errors name the
  field: `module.from: status.module of the composite resource is not a
  {ref, credentials} object: …`.
- `policy` applies only to XR-chosen modules. `repositoryAllowList` is a
  list of string prefixes the XR's `oci.ref` (or `http.url`) must start with
  — matched on the normalized location, required whenever `module.from`
  names an OCI or HTTP source; a ref outside every prefix is a fatal result
  naming the policy and the ref. `credentialsAllowList` names the step credentials an
  XR-chosen `oci` object may spend, and only on a ref the repository list
  admits — the CRD and the runtime refuse a credentials list without a
  repository list, so a credential is never spendable on an arbitrary host.
  Absent or empty, an XR object naming credentials is refused as before,
  with a hint at the policy. For a static source the policy is ignored (its
  shape is still validated).
- `limits.timeout` (a duration string, e.g. `5s`) and `limits.memory` (a
  quantity, e.g. `128Mi`) must each be at most the runtime's ceiling
  (`--module-timeout`, `--module-memory-limit`); asking for more is a fatal
  result naming both (`limits.memory 1Gi exceeds the runtime's
  --module-memory-limit of 512Mi`), asking for less narrows that run's
  epoch deadline and store limiter (`engine.RunOptions`, capped by the
  engine's `Config` whatever the caller passes).
- `sandbox` - `filesystem.privateTmp`, `egress.http[] {host | hostPattern,
  methods, pathPrefix}`, `env[] {name, value | valueFrom}`, `envFrom[]
  {credential, prefix}`. The Go types, the CRD schema (with a CEL
  rule for host XOR hostPattern) and `internal/sandbox.Validate` check the
  shape; `internal/sandbox.Ceiling` checks each grant against the operator's
  `--enable-sandbox-*` flag and refuses it with a fatal result naming the
  grant and the flag; `egress` is checked against the operator's egress
  ceiling the same way (`internal/egress`). The private `/tmp`
  (`--enable-sandbox-private-tmp`), `env` (`--enable-sandbox-env`) and
  `egress` (`--enable-sandbox-egress` + `--sandbox-egress-policy`) are all
  implemented; host directories are deliberately not mountable. The
  behaviour is the sandbox one-pager's.
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
- `policy` is a top-level sibling of `module` — the trust model's fields. It
  applies to `module.from` sources and never has to be read from the XR; the
  CRD could require `has(self.policy)` when `has(self.module.from)` for
  deployments that want it.
- `limits` is the other top-level sibling — the resource-governance
  proposal (`timeout`, `memory`, each at most the runtime's ceiling).
- The runtime's `FromComposite` reads the XR field into the object
  `module.type` names; the credentials rule is "an object read through
  `from` may name only what `policy.credentialsAllowList` allows, and only
  for a ref within `policy.repositoryAllowList`" — one rule, stated once,
  and structurally impossible to bypass from the XR.

## `policy`: what an XR-chosen module may be and spend

Refusing credentials on XR-chosen modules is safe but blunt: a platform
team that wants tenants to pick modules *from the platform's private
registry* would otherwise have to mount a Docker config into the runtime.
`policy` — read from the Input only, never through `module.from` — fences
what an XR-chosen module may be and spend:

```yaml
module:
  type: OCI
  from: status.module                       # the XR names {ref, credentials}
policy:
  repositoryAllowList: ["ghcr.io/example-org/"]   # prefixes the XR's ref must match
  credentialsAllowList: ["registry"]              # step credentials the XR object may name
```

The XR keeps choosing the artifact; the Composition keeps choosing where
from and with what. The same field serves signature-free deployments that
still want to fence tenants to a registry. Prefixes are plain string
prefixes: `ghcr.io/example-org/` with the trailing slash, or
`ghcr.io/example-organisation/…` is admitted too.

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

None was needed: the API is `v1beta1` and unreleased, and the change landed
before `v0.1.0`. An Input in the old shape fails decoding (`ociFrom` and
friends are unknown fields) or validation (`module.type is required`); the
fix is `type: OCI|HTTP|Path` plus the matching object, or `type` + `from`
for the former `*From` fields.

## Alternatives considered

| option | verdict |
|---|---|
| keep the six fields, add CEL for exactly-one | works; policy fields would still have no home and each new kind adds two fields |
| `source: {kind, oci, http, path}` nested one level down | same as the chosen shape with an extra level; nothing gained |
| `type` + typed object, `from` inside the typed object (`oci: {from: status.module}`) | reads well for one kind, but `path: {from: …}` turns a string into an object and the credentials rule needs "if from is set" inside each object |
| `policy` and `limits` under `module` | ties Composition-owned policy to the thing an XR may choose; as top-level siblings they are visibly not part of what `from` reads |
| drop `*From` sources altogether | removes the whole class of XR-author decisions; loses the per-tenant use case the README leads with |
