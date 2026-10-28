# Three-layer authorization model

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.2 (the output fence, `composeResource`,
  #174; the inline `request` for manifest-less sources stays a separate
  Draft: docs/one-pager-manifest-less-sources.md)

## Summary

Collapse every sandbox/admission decision into one uniform rule: a capability is
granted if and only if all three parties agree, down the trust ladder. The
module's manifest is the **request**; two Cedar layers are the **grants**, each
able only to narrow the preceding one.

```
grant(capability) =
      manifest REQUESTS it              (module author  - least trusted - the ask)
    ∧ Composition-Cedar PERMITS it       (composition author - narrows this Composition)
    ∧ operator-Cedar PERMITS it          (operator - the central ceiling, --sandbox-policy-file)
```

`capability` ∈ { `pullModule` (repo of a `from`-chosen source), `spendCredential`,
`grantEgress` (host/method/path), `usePrivateTmp`, `setEnv`/read a credential-bound
env var, read a step credential whole from the request (`spendCredential`) }.

Revision 1.1 makes `spendCredential` decide what the module *sees*, not only
what reaches its environment: the request it receives contains only the step
credentials granted to it (see "Step credentials" below).

This unifies, and removes the need for, these bespoke Input fields:

| removed from the Input | replaced by |
|---|---|
| `policy.repositoryAllowList` | Composition-Cedar `permit(pullModule when resource in Repository::"…")` |
| `policy.credentialsAllowList` | Composition-Cedar `spendCredential` ∧ operator-Cedar `spendCredential` |
| `sandbox.egress` / `sandbox.filesystem` | manifest `requires` ∧ Composition-Cedar ∧ operator-Cedar |
| `sandbox.env` / `sandbox.envFrom` literals | `config` (module reads via `wasmfn.GetConfig`) |
| `sandbox.env` / `sandbox.envFrom` secrets | manifest `requires.env` credential-binding ∧ `spendCredential` |

The Input shrinks to `module`, `limits` (numeric run budgets - Cedar is the wrong
tool for a clamp, they stay), `config`, and an optional `compositionPolicy` (raw
Cedar text - the composition-author layer).

## Trust safety

Each layer is a **separate Cedar `PolicySet`, AND-combined**, so no layer can
widen another. A greedy manifest → both Cedar layers still gate it. A Composition
author writing `permit(everything)` → they have merely declined to narrow; the
operator layer still caps. Injection-safe like the existing fences: a layer never
reaches into another's evaluation.

## The per-layer, per-capability defaults (the security-critical part)

An implementer must get this right: the layers don't share one default, and
each default matches its trust role and preserves today's behaviour.

**Operator layer** (`--sandbox-policy-file`): **absent → deny-all.** Already true
after the flag-drop change. It's the enabler; no policy = only the default
sandbox.

**Manifest layer**: **an absent manifest requests nothing: the default sandbox.**
`path`/`http` and manifest-less OCI sources don't request anything. A fallback
inline `request` has its design in its own one-pager
(docs/one-pager-manifest-less-sources.md).

**Composition layer** (`compositionPolicy`): two regimes, by capability. It
narrows sandbox capabilities and it's a *required fence* for XR-chosen sources -
exactly what `sandbox` and the allowlists do today:

- Sandbox capabilities (`grantEgress`, `usePrivateTmp`, `setEnv`, and
  `spendCredential` for a credential the module reads from its request - an
  env binding's or a `requires.credentials` entry, judged with no
  `context.repository`): **scoped default-permit.** If the composition
  policy is absent, or contains no rule scoping that action, the layer
  permits (no narrowing - operator ∧ manifest govern). If it contains any
  rule for that action, that action becomes
  default-deny *within the composition layer* (the author has opted into
  narrowing it). Detected the way `HasSignatureRules`/`HasPrivateTmpRules` scan
  for an action today.
- Source fencing (`pullModule`, `spendCredential`) for a `from`-chosen source:
  **default-deny.** A `from` source is refused unless the composition policy
  explicitly permits its repository (and any credential), preserving today's rule
  that `module.from` *requires* a `repositoryAllowList` (and `credentialsAllowList`
  only within it). A **static** source (the Composition specifies the module
  itself) isn't judged by this layer - it's the Composition's own choice, as today.

- The output fence (`composeResource`, revision 1.2, #174): **scoped
  default-permit in both layers**, the operator's included. It's not a
  capability a module requests but a fence on what it writes, judged over
  the response after the run (every resource added, modified or removed,
  the composite's status included, as `Kind::"<group>/<Kind>"` in a
  `Group` hierarchy with `context.change`, `name` and `target`). A layer
  with no rule scoping the action narrows nothing, and a runtime with no
  policy file doesn't either: a module's output was never gated before the
  fence existed, and a minor may only widen. Once scoped, Cedar's usual
  rule; a `forbid` alone permits nothing. The runtime evaluates
  the composition layer first and whole, then the operator's. The fence judges
  `modify` on the entire resource (body, connection details, ready flag), compared by value;
  a `context.fields` set that lets a policy permit the ready flag alone is a
  known, additive extension no use case has asked for yet.

Worked table (S = sandbox cap, F = from-source fence, O = the output fence;
the O column holds for the operator layer too, which is where it differs
from S):

| composition policy state | S: `grantEgress`/`usePrivateTmp`/`setEnv`/`spendCredential` (request) | F: `pullModule`/`spendCredential` (from) | O: `composeResource` |
|---|---|---|---|
| absent | permit (operator ∧ manifest decide) | deny (from-source refused) | permit, both layers |
| present, no rule for the action | permit | deny | permit |
| present, has `permit` for it | permit if matched, else deny | permit if matched, else deny | permit if matched, else deny |
| present, has `forbid` for it | forbid wins | forbid wins | forbid wins (and with no permit beside it, every change is refused) |

## The env model (#3)

`sandbox.env`/`sandbox.envFrom` leave the Input entirely.

- **Non-secret config** (`LOG_LEVEL=debug`) was never env - it's
  deployment config. It moves to the existing `config` field; the guest already
  reads per-instance config through `wasmfn.GetConfig`. It takes no env
  variable, ceiling or credential.
- **Secret env** (a library that only reads `$DATABASE_URL`): the **manifest**
  declares the binding - `requires.env: [{name: DATABASE_URL, fromCredential:
  {name: db, key: url}}]` - and the module itself defines its env contract
  there. At run time
  `Materialize` (internal/sandbox) resolves it from the request's step
  credentials exactly as `valueFrom.credential` does today (reuse
  `credentialData`, the withheld-pull-credential and NUL guards unchanged), and
  `spendCredential(db)` must permit it in **both** Cedar layers. The credential
  VALUE still arrives at the pipeline step. The module never authors it. The
  design drops the bulk-import-every-key `envFrom` shape - a binding specifies exactly
  the keys it needs, which was the safer half anyway.

Both `env.valueFrom.credential` and `envFrom.credential` then disappear from
the Input, and their one useful behaviour (inject a named credential key as
a named env var) lives on as a manifest binding that Cedar judges. That closes
the duplication question and the gap that a module can't request env for
itself, in one move.

## Step credentials (revision 1.1)

A pipeline step's credentials arrive in every `RunFunctionRequest`. Before
revision 1.1 the runtime forwarded every one of them but the pull credential,
as Crossplane hands them to a native function (and as the Go runtime did), so
`spendCredential` judged only an env binding - a convenience - while the module
could read the same key from its request. Now the runtime edits the request, at the
wire level, so it contains only the credentials granted to the module:

- the credential each admitted `requires.env` binding reads, whole;
- each credential the manifest requires whole, `requires.credentials:
  [cmdb]` - a shared Secret the module picks a key from - admitted by
  `spendCredential` under both layers, exactly as an env binding's
  credential is;
- nothing else: a module with no manifest sees none, as does one whose
  manifest doesn't require a credential. The pull credential is never
  forwarded (the runtime refuses a manifest that names it).

A required credential either layer doesn't permit is a fatal result
before the run (`… requires credential "cmdb" (requires.credentials[0]),
which the operator policy (--sandbox-policy-file) does not permit`), and so
is one the step doesn't include, as for an env binding. `function validate
--resolve` lists the credentials an admitted step's module receives.

## Cedar schema (unchanged, reused for both layers)

Both the operator and composition policies compile against the **same** schema
already in `internal/authz`: actions `pullModule`, `spendCredential`,
`grantEgress`, `usePrivateTmp`, `setEnv`, `requireSignature`, `dialAddress`,
and entities `Repository`, `HostPattern`, `Capability`, `Credential`. The
principal is `Request { namespace, xrKind, composition }`. The runtime
compiles the composition policy from the Input's `compositionPolicy` text and
**caches it by content hash** (it's per-Composition, not per-request). A malformed
`compositionPolicy` is a fatal result at admission, in the runtime's words, the
same as a malformed operator policy is a startup or `function validate` error.

## Code shape (for the implementer)

1. `input/v1beta1/input.go`: delete `Policy` and `Sandbox`; add
   `CompositionPolicy string` (raw Cedar, `+optional`). Regenerate the CRD. Keep
   `Module`, `Limits`, `Config`.
2. `internal/authz`: add `CompositionPolicy` (compile-from-bytes, content-hash
   cache) with `Permits*` mirroring `OperatorPolicy`, plus the scoped-default
   helpers (`ScopesAction`). Add a small `Decision` helper that AND-combines
   operator ∧ composition ∧ (manifest request) per capability with the
   preceding defaults.
3. `internal/manifest`: extend `Requires` with `Env []EnvBinding{Name,
   FromCredential{Name,Key}}`. `Check`/`Sandbox`/`Validate` learn env bindings.
4. `internal/sandbox`: `Materialize` sources env from the manifest bindings +
   request credentials (not the Input). Drop the Input env/envFrom paths.
5. `internal/admission`: replace the `policy`/`sandbox` checks with the
   three-layer `Decision`. `module.FromComposite`/`policy.go`: the from-source
   fence becomes the composition-layer `pullModule`/`spendCredential` default-deny.
6. `cmd/function/fn.go`, `validate.go`: build both policy layers, with the
   composition policy read from the Input.
7. Tests to green. **Defer** (call out, don't attempt in this pass): the inline
   `request` for manifest-less sources (still Draft), all `guestfn` changes
   (scaffold/build/push/manifest), the examples, and the full doc scrub.

## Migration / back-compat

Breaking, and large - stacked on the flag-drop change. An existing Composition
using `policy`/`sandbox` fails to decode (strict) or is refused. `function
validate` reports the removed field and its replacement. A `vX.0` and a
migration note are due, and `guestfn` should learn to emit a `compositionPolicy`
skeleton from an old `sandbox`/`policy` block to ease the port (later).

## Non-goals

- Letting the manifest or the composition policy **widen** past the operator
  ceiling - the AND combination of the layers makes that impossible.
- Cedar for `limits` - a numeric clamp isn't an authorization question.
- A permissive default for the operator or the from-source fence - both stay
  deny-by-default for capabilities. The output fence is the recorded
  exception: scoped default-permit, since a default-deny would have refused
  every module's output on upgrade.
- Fencing `results`, `requirements` or `context` writes: `results` are
  messages, `requirements` are reads the proxy already relays round trip,
  and context writes could take an action of the same form later without
  touching this design.
