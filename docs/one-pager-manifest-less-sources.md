# Manifests for Manifest-less Sources

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 0.1
* Tracking: https://github.com/jonasz-lasut/function-wasm/issues/30

## Context

The three-layer authorization model (docs/one-pager-three-layer-authz.md) makes a
capability grant the conjunction of three layers, down the trust ladder:

```
manifest REQUESTS it            (module author  - the ask)
  ∧ Composition-Cedar PERMITS it (composition author - narrows for this Composition)
  ∧ operator-Cedar PERMITS it    (operator - the central ceiling)
```

The **request** layer is the module's `wasmfn.yaml` manifest. For an OCI
artifact it travels as the `application/vnd.wasmfn.manifest.v1+json` layer, so
it is covered by the manifest digest a Composition pins and by a cosign
signature. But two of the three source kinds have no such layer:

- `module.type: Path` - a `.wasm` file under the runtime's `--module-dir`.
- `module.type: HTTP` - a `.wasm` fetched by URL+digest.
- `module.type: OCI` **without** a manifest layer (a bare `application/wasm`
  push, or a `FROM scratch` tar) is the third case, already handled: the
  manifest is optional and its absence means "no requirements".

With no manifest the request layer is empty, so - by the model's default-deny -
a manifest-less module gets **only the default sandbox** (nothing but the
request). That is correct and safe, but it strands operators who legitimately run
`path`/`http` modules and need egress, a private `/tmp`, or a credential-sourced
env var. This one-pager decides how a manifest-less source expresses its request
without weakening the trust model.

## The key observation

The request layer is *just a request* - it never grants anything; the two Cedar
layers still gate it. So a manifest-less source is missing nothing more than a
**place to put its `wasmfn.yaml`**: the same document a module author already
writes and `guestfn push` embeds as an OCI layer, only stored beside the module
instead of inside the artifact. Keeping the module author as the request author -
rather than moving the request into the Input - keeps one author per module and
one schema (`internal/manifest.Requires`) for "what a module needs".

## What shipped: manifest by reference

A `path` or `http` source may **name its manifest separately**, and the runtime
loads it as the request layer, gated by the identical two Cedar layers:

```yaml
module:
  type: HTTP
  http:
    url: https://example.com/fn.wasm
    digest: sha256:...
    manifestURL: https://example.com/fn-manifest.yaml   # optional
    manifestDigest: sha256:...                            # pins it, set together
---
module:
  type: Path
  path: fn.wasm
  manifestPath: fn-manifest.yaml                          # optional, under --module-dir
```

- The referenced file is a `wasmfn.yaml` (the same YAML `guestfn build` writes and
  `guestfn push` embeds). The resolver normalizes it to the JSON an OCI manifest
  layer already carries, so `internal/manifest.Parse` and the manifests store are
  unchanged: `manifestURL` is verified against `manifestDigest` and bounded to
  `manifest.MaxSize`; `manifestPath` is read under `--module-dir` with the same
  confinement as the module file.
- It reaches the runtime through the same `module.Ref.Manifest` seam an OCI
  manifest layer does, so `admission.AdmitRequires`, `checkManifestGrants` and
  `function validate --resolve` need no change - a referenced manifest is decided
  by the three layers exactly like an embedded one, and can only refuse a run
  earlier, never make one possible.
- The manifest caches key on the **manifest's own identity**
  (`module.Ref.ManifestKey`), not the module digest: a manifest-less source names
  its manifest separately, so the same module file may run with one manifest,
  another, or none. An http manifest keys by `manifestDigest` (content-addressed,
  cached); a path manifest is read **fresh every request** (its file may be
  edited between renders) and not cached, so the local-dev loop reflects an edit
  immediately.

### The `from` fence

When the composite resource chooses an http source (`module.from`), its
`manifestURL` is fenced like the module URL: the manifest's own normalized
location must be permitted by a `compositionPolicy` `pullModule` rule, or its
author could point the runtime at any host to `GET`. A static `manifestPath`
(Path) has no host and never reaches the fence. `manifestPath` itself is read
from the Input only, never through `module.from`: an XR author chooses the module
file, not what its manifest requests.

## Options considered

1. **Nothing - manifest-less means default sandbox only.** Simplest and safe;
   nudges everyone to OCI + `guestfn push`. Rejected as the *sole* answer: it
   breaks the local-dev loop (`--module-dir` + `crossplane render`) for any
   module that needs egress, and http-hosted modules have no path to a sandbox
   at all.

2. **Inline `request` block in the Input**, valid only for manifest-less sources,
   authored by the Composition. Considered and **not taken**: it makes the
   Composition author the request author for these sources only, so a module has
   two possible request authors and two schemas to keep aligned. Manifest by
   reference keeps the module author as the sole request author, at the cost of a
   second small artifact to publish (a URL) or place (a file).

3. **Sidecar `wasmfn.yaml` for `path` sources** - auto-discovery of a manifest
   beside the module. Rejected in favour of an **explicit** `manifestPath`: a
   source states what it loads, nothing is picked up implicitly, and the same
   field shape (`http.manifestURL`, `manifestPath`) covers both kinds.

4. **Manifest by reference (shipped).** `http.manifestURL`/`manifestDigest` and
   `manifestPath`, admitted only when the resolved source carries no embedded
   manifest, providing what an OCI manifest layer would. Both Cedar layers gate
   it unchanged.

## Non-goals

- Letting a referenced manifest **grant** anything. It is a request; the two
  Cedar layers decide. A permissive manifest on a runtime with no operator policy
  still gets the default sandbox.
- An inline `request` in the Input (option 2). If a use case appears for a
  Composition-authored request on a manifest-less source, it can be added later
  over the same `Requires` schema.
- A manifest by reference *and* an embedded one on the same OCI artifact.
  `manifestURL`/`manifestPath` are for the manifest-less kinds; an OCI artifact
  carries its manifest as a layer.
