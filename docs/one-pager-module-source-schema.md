# Module source schema

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 2.3 (rewritten in the present tense over the Rust workspace; the Input is `wasm.fn.crossplane.io/v1`)

The Input is what a Composition step hands the runtime. A discriminated
`module` says what runs, and the fields the Composition author controls sit
beside it at the top level. The 2026-08-16 architecture review
raised it. The user-facing field reference is
[the Input reference](operators/input-reference.md); this record keeps why the
Input is structured this way and what `compositionPolicy` and `limits` decide. The
[trust model](one-pager-trust-model.md) describes what the `compositionPolicy`
fence means, [resource governance](one-pager-resource-governance.md)
`limits`, and the [sandbox](one-pager-sandbox.md) the capabilities a manifest
may request.

## The Input, in one place

```yaml
apiVersion: wasm.fn.crossplane.io/v1
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
optional. A `v1beta1` document with the same fields is accepted through 1.x
and removed in 2.0.0 (`input::V1BETA1_DEPRECATION`, see
[Compatibility](contract/compatibility.md)).

Everything but `module` (and within it, the object `from` names) is read from
the Input only, never through `module.from`: an XR author may choose the
module but may not widen its permissions, grants or budget. The Input has no
`sandbox` subtree. A module requests anything beyond the default sandbox in
its manifest's `requires`. The runtime grants a capability only when the
manifest, the `compositionPolicy` and the operator's `--sandbox-policy-file`
all permit it ([three-layer authorization](one-pager-three-layer-authz.md)).
`function validate` refuses an Input with the removed `policy`
or `sandbox` field and names the replacement (`the Input's policy field was
removed: …`, `the Input's sandbox field was removed: …`).

## Why this structure

A source is chosen in one of two places: the Composition author writes it
into the Input, or an XR author names it in a field of the composite resource.
Rules that depend on where a source came from (an XR-chosen source may reference a
credential only where the Composition permits it, and only inside an allowed
registry) belong in the structure of the schema. A `type` discriminator
next to a typed object is the Crossplane idiom, and `from` is an orthogonal
switch on it:

- `module.type` says what kind of source the step runs. The Composition author
  always chooses the kind, an XR author only the instance.
- Exactly one of the typed object (`oci`, `http`, `path`) or `from` is set,
  and no object of another type may be present. The runtime checks it on
  every request. Three CEL rules on the `module` object say the same for
  tooling that reads the schema (`crossplane resource validate`, IDEs), such
  as `self.type == 'OCI' ? (has(self.oci) != has(self.from)) : !has(self.oci)`
  and its two siblings. Crossplane never installs a function's Input CRD (an
  Input is a fragment of a Composition), so the runtime is the
  only gate that always runs: `admission::admit` and `from::from_composite`
  apply every rule the CRD under `package/input/` states.
- The Composition-owned rules are top-level siblings of `module`, never read
  from the XR. The CRD's CEL requires a non-empty `compositionPolicy` when
  `module.from` names an OCI or HTTP source.
- `limits` is the other top-level sibling.
- An object read through `from` may reference a
  credential only where the `compositionPolicy` permits `spendCredential`,
  and only for a ref a `pullModule` permit admits. The XR can't bypass that rule,
  because the policy never comes from the XR.

## `module.from` and `allowEmpty`

`module.from` names a field of the observed composite resource under `spec.`
or `status.` (pattern-checked), read on every request. `from_composite`
decodes it, refusing unknown fields, into the object `type` names (an
`{ref, credentials}` object for `OCI`, `{url, digest}` for `HTTP`, a string
for `Path`) and then validates it like a static source. An error quotes the
field: `module.from: status.module of the composite resource is not a
{ref, credentials} object: …`. A field the composite resource hasn't set is
an error naming it (`module.from: cannot read status.module from the
composite resource: module: no such field`).

`module.allowEmpty: true` changes that case. The step doesn't run a module and
returns the request's desired state and context unchanged, built at the wire
level from the caller's own bytes and counted as
`requests_total{outcome="skipped"}`. A field that's set is read, fenced and
run exactly as without it. `allowEmpty` is read from the Input only and
refused without `from`: the Composition author chooses whether a step may be
left empty, the composite resource only whether that step is (issue #82). It
serves the reserved hook of the first use case ([use cases](use-cases.md))
before any tenant fills it in, and a smoke test of a deployed runtime that
doesn't need a module built.

## `compositionPolicy` and what an XR-chosen module may be and spend

Refusing credentials on XR-chosen modules would be safe but blunt: a platform
team that wants tenants to pick modules from the platform's private registry
would have to mount a Docker config into the runtime. `compositionPolicy`
fences what an XR-chosen module may be and spend instead:

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

It's raw Cedar over the same schema as the operator's policy, compiled at
admission (content-hash cached). Malformed Cedar is a fatal result. For an
XR-chosen source it's a required, default-deny fence. The XR's `oci.ref` (or
`http.url`), normalized, must be permitted by a `pullModule` rule over the
`Repository` hierarchy. A named credential also needs a `spendCredential`
permit, whose `context.repository` contains the ref's location so both halves
co-locate. For sandbox actions it only narrows, and only where it scopes the
action. A static source skips the fence: the Composition named it and is
trusted with it. The XR keeps choosing the artifact; the Composition keeps
choosing where from and with what. The same field serves signature-free
deployments that still need to fence tenants to a registry.

The `Repository` hierarchy is boundary-correct: `Repository::"ghcr.io/example-org"`
admits `ghcr.io/example-org/mod` but never the sibling namespace
`ghcr.io/example-org-other/…`. A trailing slash is optional and doesn't
change the boundary.

A manifest-less source can reference its manifest instead: `module.http.manifestURL`
together with `manifestDigest`, or `module.manifestPath` with `type: Path`
([module manifests](guests/module-manifests.md)). The runtime loads it as it
loads an OCI manifest layer, so the three-layer decision is identical. For a `module.from` `http` source the `manifestURL` is fenced by
`pullModule` like the module URL. The Input alone supplies `manifestPath`.

## `limits` and per-composition budgets

The ceilings are global (`--module-timeout`, `--module-memory-limit`), so a
trusted internal module and a tenant's labelling module would get the same
budget. The operator sets the ceiling and the Composition requests less, so
the Input has a top-level `limits`, read from the Input only (an XR author
must not widen a budget). `admission::admit` checks each value against its
flag at request time. Asking for more is a fatal result that quotes both
(`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`);
asking for less narrows that run's epoch deadline and memory limiter in the
engine's `RunOptions`. Raising a limit past the ceiling remains operator-only.
A Composition author who sets limits with little headroom gets fatal results
the operator didn't cause. The benefit is a shared runtime where the loudest
module can't spend the whole ceiling.

## Alternatives

Optional fields under `module` (`oci`, `http`, `path` and a `*From`
pointer for each) with a CEL rule that exactly one is set would work. It
presents every source kind twice. Each new kind adds two fields, and the
policy fields would still have no home.

`source: {kind, oci, http, path}` nested one level down is the chosen
structure with an extra level and gains nothing.

Putting `from` inside the typed object (`oci: {from: status.module}`) reads
well for one kind, but `path: {from: …}` turns a string into an object, and
the credentials rule then needs an "if from is set" check inside each object.

`policy` and `limits` under `module` would tie Composition-owned policy to the
thing an XR may choose. As top-level siblings they're visibly not part of
what `from` reads.

Dropping `from` sources altogether removes the whole class of XR-author
decisions, but loses the per-tenant case that [the use cases](use-cases.md) lead
with.
