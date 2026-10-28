# Three-layer authorization model

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.3 (rewritten in the present tense over the Rust workspace; supersedes the request-delivered-secrets record)

## Summary

A capability is granted if and only if three parties agree, down the trust
ladder (Jonasz, 2026-08-19). The module's manifest is the request. The
composition policy and the operator policy, both Cedar, are the grants, and
each can only narrow what the preceding layer allows.

```
grant(capability) =
      manifest REQUESTS it              (module author, least trusted: the ask)
    ∧ composition policy PERMITS it     (composition author: narrows this Composition)
    ∧ operator policy PERMITS it        (operator: the ceiling, --sandbox-policy-file)
```

The capabilities are a private `/tmp` (`usePrivateTmp`), HTTP egress to a host,
method and path (`grantEgress`), an environment variable bound to a step
credential (`setEnv` and `spendCredential`), a step credential read whole from
the request (`spendCredential`) and the repository of a source an XR chose
(`pullModule`). A further action, `composeResource`, fences what a module
writes rather than what it may do (see "The output fence").

Because each decision belongs to a layer, the Input has no `sandbox` or
`policy` field. It holds `module`, `limits`, `config` and an optional
`compositionPolicy`, the composition author's raw Cedar. `limits` stay numeric
clamps. Non-secret module settings live in `config`, which the guest reads
through `GetConfig`.

## Trust safety

Each layer is a separate Cedar `PolicySet`, and the layers are AND-combined, so
no layer can widen another. A greedy manifest still meets both policies, and a
composition author who writes `permit(everything)` has only declined to narrow:
the operator policy still caps. Each layer is evaluated apart from the others.

## Defaults per layer

The layers don't share a default: each matches its trust role.

The operator layer is default-deny. Without `--sandbox-policy-file`, a module
gets the default sandbox and nothing else.

An absent manifest requests nothing, so a `path`, `http` or OCI source without
a manifest runs in the default sandbox. A manifest can only refuse earlier; it
never makes a run possible. A `path` or `http` source can point at a
`wasmfn.yaml` by reference (`manifestPath`, `manifestURL`). Such a source is a static source,
fenced like any other, and the manifest read through it makes the same request.
See [Module manifest](one-pager-module-manifest.md) and
[Module manifests](guests/module-manifests.md).

The composition layer has two regimes, by capability:

- Sandbox capabilities (`grantEgress`, `usePrivateTmp`, `setEnv`, and
  `spendCredential` for a credential the module reads from its request) are
  scoped default-permit. A composition policy that's absent, or has no rule
  for an action, doesn't narrow it, and the operator and manifest decide. Once
  a rule scopes an action, that action becomes default-deny within this layer:
  the author has opted in to narrowing it.
- Source fencing (`pullModule`, and `spendCredential` for the source's pull
  credential) of a `from`-chosen source is default-deny. An XR's choice of
  module is refused unless the composition policy permits its repository. A
  static source, one the Composition specifies itself, isn't judged by this
  layer: it's the composition author's own choice.

| composition policy state | sandbox capability | `from` source fence | output fence |
|---|---|---|---|
| absent | permit | deny | permit |
| present, no rule for the action | permit | deny | permit |
| present, a `permit` for it | permit if matched, else deny | permit if matched, else deny | permit if matched, else deny |
| present, a `forbid` for it | forbid wins | forbid wins | forbid wins |

A malformed `compositionPolicy` is a fatal result at admission, and a
malformed operator policy is a startup or `function validate` error. The
runtime compiles the composition policy once per distinct text, cached by
content hash.

## Environment and step credentials

A module that reads only `$DATABASE_URL` declares the binding in its manifest:
`requires.env: [{name: DATABASE_URL, fromCredential: {name: db, key: url}}]`.
The runtime resolves it from the request's step credentials, and
`spendCredential` for `db` must be permitted by both Cedar layers. The module
defines its env contract and never supplies the value. A binding lists exactly
the keys it needs. There's no bulk import of every key of a credential.

Step credentials arrive in every `RunFunctionRequest`. Crossplane hands them
all to a native function, and forwarding them all made `spendCredential` gate
a convenience: a module denied an env binding could still read the key from
its request (issue #122, 2026-10). The runtime edits the request at the wire
level instead, and the module receives only:

- the credential each admitted `requires.env` binding reads, whole;
- each credential the manifest lists under `requires.credentials`, whole: a
  shared Secret the module picks a key from, admitted by `spendCredential`
  under both layers like an env binding's.

A module without a manifest, or whose manifest doesn't require a credential,
sees none. The pull credential is never forwarded, and the runtime refuses a
manifest that names it. A required credential that either layer doesn't
permit, or that the step doesn't include, is a fatal result before the run:

```
… requires credential "cmdb" (requires.credentials[0]), which the operator policy (--sandbox-policy-file) does not permit
```

`function validate --resolve` lists the credentials an admitted step's module
receives.

## The output fence

`composeResource` (Jonasz, 2026-10-27, #174) fences what a module composes.
After the run, the runtime diffs the response's desired state against the
request's and judges every change (an addition, a modification or a removal,
the composite's included) as `Kind::"<group>/<Kind>"` in a `Group` hierarchy,
with `context.change`, `name` and `target`. `modify` is judged on the entire
resource, compared by value. The composition layer is evaluated first and
whole, then the operator's. A refusal is a fatal result naming kind, name,
change and layer.

It's the one action that's scoped default-permit in the operator layer too.
A module's output was never gated before the fence, and a default-deny would
have refused every module's output on upgrade. A runtime with no policy file,
or a layer with no rule for the action, narrows nothing. Once scoped, Cedar's
usual rule applies, and a `forbid` alone permits nothing. A `context.fields`
set that would let a policy permit the ready flag alone is a possible additive
extension no use case has asked for yet.

## Cedar schema

Both policies compile against one schema. The composition policy can use
`pullModule`, `spendCredential`, `grantEgress`, `usePrivateTmp`, `setEnv` and
`composeResource`, and the operator policy everything but `pullModule`, plus
`requireSignature`, `trustSigner` and `dialAddress`. The principal is
`Request { namespace, xrKind, composition }`. The entities, the contexts and
the evaluation rules are on [Cedar policy](operators/cedar-policy.md).

## Code shape

`crates/function/src/authz.rs` is the only file that imports `cedar-policy`. It
holds `CompositionPolicy` and `OperatorPolicy` with a `permits_*` method per
action, and `scopes_action` (the composition layer's opt-in signal, with
`scopes_compose_resource` for the operator's). `crates/function/src/admission.rs`
combines them: `admit` runs the Input's checks, `admit_requires` intersects the
manifest's request with both layers into `Capabilities`, and `admit_compose`
judges the output fence. `crates/function/src/from.rs` applies the `from`
fence. `crates/function/src/sandboxenv.rs` materializes the env bindings and
checks the required credentials, and `crates/function/src/protowire.rs` drops
the credentials that weren't granted. `function validate` calls the same
functions.

## Alternatives

Cedar for `limits` was rejected: a numeric clamp isn't an authorization
question. The limits remain Input fields under the runtime's ceiling flags.

A default-deny output fence in the operator layer would have matched the other
capabilities. It lost because it would have broken every existing module on
upgrade.

## Non-goals

- Letting the manifest or the composition policy widen past the operator
  ceiling. The AND combination makes it impossible.
- A permissive default for the operator layer or the `from` fence. Both stay
  default-deny. The output fence is the recorded exception.
- Fencing `results`, `requirements` or `context` writes. `results` are
  messages, `requirements` are reads the proxy already relays, and a `context`
  write could take an action of the same form later without touching this
  design.
