# Three-Layer Authorization Model

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft (implementer's brief)

## Summary

Collapse every sandbox/admission decision into one uniform rule: a capability is
granted iff all three parties agree, down the trust ladder. The module's manifest
is the **request**; two Cedar layers are the **grants**, each able only to narrow
the one above it.

```
grant(capability) =
      manifest REQUESTS it              (module author  - least trusted - the ask)
    ∧ Composition-Cedar PERMITS it       (composition author - narrows this Composition)
    ∧ operator-Cedar PERMITS it          (operator - the central ceiling, --sandbox-policy-file)
```

`capability` ∈ { `pullModule` (repo of a `from`-chosen source), `spendCredential`,
`grantEgress` (host/method/path), `usePrivateTmp`, `setEnv`/read a credential-bound
env var }.

This unifies, and lets us delete, a pile of bespoke Input fields:

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

This is what an implementer must get right. The three layers do NOT share one
default; each default matches its trust role and preserves today's behaviour.

**Operator layer** (`--sandbox-policy-file`): **absent → deny-all.** Already true
after the flag-drop change. It is the enabler; no policy = only the default
sandbox.

**Manifest layer**: **absent → requests nothing → default sandbox.** `path`/`http`
and manifest-less OCI carry no request; a fallback inline `request` is designed
separately (docs/one-pager-manifest-less-sources.md).

**Composition layer** (`compositionPolicy`): two regimes, by capability, so it is
a *narrower* for sandbox and a *required fence* for XR-chosen sources - exactly
what `sandbox` and the allowlists do today:

- Sandbox capabilities (`grantEgress`, `usePrivateTmp`, `setEnv`): **scoped
  default-permit.** If the composition policy is absent, or contains no rule
  scoping that action, the layer permits (no narrowing - operator ∧ manifest
  govern). If it contains any rule for that action, that action becomes
  default-deny *within the composition layer* (the author has opted into
  narrowing it). Detected the way `HasSignatureRules`/`HasPrivateTmpRules` scan
  for an action today.
- Source fencing (`pullModule`, `spendCredential`) for a `from`-chosen source:
  **default-deny.** A `from` source is refused unless the composition policy
  explicitly permits its repository (and any credential), preserving today's rule
  that `module.from` *requires* a `repositoryAllowList` (and `credentialsAllowList`
  only within it). A **static** source (the Composition names the module itself)
  is not gated by this layer - it is the Composition's own choice, as today.

Worked table (S = sandbox cap, F = from-source fence):

| composition policy state | S: grantEgress/usePrivateTmp/setEnv | F: pullModule/spendCredential (from) |
|---|---|---|
| absent | permit (operator ∧ manifest decide) | deny (from-source refused) |
| present, no rule for the action | permit | deny |
| present, has `permit` for it | permit if matched, else deny | permit if matched, else deny |
| present, has `forbid` for it | forbid wins | forbid wins |

## The env model (#3)

`sandbox.env`/`sandbox.envFrom` leave the Input entirely.

- **Non-secret config** (`LOG_LEVEL=debug`) was never really env - it is
  deployment config. It moves to the existing `config` field; the guest already
  reads per-instance config through `wasmfn.GetConfig`. No env, no ceiling, no
  credential.
- **Secret env** (a library that only reads `$DATABASE_URL`): the **manifest**
  declares the binding - `requires.env: [{name: DATABASE_URL, fromCredential:
  {name: db, key: url}}]` - so the module owns its own env contract. At run time
  `Materialize` (internal/sandbox) resolves it from the request's step
  credentials exactly as `valueFrom.credential` does today (reuse
  `credentialData`, the withheld-pull-credential and NUL guards unchanged), gated
  by `spendCredential(db)` in **both** Cedar layers. The credential VALUE still
  arrives at the pipeline step; the module never authors it. The
  bulk-import-every-key `envFrom` shape is dropped - a binding names exactly the
  keys it needs, which was the safer half anyway.

So `env.valueFrom.credential` and `envFrom.credential` both disappear from the
Input; their one useful behaviour (inject a named credential key as a named env
var) survives as a manifest binding, gated by Cedar. That closes the duplication
question and the "a module cannot request env for itself" gap in one move.

## Cedar schema (unchanged, reused for both layers)

Both the operator and composition policies compile against the **same** schema
already in `internal/authz`: actions `pullModule`, `spendCredential`,
`grantEgress`, `usePrivateTmp`, `setEnv`, `requireSignature`, `dialAddress`;
entities `Repository`, `HostPattern`, `Capability`, `Credential`; principal
`Request { namespace, xrKind, composition }`. The composition policy is compiled
from the Input's `compositionPolicy` text and **cached by content hash** (it is
per-Composition, not per-request). A malformed `compositionPolicy` is a fatal
result at admission, in the runtime's words, the same as a malformed operator
policy is a startup/validate error.

## Implementation shape (for the implementer)

1. `input/v1beta1/input.go`: delete `Policy` and `Sandbox`; add
   `CompositionPolicy string` (raw Cedar; `+optional`). Regenerate the CRD. Keep
   `Module`, `Limits`, `Config`.
2. `internal/authz`: add `CompositionPolicy` (compile-from-bytes, content-hash
   cache) with `Permits*` mirroring `OperatorPolicy`, plus the scoped-default
   helpers (`ScopesAction`). Add a small `Decision` helper that ANDs
   operator ∧ composition ∧ (manifest request) per capability with the defaults
   above.
3. `internal/manifest`: extend `Requires` with `Env []EnvBinding{Name,
   FromCredential{Name,Key}}`. `Check`/`Sandbox`/`Validate` learn env bindings.
4. `internal/sandbox`: `Materialize` sources env from the manifest bindings +
   request credentials (not the Input); drop the Input env/envFrom paths.
5. `internal/admission`: replace the `policy`/`sandbox` checks with the
   three-layer `Decision`. `module.FromComposite`/`policy.go`: the from-source
   fence becomes the composition-layer `pullModule`/`spendCredential` default-deny.
6. `cmd/function/fn.go`, `validate.go`: build both policy layers; carry the
   composition policy from the Input.
7. Tests to green. **Defer** (call out, do not attempt in this pass): the inline
   `request` for manifest-less sources (still Draft), all `guestfn` changes
   (scaffold/build/push/manifest), the examples, and the full doc scrub.

## Migration / back-compat

Breaking, and large - stacked on the flag-drop change. An existing Composition
using `policy`/`sandbox` fails to decode (strict) or is refused; `function
validate` names the removed field and the replacement. Worth a `vX.0` and a
migration note; `guestfn` should learn to emit a `compositionPolicy` skeleton
from an old `sandbox`/`policy` block to ease the port (later).

## Non-goals

- Letting the manifest or the composition policy **widen** past the operator
  ceiling - the AND forbids it.
- Cedar for `limits` - a numeric clamp is not an authorization question.
- A permissive default for the operator or the from-source fence - both stay
  deny-by-default.
