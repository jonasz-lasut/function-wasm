# Module Manifest

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.1

A module declares what it needs — the sandbox capabilities it cannot run
without, the shape of the `config` it reads, the oldest runtime it works on
— in a manifest that travels beside the module as a second layer of its OCI
artifact. The runtime reads the layer before the run and fails fast, with a
message naming the module and the refusing policy layer, when a requirement
is not permitted or the `config` does not match the module's schema;
registries and `guestfn inspect` show what a module wants before anyone
runs it; `guestfn scaffold composition` writes the Input from it. A
manifest never widens anything: it is the *request* of the three-layer
decision (docs/one-pager-three-layer-authz.md), granted only where the
Input's `compositionPolicy` and the operator's `--sandbox-policy-file` both
permit. Revision 1.1 records that change: `requires` gained `env`
credential bindings and its check became the three-layer decision. Modules
served as `path` or `http` sources have no artifact layer, but may name
their `wasmfn.yaml` manifest by reference
(docs/one-pager-manifest-less-sources.md) - loaded through the same
`Ref.Manifest` seam and decided by the same layers.


## Before this

A module that needs egress in a Composition that grants none fails inside
the guest — `wasmfn: sandbox.egress: HTTP egress is not granted to this
module` wrapped in a `*url.Error` for a Go guest, whatever a Rust or TinyGo
helper makes of it, or a nil dereference in a careless one — after the
request was resolved, loaded, instantiated and half-run. A wrong `config` is
caught only where the guest checks: `wasmfn.GetConfig` decodes JSON, so a
wrong type is a guest-side fatal result and an unknown key is silently
ignored; a Rust guest with serde behaves the same, a hand-rolled one
differently. Nothing in a registry says what a module needs; a Composition
author copies the `sandbox` block from a README, if there is one. The
runtime cannot tell a module needs `wasmfn.http`'s answer to carry a field
its version does not have.

## Principles

1. **A manifest is a request, not a grant.** Its only effects are an
   earlier, clearer refusal, documentation and scaffolding. The
   composition and operator policy layers still grant, and either can
   refuse.
2. **Beside the module, inside the artifact.** The manifest is a layer of
   the same OCI artifact as the module, so it is named by the manifest
   digest a Composition pins and covered by a cosign signature of that
   digest; the module bytes themselves are untouched, and nothing parses
   WebAssembly to find it (wasmtime stays the only decoder of a module).
3. **Absent is fine.** A module without a manifest behaves exactly as
   today; unknown top-level fields are ignored, an unknown *requirement*
   fails closed.
4. **Language-agnostic.** `guestfn` writes it from a file next to the
   project; no SDK, no toolchain feature, so Go, TinyGo and Rust guests are
   equal.

## Shape

The source is `wasmfn.yaml` in the project (scaffolds ship one):

```yaml
abi: 1                                   # required; the export set of docs/abi.md
name: greeter                            # → org.opencontainers.image.title
version: 0.1.0                           # → …image.version; guestfn push --module-version overrides
source: https://github.com/example/greeter   # → …image.source (optional)
requires:                                # what the run must be granted; each optional
  egress:
    http:                                # the egress package's HTTPRule shape, verbatim
    - host: api.example.com
      methods: [GET]
      pathPrefix: /v1/
  filesystem: {privateTmp: true}
  env:                                   # secret env: bindings to step-credential keys
  - name: DATABASE_URL
    fromCredential: {name: db, key: url}
config:
  schema:                                # JSON Schema 2020-12, inline; validates the Input's config
    type: object
    properties: {greeting: {type: string}, greetingUrl: {type: string, format: uri}}
    additionalProperties: false
minRuntime: v0.2.0                       # oldest function-wasm runtime that serves this module
```

**The layer.** `guestfn push` adds the manifest as UTF-8 JSON in a layer
of media type `application/vnd.wasmfn.manifest.v1+json` after the
`application/wasm` layer, lists both digests in the artifact config's
`layerDigests`, and sets the standard `org.opencontainers.image.{title,
version, source, description, revision}` annotations from the manifest and
`--revision` for registry UIs. At most 64 KiB; the resolver picks the
module layer as before (a wasm-typed layer, else the only layer that is not
the manifest layer). `requires.egress.http` reuses `egress.HTTPRule`,
so a requirement and a compiled grant compare like with like;
`requires.env` binds variable names to step-credential keys - the value
still arrives at the pipeline step, never in the manifest.
`oras push fn.wasm:application/wasm wasmfn.json:application/vnd.wasmfn.manifest.v1+json`
produces the same artifact.

**Who writes it.** `guestfn build` reads `wasmfn.yaml` when present and
validates it (strictly: unknown fields refused, egress rules through
`internal/egress.ValidateRules`, env bindings through
`internal/sandbox.ValidateBindings`, the schema compiled) — a
project with a bad manifest does not build — and checks the scaffold's
example `config` against the schema; `guestfn push` reads it again and adds
the layer. Not `pkg/wasmfn`: TinyGo and Rust guests do not use `wasmfn`,
and anything declared in code would need instantiation to read.

**Decided by the policy layers.** In `RunFunction`, after admission and
the load, before `Run`: `admission.AdmitRequires` decides every
requirement by the three-layer rule - each egress rule (once per rule and
method, so a policy can key on `context.method`), `filesystem.privateTmp`
and each env binding (`setEnv` over the bound names, `spendCredential` per
binding) must be permitted by the Input's `compositionPolicy` (scoped
default-permit: it narrows only the actions it writes rules for) and by
the operator's `--sandbox-policy-file` (default-deny) - then
`checkManifestGrants` holds the rest of the manifest against the run:

- `abi` == 1; `minRuntime` ≤ the runtime's version from its build info (a
  `(devel)` runtime passes);
- `config.schema` → the Input's `config` (absent = `{}`) validates.

A miss is a fatal result, outcome `refused`, before the module runs:
`module oci ghcr.io/example/greeter@sha256:3f2a… requires egress GET to
host "api.example.com" (requires.egress.http[0]), which the operator
policy (--sandbox-policy-file) does not permit` (or `… which the
compositionPolicy does not permit`); `… requires a private /tmp
(requires.filesystem.privateTmp), which the operator policy
(--sandbox-policy-file) does not permit for this request`; `…
requires runtime v0.3.0 or newer, this is v0.2.1`; `config does not match
the module's schema: /greeting: got number, want string`. The composition
layer is checked first, whole: the author closest to the fix reads their
own layer's refusal even where the operator would also deny. Only
narrowing: a manifest can make a run fail earlier; it cannot make a run
possible.

**`guestfn scaffold composition [--from fn.wasm | <ref>] [--name greeter]
[--function-name function-wasm]`** prints a Composition step (or, with
`--full`, a Composition like the scaffold's `example/composition.yaml`)
whose `module` is `Path` for a file or the pinned `OCI` reference, whose
`config` is a skeleton from the schema (required keys with placeholders,
defaults where the schema has them) and whose `limits` are commented — the
module's sandbox needs are its manifest's `requires`, granted by the
policy layers, never copied into the Input. `guestfn push` prints the
`requires:` block under the `module:` block it prints today whenever the
manifest requires anything, so a Composition author knows what the policy
layers must permit.

## Mechanics

- `internal/manifest`: `Manifest` types with JSON tags, `Load` (the YAML
  file, strict), `Parse` (the layer: unknown top-level fields ignored, an
  unknown `requires` field refused, size capped), `Validate` (egress rules
  through `egress.ValidateRules`, env bindings through
  `sandbox.ValidateBindings`,
  semver, the schema compiled once with `$ref`s to URLs refused), `Check(g
  Grants, config, runtimeVersion)`, `ValidateConfig`, `Summary()`, `JSON()`,
  `RuntimeVersion()`, `LayerMediaType`, `MaxSize`, `FileName`. JSON Schema:
  `github.com/santhosh-tekuri/jsonschema/v6` (pure Go, draft 2020-12), a
  loader that refuses every URL, `json.Number` for instances, the first
  leaf error rendered as `<json pointer>: <message>`.
- `internal/module`: `Ref.Manifest(ctx) ([]byte, bool, error)` — the OCI
  resolver fetches the artifact's manifest, picks the manifest layer
  (`ManifestLayer`), reads it through the blob store verified against its
  digest and bounded to `manifest.MaxSize`; path and http sources report
  none; `WasmLayer` skips the manifest layer when looking for the only
  layer.
- `cmd/function/fn.go`: `AdmitRequires` + `checkManifestGrants` between
  `load` and `Run`;
  `manifestFor` reads memory → the on-disk store `manifests/<digest>`
  (`cache.ManifestsDir`, opened with the other two, swept with them; an
  empty entry means "no manifest") → `Ref.Manifest`, so a warm volume asks
  the registry nothing; the parsed manifest and compiled schema live in a
  `sync.Map` per process. `warm.go` logs a warmed module's requirements at
  debug; `validate --resolve` reads the same manifest, prints its
  `Summary()` on the resolved line and applies the same decision with the
  same refusal.
- `cmd/guestfn`: `build` validates `wasmfn.yaml` and the example config;
  `push [--manifest f] [--module-version v] [--revision r]` adds the layer
  and the annotations and prints the `requires:` block so a Composition
  author knows what the policy layers must permit;
  `inspect <ref>` and `manifest show <ref>` read the layer, `manifest
  validate [wasmfn.yaml]` checks the file; `scaffold composition [--from
  fn.wasm|<ref>] [--manifest f] [--name] [--function-name] [--full]` prints
  a step or a Composition from a manifest (`module` pinned, a `config`
  skeleton from the schema's top-level properties);
  the three template sets and examples carry a `wasmfn.yaml` with a
  `config.schema` for `greeting`/`greetingUrl` and no `requires`
  (`--version` cannot be a subcommand flag: kong's root `--version` takes it,
  hence `--module-version`).
- Tests: `internal/manifest` table tests for `Parse`, `Load`, `Validate`,
  `Check` (each requirement covered and missing, pattern under pattern,
  method subset, prefix rules, `minRuntime`, the schema messages);
  `internal/module` `TestRefManifest`; `cmd/function`
  `TestRunFunctionManifest` (artifacts pushed to an in-memory registry with
  a manifest layer: granted, ungranted, ceiling-first, config, bad
  manifests, the store read with the registry gone) and
  `TestValidateResolveManifest`; `cmd/guestfn` push/inspect/scaffold tests.

## Trust and threat notes

The manifest is a request both policy layers must permit, so
it can refuse a run or document a need — never grant, never widen, never
touch another module's run (there is no "denies" list). It travels in the
artifact whose manifest digest the Composition pins: whoever can change it
can change the module, and `--cosign-key` covers both. `config.schema`
runs a validator over Composition-authored data: the schema is bounded
(64 KiB), offline (`$ref` to URLs refused), RE2 only, compiled once per
digest, so a hostile schema costs a bounded parse and a refusal. Reading
the manifest parses JSON, never WebAssembly. Fail-closed on unknown
`requires.*` means a module built for a newer runtime is refused with a
message, not run without a capability it counted on. Nothing here reads
anything from the composite resource.

## Phasing

| phase | what | release |
|---|---|---|
| 1 | `internal/manifest`, `wasmfn.yaml` in the scaffolds, `guestfn build` validation, `push` layer + annotations + the printed `sandbox:` block, `inspect`, `manifest show` | v0.2 |
| 2 | runtime: `Ref.Manifest`, the manifests store, `checkManifest`, `validate --resolve` | v0.2 |
| 3 | `config.schema` in the runtime and in `guestfn` | v0.2 |
| 4 | `guestfn scaffold composition` | v0.2 |

## Decisions

- **An OCI layer, not a custom section** (Jonasz, 2026-08-17). The draft
  put the manifest inside the module as a `wasmfn.manifest` custom
  section, mirrored to an annotation; reading it needed a WebAssembly
  section walker beside wasmtime, which the admission-tooling review had
  just ruled out. As a second layer of the artifact the manifest is
  covered by the same digest and signature, needs no parser at all, and
  `oras push` can produce it. The cost accepted: `path` and `http` sources
  carry no artifact layer, so they name their `wasmfn.yaml` by reference
  instead (`manifestURL`/`manifestDigest`, `manifestPath`;
  docs/one-pager-manifest-less-sources.md), and `guestfn manifest set` —
  embedding into an existing module — has no meaning and does not exist.
- **Environment variables are not a requirement** (Jonasz, 2026-08-17;
  **superseded** by the three-layer model, 2026-08-19): the draft let a
  module list the `env` keys it reads; dropped before it shipped, because
  environment values were then the Composition's. The three-layer change
  reversed this deliberately: with the Input's env fields gone, the
  manifest is where a module declares its env contract - `requires.env`
  binds a variable to a step-credential key, gated by `setEnv` and
  `spendCredential` in both Cedar layers, and the value still arrives at
  the pipeline step, never in the manifest.
- **Home of the manifest**: the artifact, source of truth; the standard OCI
  annotations are derived from it for registry UIs, never read by the
  runtime.
- **Writer**: `guestfn` only, from `wasmfn.yaml`; `pkg/wasmfn` stays out.
- **`requires` is hard**: an unmet requirement refuses the run; an
  `optional: true` per rule can be added later without breaking anything.
- **`config.schema` enforced by the runtime**, not only by `guestfn`: a Go
  guest already fails on a bad config, only later and worse; the
  per-request cost is microseconds with the schema compiled once per
  digest, and the failure names the field.
- **Source file name**: `wasmfn.yaml`.
