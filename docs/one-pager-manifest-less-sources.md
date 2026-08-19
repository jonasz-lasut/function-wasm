# Sandbox Requests for Manifest-less Sources

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft
* Tracking: https://github.com/jonasz-lasut/function-wasm/issues/30 (deferred past 0.2)

## Context

The three-layer authorization model (docs/one-pager-three-layer-authz.md) makes a
capability grant the conjunction of three layers, down the trust ladder:

```
manifest REQUESTS it            (module author  - the ask)
  ∧ Composition-Cedar PERMITS it (composition author - narrows for this Composition)
  ∧ operator-Cedar PERMITS it    (operator - the central ceiling)
```

The **request** layer is the module's `wasmfn.yaml` manifest (the
`application/vnd.wasmfn.manifest.v1+json` OCI layer): egress rules, a private
`/tmp`, and env credential-bindings the module declares it needs.

But two of the three module source kinds carry **no manifest**:

- `module.type: Path` - a `.wasm` file under the runtime's `--module-dir`. The
  resolver never reads a manifest layer (there is no OCI artifact).
- `module.type: HTTP` - a `.wasm` fetched by URL+digest. Same: no artifact, no
  manifest layer.
- `module.type: OCI` **without** a manifest layer (a bare `application/wasm`
  push, or a `FROM scratch` tar) is the third case, already handled: the
  manifest is optional and its absence means "no requirements".

With no manifest, the request layer is empty, so - by the model's default-deny -
a manifest-less module gets **only the default sandbox** (nothing but the
request). That is correct and safe, but it strands operators who legitimately run
`path`/`http` modules and need egress, a private `/tmp`, or a credential-sourced
env var. This one-pager decides how a manifest-less source expresses its request
without weakening the trust model.

## The key observation

The request layer is *just a request* - it never grants anything; the two Cedar
layers still gate it. So **who authors the request only affects trust if the
request itself can cause harm, and it cannot** (a greedy request is denied by
Cedar). That frees us to source the request from a **more trusted** party than
the module when there is no manifest.

The natural such party is the **Composition author** - already trusted above the
module, already the author of the Input. So a manifest-less source may carry its
request **inline in the Input**, and it is admitted through the identical
two-Cedar-layer gate. For a manifest-carrying OCI source the manifest owns the
request and the inline field is refused - the request has exactly one author per
module.

## Options considered

1. **Nothing - manifest-less means default sandbox only.** Simplest and safe;
   nudges everyone to OCI + `guestfn push`. Rejected as the *sole* answer: it
   breaks the local-dev loop (`--module-dir` + `crossplane render`) for any
   module that needs egress, and http-hosted modules have no path to a sandbox
   at all.

2. **Inline `request` in the Input, valid only for manifest-less sources**
   (recommended). A small block mirroring the manifest's `requires`
   (`egress.http[]`, `filesystem.privateTmp`, `env[]` credential-bindings). It is
   the request layer for that source; both Cedar layers gate it unchanged. For a
   source that *does* carry a manifest, the field is a validation error - the
   manifest is authoritative, so a module can never have two conflicting request
   authors. Trust: the request is Composition-authored (more trusted than a
   module manifest), and gated identically, so the model is if anything stronger
   here.

3. **Sidecar `wasmfn.yaml` for `path` sources.** A manifest file beside the
   `.wasm` under `--module-dir` (operator-controlled), loaded as the request.
   Cheap and faithful for local dev, but only covers `path`, not `http`. Keep as
   an *additional* convenience for `path` (it reuses the exact manifest parser),
   not the general answer.

4. **Manifest-by-reference for `http`.** The Input's `http` source names a
   separate manifest URL/digest fetched alongside the module. Faithful but adds a
   second fetch, a second digest to pin, and a new failure mode. Deferred - option
   2 already covers `http`.

## Recommendation

- **Option 2 is the general mechanism**: an optional `module.request` (or
  top-level `request`) block, admitted only when the resolved source carries no
  manifest, providing what `requires` would. Both Cedar layers gate it exactly as
  they gate a manifest. A `request` on a manifest-carrying source is refused, as
  is a manifest-less source that asks for a capability without a `request`.
- **Option 3 (`path` sidecar) as a convenience**: if a `wasmfn.yaml` sits beside
  a `path` module, load it as the manifest (reusing `manifest.Load`); an inline
  `request` for that same source is then refused, keeping one author. This keeps
  the local-dev loop identical to production (the module ships its `wasmfn.yaml`
  either way).
- **Defer option 4** until a user actually hosts modules by URL and needs a
  manifest they cannot inline.

## Shape (sketch, not final)

```yaml
module:
  type: HTTP
  http: {url: https://example.com/fn.wasm, digest: sha256:...}
request:                     # allowed ONLY because this source has no manifest
  egress:
    http: [{host: api.example.com, methods: [GET]}]
  filesystem: {privateTmp: true}
  env: [{name: TOKEN, fromCredential: {name: api, key: token}}]  # a binding, never a literal
```

The `request` reuses the manifest's `Requires` types verbatim
(`internal/manifest`), so there is one schema for "what a module needs", authored
either by the module (manifest) or the Composition (inline) - never both.

## Non-goals

- Letting a `request` **grant** anything. It is a request; the two Cedar layers
  decide. A permissive `request` on a runtime with no operator policy still gets
  the default sandbox.
- Literal env values in `request`. Non-secret config is `config`
  (`wasmfn.GetConfig`); `request.env` carries credential *bindings* only, like the
  manifest (docs/one-pager-three-layer-authz.md, the env model).
- A `request` field on a manifest-carrying source. One module, one request
  author.

## Open questions

- Field placement: `module.request` (grouped with the source it annotates) vs a
  top-level `request` sibling of `module`. Leaning `module.request`, since it is
  meaningful only for the source and must not appear for a manifest-carrying one.
- Does `validate --resolve` surface "this source is manifest-less, so `request`
  is required to grant it anything"? Almost certainly yes - it is the runtime's
  own admission, and the message should name the missing `request`.
