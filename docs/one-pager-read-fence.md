# Read fence

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.0 (a `requireResource` action over the response's requirements, for finding F-6 of the 1.0 security audit, #208; the owner settled the open points on 2026-11-05)

## Summary

`composeResource` fences what a module writes to the desired state. Through
the response's `requirements` a module requests any resource the Function's
RBAC lets Crossplane read, as a native function can, and the runtime relays
the selectors without reading them. Crossplane then fetches cluster data the
sandbox never exposes and sends it to the module in the next request's
`required_resources`. No policy rule judges that request today. The
Function's RBAC is the one fence, and RBAC is per Function, never per
Composition or per tenant.

This record adds a tenth Cedar action, `requireResource`. The runtime judges
every resource selector of a response in both policy layers, scoped
default-permit in each, over the entities `composeResource` uses and with a
refusal sentence of the same form. The runtime judges the selectors because
the selectors are what it sees. Crossplane reads the resources themselves
after the response and sends them to the module in a later request. The
manifest gains an advisory `requires.resources` declaration, for the
tooling.

## Design

### The action

`requireResource` takes a `Kind` resource in the `Group` hierarchy
`composeResource` uses: `Kind::"<group>/<Kind>"` from the selector's
`apiVersion` and `kind`, the core group spelled `core`, the version dropped,
`resource in Group::"apps"` true for every kind of the group. The context
describes the selector:

| attribute | present | value |
|---|---|---|
| `context.name` | for a match by name | the selector's `matchName` |
| `context.labels` | for a match by labels | a record of the selector's labels, label keys as attributes, so `context.labels has team` and `context.labels["app.kubernetes.io/part-of"] == "x"` both read |
| `context.namespace` | when the selector sets one | the selector's `namespace` |

An attribute the selector doesn't set is absent, never empty, and a
selector matches by name or by labels, never both. Test an attribute with
`has` first, as `spendCredential` rules test `context.repository`. Cedar
treats a rule that reads an absent attribute as unmatched, which turns a
`forbid` into no fence. The Cedar page says so beside the worked example.

The principal is `Request::"self"` as for every action. A tenancy rule
reads `context has namespace && context.namespace == principal.namespace`.

### The selectors

After the run, over the response the runner decoded for the output fence,
the runtime reads every `ResourceSelector` in `requirements.resources` and
in the deprecated `requirements.extra_resources`. Crossplane still serves
the deprecated map, and a module could otherwise use it to step around the
fence. The selectors of each map come in key order, the current map first,
so the first refusal is the same on every reconcile. The response's bytes
don't change.

A selector with no `apiVersion` or no `kind` matches no rule. Where a layer
scopes the action the runtime refuses such a selector with its own
sentence, as the output fence refuses a composed resource with no kind.
Where no layer scopes the action the runtime relays it as today, and
Crossplane answers it.

The runtime judges the selectors on every response, one with a fatal result
included. The output fence skips such a response because Crossplane applies
none of its desired state, and the guest's own message stays the one the
author reads. A selector is a request the proxy relays whatever the results
say. Whether Crossplane acts on the selectors of a fatal response is
Crossplane's choice, which may change between its minors. A refused selector
replaces the guest's fatal result with the runtime's, which states the cause
the author fixes first.

The runtime judges the selectors before it diffs the desired state. A
response with a refused selector and a refused kind reports the selector.
The order follows the module's logic, where the read precedes the write,
and the cheaper judgment runs first.

`requirements.schemas`, the CRD schemas a module may request, remains
unfenced. A schema is public metadata of a kind. The request's
`required_resources` keeps its bytes too. It contains what Crossplane
fetched for a selector the runtime permitted on the previous call.

### Evaluation

Both layers judge the action as they judge `composeResource`:

- scoped default-permit in each layer: a layer narrows the reads only where
  one of its rules spells the action, and a `forbid` alone permits nothing
- the composition layer first and whole, then the operator's, with the
  refusal stating the layer

The operator layer keeps the scoped default-permit here for the reason the
output fence recorded. No rule judged the selectors before this change. A
default-deny would refuse, on upgrade, every module that requests an
EnvironmentConfig, the policy-gate example included. A runtime with no
policy file and a Composition with no rule for the action pay one boolean
per response.

A refusal is a fatal result on a fresh response, counted as
`requests_total{outcome="refused"}` and prefixed with the module as every
refusal is:

- by name: `module oci …: required core/Secret "db-password" in namespace
  "team-b" (requirements.resources["db"]), which the compositionPolicy does
  not permit (requireResource)`
- by labels: `module oci …: required core/Secret with labels
  team=a,tier=gold (requirements.resources["secrets"]), which the operator
  policy (--sandbox-policy-file) does not permit (requireResource)`
- a selector no rule can judge: `module oci …: required resource "db"
  states no apiVersion or kind, so the policy layers cannot judge it
  (requireResource)`

The `in namespace` clause appears when the selector sets one, the labels
come in key order, and the sentence spells the map as the response did.
`requests_total` already counts a refused request, which is the one
metric the fence needs.

### Offline validation

`function validate` can't judge a selector offline any more than it can
judge output. It marks a step whose `compositionPolicy` scopes the action
with `requireResource` on its OK line and in the JSON `admitted` list,
beside `composeResource`.

### The manifest's `requires.resources` declaration

A manifest declares what the module asks Crossplane for under `requires`,
beside the capabilities it needs (Jonasz, 2026-11-05: under `requires`, not
a top-level `reads`):

```yaml
requires:
  resources:
    - kind: apiextensions.crossplane.io/EnvironmentConfig   # <group>/<Kind>, the core group spelled core
      by: [name]                                            # name, labels; both when omitted
```

The declaration is advisory, as `composes` is, and unlike the other
`requires` entries the runtime never grants or refuses it before the run. The fence
judges the actual selectors after the run regardless of the declaration,
because a manifest requests and never grants. The tooling reads it in the
three places that read `composes`:

- `function validate --resolve` warns where a layer would refuse a declared
  kind: `the module declares it requires
  apiextensions.crossplane.io/EnvironmentConfig (by name), which the
  compositionPolicy does not permit (requireResource)`. The offline
  judgment evaluates a read by name with `context.name` empty and a read by
  labels with `context.labels` empty, and never sets `context.namespace`,
  so a rule conditioned on a value may warn for a selector the run would
  permit. The warning is advisory, as the `composes` warning is.
- `guestfn inspect` shows it in the manifest summary, as `resources <kinds>`.
- `guestfn scaffold composition` writes one `requireResource` permit per
  declared kind into the `compositionPolicy` skeleton, with
  `context has name` or `context has labels` from `by`.

`guestfn build` and `guestfn manifest validate` refuse a `kind` that isn't
`<group>/<Kind>`, a `by` entry other than `name` and `labels`, and an
unknown field inside an entry, the strict decode the rest of `requires`
gets. The policy-gate
example and the end-to-end guest declare their EnvironmentConfig read, the
example's version bumped so the published artifact contains it.

### A worked example

A Composition whose module reads the EnvironmentConfig its XR specifies:

```cedar
permit (principal, action == Action::"requireResource",
        resource == Kind::"apiextensions.crossplane.io/EnvironmentConfig")
when { context has name };
```

An operator who keeps every module's reads inside the tenant's namespace,
with the cluster-scoped Crossplane kinds open:

```cedar
forbid (principal, action == Action::"requireResource", resource)
when { resource == Kind::"core/Secret" };

permit (principal, action == Action::"requireResource", resource)
when { context has namespace && context.namespace == principal.namespace };

permit (principal, action == Action::"requireResource",
        resource in Group::"apiextensions.crossplane.io");
```

The first rule keeps Secrets off limits for every module, the second lets a
module read its own namespace and the third opens the cluster-scoped
Crossplane kinds. A selector that omits the namespace and selects a
namespaced kind matches no `permit`, so a module can't read across
namespaces by leaving the field out.

## Code shape

`crates/function/src/authz.rs` gains the action name, the `Requirement`
type (group, kind, the match as a name or a sorted label map, the optional
namespace, and the map and key it came from, for the sentence), its context
builder and a `permits_require` on both policies over the existing
`kind_entities`. The operator policy's `scopes_compose` boolean becomes the
same per-action map the composition policy keeps, so both expose
`scopes_action`.

A new `crates/function/src/require.rs` is the pure half, beside
`compose.rs`. It turns the selectors of a decoded `RunFunctionResponse`
into `Requirement`s in judging order, or returns the error for a selector
no rule can judge. It's plain Rust over the prost types, with `authz.rs`
keeping the Cedar import, and its tests run without an engine. `compose::group_of` reads the `apiVersion`.

`crates/function/src/admission.rs` gains `require_fenced`,
`require_refused_by`, `admit_require` and `describe_requirement`, mirroring
their `compose` twins, with the layer naming shared. `runner.rs` calls them
after the response decode and before the output fence, under the same
`tracing::debug!` of what it judges. `validate.rs` adds the marker and the
advisory loop over the manifest's `requires.resources`, beside the one over
`composes`.

`crates/function/src/manifest.rs` gains the `RequiredResource` type
(`kind`, `by`) under `Requires`, its validation, its strict decode with the
rest of `requires` and its place in the summary. `crates/guestfn/src/composition.rs` writes the skeleton
permits from it.

## Tests

- `authz.rs`: the hierarchy over `Kind` and `Group` for the new action, the
  three context attributes with and without `has`, a `forbid` alone, and
  the operator layer scoping only when a rule spells the action.
- `require.rs`: both maps in key order, the core group, a selector with no
  kind and a response with no requirements.
- `admission.rs`: the layer stated in each sentence, and an unscoped layer
  narrowing nothing.
- `manifest.rs`: `requires.resources` parsed, validated and refused for a
  bad kind, a bad `by` and an unknown field, and shown in the summary.
- `runner.rs`: a `fixed_component` guest returning a response with
  selectors, admitted under one policy and refused under another, the
  fatal-result case judged, and the `refused` outcome counted.
- Conformance: a `require.yaml` fixture beside `compose.yaml` whose first
  step scopes the action, with `require-ceiling.cedar` and the
  `RequireResource`, `RequireResource-json` and
  `RequireResource-OperatorCeiling` goldens, recorded on purpose, plus the
  `RequireResource-Declared` and `RequireResource-DeclaredCeiling` goldens
  for the advisory warning under `--resolve`, as `composes` has.
- `guestfn`: the scaffold-composition golden with a manifest that declares
  reads, and `inspect` showing them.
- End to end, default profile: the guest's `requirements` knob under a
  `compositionPolicy` that forbids the EnvironmentConfig kind, expecting the
  fatal sentence and no `status.environmentConfig`, beside the existing
  round-trip test, which gains a `permit` to prove the admitted path. Strict
  profile: the operator policy scopes the action to the tenant's namespace
  and refuses the same ask with its own sentence.

## Documentation

`docs/operators/cedar-policy.md` gets the action in the layers and actions
tables, a "The read fence" section replacing the paragraph that proposes
it, the worked example and the refusal rows. `trust-model.md`, `AGENTS.md`
and the one-pager on three-layer authorization lose the sentence that
states the gap and count ten actions. `troubleshooting.md` gets the two
sentences, `how-a-request-runs.md` the step and `docs/project/testing.md`
the cluster case. `docs/guests/module-manifests.md` gets a "The
`requires.resources` declaration" section beside the `composes` one, and the
module manifest one-pager a revision for the field.

## Compatibility

An added action is what a minor may add under the compatibility promise. A
policy that doesn't spell it decides nothing new, the Input doesn't change
and no existing sentence changes. The new sentences join the goldens. A
policy that already spelled `requireResource` in a rule starts to narrow
reads on upgrade, which is what its author wrote.

## Alternatives

- Judging the delivered resources in the next request's
  `required_resources`. Rejected. Crossplane has read them by then, and the
  proxy would have to edit the request to withhold them, against the design
  that forwards the caller's bytes.
- Default-deny in the operator layer. Rejected for the reason recorded for
  the output fence. The modules that read an EnvironmentConfig today, the
  policy-gate example and the end-to-end guest, would stop on upgrade.
- Labels as a set of `key=value` strings. Rejected. A record keeps `has`
  and equality natural, and a set would make a rule parse strings.
- Skipping a response with a fatal result, as the output fence does.
  Rejected. The selector travels either way.
- Releasing the fence without the manifest's declaration and adding it in a
  later minor. Rejected (Jonasz, 2026-11-05). The tooling that reads
  `composes` reads the declaration the same way, and a module author who
  declares what the module composes expects to declare what it requires
  beside it.
- A top-level `reads` field for the declaration. Rejected (Jonasz,
  2026-11-05) for `requires.resources`: what a module asks Crossplane for is
  something it requires, named as the proto names it, and the author's
  `requires` block is the one place that lists what the module needs.
- Fencing `requirements.schemas`. Non-goal (Jonasz, 2026-11-05). A schema
  is public metadata, and the runtime relays a schema request as before.

## Decision

Jonasz settled the four open points on 2026-11-05.

- The runtime judges the selectors of a response with a fatal result.
- The runtime judges the deprecated `extra_resources` map beside `resources`.
- The manifest's declaration, as `requires.resources`, is part of the same release as the fence.
- `requirements.schemas` remains unfenced.

Implemented on 2026-11-05 (#233), with the other audit findings of #204
closed before 1.0.0.
