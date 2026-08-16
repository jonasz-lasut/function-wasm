# Module Source Schema

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft, revision 0.3

Whether the Input's `module` object should change shape before `v0.1.0`
freezes it: from six sibling fields to a discriminated union, with `policy`
and `limits` as top-level siblings. Raised by the 2026-08-16 architecture
review; nothing here is implemented. This document is the authoritative
shape and holds every proposed Input field: the trust-model and
resource-governance one-pagers describe what is implemented and point here
for `policy` and `limits`.

## The proposed Input, in one place

```yaml
apiVersion: wasm.fn.crossplane.io/v1beta1
kind: Input
module:                                   # what runs
  type: OCI                               # OCI | HTTP | Path — always the Composition's choice
  oci:                                    # exactly one of the typed object …
    ref: ghcr.io/example/greeter:v1@sha256:…
    credentials: registry
  # http: {url, digest}
  # path: fn.wasm
  # from: status.module                   # … or the XR field holding it (an object of `type`)
policy:                                   # what an XR-chosen module may be and spend
  repositoryAllowList: ["ghcr.io/example-org/"]   # prefixes the XR's ref must match
  credentialsAllowList: ["registry"]              # step credentials the XR object may name
limits:                                   # what this step's run may consume, ≤ the runtime's ceilings
  timeout: 5s
  memory: 128Mi
sandbox:                                  # grants (sandbox one-pager): filesystem, network, env
  scratch: true
config:                                   # opaque, forwarded to the module
  greeting: hi
```

`module` is required; `policy`, `limits`, `sandbox` and `config` are
optional. Everything but `module` (and within it, the object `from` names)
is read from the Input only — never through `module.from`: an XR author may
choose the module, not widen its permissions, its grants or its budget.

## Today

```yaml
module:
  # exactly one of
  oci:      {ref, credentials}
  http:     {url, digest}
  path:     fn.wasm
  ociFrom:  status.module     # the XR field holds an {ref} object
  httpFrom: spec.url          # … an {url, digest} object
  pathFrom: spec.path         # … a string
```

Six optional fields, an "exactly one" invariant, and every source kind
present twice — statically and as a `*From` pointer. The invariant is
enforced by the runtime and, since this revision, by a CEL rule in the CRD.
Rules that depend on *where* a source came from (an XR-chosen source may not
name credentials; a proposed repository allowlist applies to XR-chosen
sources only) live in code and prose, not in the schema. Adding a source
kind adds two fields; adding a policy field has no natural home.

## Proposal

The Crossplane idiom — a `type` discriminator next to a typed object — with
`from` as an orthogonal switch:

```yaml
module:
  type: OCI                    # OCI | HTTP | Path
  oci:
    ref: ghcr.io/example/greeter:v1@sha256:…
    credentials: registry
```

```yaml
module:
  type: OCI
  from: status.module          # the XR field holds an OCI object; oci: is not set
policy:                        # Composition-owned, never read from the XR
  repositoryAllowList: ["ghcr.io/example-org/"]
  credentialsAllowList: ["registry"]
```

- `module.type` says what kind of source this step runs; the Composition
  author always chooses the kind, an XR author only the instance.
- Exactly one of the typed object (`oci`/`http`/`path`) or `from` is set,
  and it must match `type` — three CEL rules, all expressible:
  `self.type == 'OCI' ? (has(self.oci) != has(self.from)) : !has(self.oci)`
  and its two siblings.
- `policy` is a top-level sibling of `module` — the trust model's fields:
  `repositoryAllowList` (prefixes an XR-chosen ref must match) and
  `credentialsAllowList` (step credentials an XR-chosen object may name;
  absent or empty means none, today's rule). It applies to `module.from`
  sources; the CRD can require `has(self.policy)` when `has(self.module.from)`
  for deployments that want it, and it never has to be read from the XR.
- `limits` is the other top-level sibling — the resource-governance
  proposal (`timeout`, `memory`, each at most the runtime's ceiling).
- The runtime's `FromComposite` reads the XR field into the object
  `module.type` names; the credentials rule becomes "an object read through
  `from` may name only what `policy.credentialsAllowList` allows, and only
  for a ref within `policy.repositoryAllowList`" — one rule, stated once,
  and structurally impossible to bypass from the XR.

## `policy`: what an XR-chosen module may be and spend

Refusing credentials on XR-chosen modules is safe but blunt: a platform
team that wants tenants to pick modules *from the platform's private
registry* must mount a Docker config into the runtime. The shape above gives
the Composition a top-level `policy` — read from the Input only, never
through `module.from` — that fences what an XR-chosen module may be and
spend:

```yaml
module:
  type: OCI
  from: status.module                       # the XR names {ref, credentials}
policy:
  repositoryAllowList: ["ghcr.io/example-org/"]   # prefixes the XR's ref must match
  credentialsAllowList: ["registry"]              # step credentials the XR object may name
```

- `policy.repositoryAllowList`: an XR-chosen `ref` (or `url` for `HTTP`)
  must start with one of the entries; anything else is a fatal result naming
  the policy. Absent, any repository is allowed — today's behaviour.
- `policy.credentialsAllowList`: the step credentials an XR-chosen object may
  name, spent only on a ref the repository list admits. Absent or empty, none
  — today's rule. A credential outside the list is a fatal result.

The XR keeps choosing the artifact; the Composition keeps choosing where
from and with what. The rule becomes structural rather than a refusal, the
same field serves signature-free deployments that still want to fence
tenants to a registry, and the CRD can validate it (`policy` required
whenever `module.from` is set, for deployments that want that). Deferred
until a deployment needs private-registry tenant modules without a mounted
Docker config; it lands together with the discriminated `module`.

## `limits`: per-Composition budgets

The ceilings are global. A trusted internal policy module and a tenant's
labelling module get the same 30 s / 512 MB, and the operator cannot give
one more room without giving it to all. The sandbox one-pager's principle
applies — the operator sets the ceiling, the Composition asks for less —
so the Input gains a top-level `limits`, a sibling of `module` and `policy`:

```yaml
module:
  type: OCI
  oci: {ref: ghcr.io/example/greeter:v1@sha256:…}
limits:
  timeout: 5s          # ≤ --module-timeout
  memory: 128Mi        # ≤ --module-memory-limit
```

read from the Input only (never through `module.from`: an XR author must
not widen a budget), validated against the flags at request time and
applied to that run's store. Raising above the ceiling stays operator-only.
Trade-offs: two more Input fields to document, and a Composition author who
sets them tightly gets fatal results the operator did not cause; the
benefit is a shared runtime where the loudest module cannot spend the whole
ceiling. This is a small change once wanted; it is deferred until a
deployment asks for it, so the Input does not grow speculatively before
`v0.1.0`.

## Migration

None: the API is `v1beta1` and unreleased. Doing this later means a
`v1beta2` with a conversion — the Input has no controller, so conversion is
documentation and a `guestfn`-style rewrite. Doing it now costs a day: the
Go types, `FromComposite`, `Validate`, the CRD markers, README, AGENTS,
scaffold templates and goldens, and every test that builds an Input.

## Alternatives

| option | verdict |
|---|---|
| keep the six fields, add CEL for exactly-one (done) | works; policy fields would still have no home and each new kind adds two fields |
| `source: {kind, oci, http, path}` nested one level down | same as the proposal with an extra level; nothing gained |
| `type` + typed object, `from` inside the typed object (`oci: {from: status.module}`) | reads well for one kind, but `path: {from: …}` turns a string into an object and the credentials rule needs "if from is set" inside each object |
| `policy` and `limits` under `module` | ties Composition-owned policy to the thing an XR may choose; as top-level siblings they are visibly not part of what `from` reads |
| drop `*From` sources altogether | removes the whole class of XR-author decisions; loses the per-tenant use case the README leads with |

## Recommendation

Do it before `v0.1.0` if `policy` (trust model) or `limits` (resource
governance) is wanted in the first release — they belong with the
discriminated `module`, and the schema is the cheaper half. If not, ship the
six fields with the CEL rule and revisit with the first policy field; the
cost then is a `v1beta2`.
