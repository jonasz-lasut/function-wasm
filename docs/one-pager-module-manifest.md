# Module manifest

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.3 (`composes`, the advisory declaration
  of what a module writes, #174)

A module declares what it needs - the sandbox capabilities it can't run
without, the structure of the `config` it reads, the oldest runtime it
works on - in a manifest that travels beside the module as a second layer
of its OCI artifact. The runtime reads the layer before the run and fails
fast, with a message that identifies the module and the refusing policy
layer, when a `requires` entry isn't permitted or the `config` doesn't
match the module's schema; registries and `guestfn inspect` show what a
module needs before anyone runs it, and `guestfn scaffold composition`
writes the Input from it. A manifest never widens anything. It's the
*request* of the three-layer
decision (docs/one-pager-three-layer-authz.md), granted only where the
Input's `compositionPolicy` and the operator's `--sandbox-policy-file` both
permit. Revision 1.1 records that change: `requires` gained `env`
credential bindings and its check became the three-layer decision.
Revision 1.2 adds `requires.credentials`, the step credentials a module
reads whole from its request: the runtime now forwards a module only the
step credentials its manifest names and both layers permit. Modules
served as `path` or `http` sources have no artifact layer, but may specify
their `wasmfn.yaml` manifest by reference
(docs/one-pager-manifest-less-sources.md) - loaded through the same
`Ref.Manifest` seam and decided by the same layers.


## Before this

A module that needs egress in a Composition that grants none fails inside
the guest - `wasmfn: sandbox.egress: HTTP egress is not granted to this
module` wrapped in a `*url.Error` for a Go guest, whatever a Rust or TinyGo
helper makes of it, or a nil dereference in a careless one - after the
request was resolved, loaded, instantiated and half-run. Only the guest's
own check catches a wrong `config`: `wasmfn.GetConfig` decodes JSON, so a
wrong type is a guest-side fatal result and an unknown key passes with no
warning; a Rust guest with `serde` behaves the same, a
hand-rolled one differently. A registry doesn't say what a module needs; a
Composition author copies the `sandbox` block from a README, if there is
one. The runtime can't detect a module's need for a field in
`wasmfn.http`'s answer that its version doesn't have.

## Principles

1. **A manifest is a request, never a grant.** Its only effects are an
   earlier, clearer refusal, documentation and scaffolding. The
   composition and operator policy layers still grant, and either can
   refuse.
2. **Beside the module, inside the artifact.** The manifest is a layer of
   the same OCI artifact as the module, so it's named by the manifest
   digest a Composition pins and covered by a cosign signature of that
   digest; the module bytes themselves are untouched, and nothing parses
   WebAssembly to find it (Wasmtime remains the only decoder of a module).
3. **Absent is fine.** A module without a manifest behaves exactly as
   today; unknown top-level fields are ignored, an unknown entry under
   `requires` fails closed.
4. **Language-agnostic.** `guestfn` writes it from a file next to the
   project, independent of any SDK or toolchain feature, so Go, TinyGo and
   Rust guests are equal.

## Shape

The source is `wasmfn.yaml` in the project (scaffolds include one):

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
  credentials: [cmdb]                    # step credentials read whole from the request
config:
  schema:                                # JSON Schema 2020-12, inline; validates the Input's config
    type: object
    properties: {greeting: {type: string}, greetingUrl: {type: string, format: uri}}
    additionalProperties: false
minRuntime: v0.2.0                       # oldest function-wasm runtime that serves this module
```

**The layer.** `guestfn push` adds the manifest as UTF-8 JSON in a layer
of media type `application/vnd.wasmfn.manifest.v1+json` after the
`application/wasm` layer and lists both digests in the artifact config's
`layerDigests`. It also sets the standard `org.opencontainers.image.{title,
version, source, description, revision}` annotations from the manifest and
`--revision` for registry UIs. At most 64 KiB; the resolver picks the
module layer as before (a layer of media type `application/wasm`, else the
only layer that's not the manifest layer). `requires.egress.http` reuses
`egress.HTTPRule`, so a `requires` rule and a compiled grant compare like
with like; `requires.env` binds variable names to step-credential keys and
`requires.credentials` names the step credentials the module reads whole
from its request - the values still arrive at the pipeline step, never in
the manifest, and the request the module receives contains only the
credentials these two name (the runtime edits every other one out of it).
`oras push fn.wasm:application/wasm wasmfn.json:application/vnd.wasmfn.manifest.v1+json`
produces the same artifact.

**Who writes it.** `guestfn build` reads `wasmfn.yaml` when present and
validates it (strict parse: unknown fields refused, egress rules through
`internal/egress.ValidateRules`, env bindings through
`internal/sandbox.ValidateBindings`, the schema compiled) - a
project with a bad manifest doesn't build - and checks the scaffold's
example `config` against the schema; `guestfn push` reads it again and adds
the layer. It's a data file, not something declared in the guest's code:
anything in code would need instantiating the module to read, and the manifest
must be readable without running it (and by every language equally).

**Decided by the policy layers.** In `RunFunction`, after admission and
the load, before `Run`: `admission.AdmitRequires` decides every
`requires` entry by the three-layer rule - each egress rule (once per rule and
method, so a policy can key on `context.method`), `filesystem.privateTmp`,
each env binding (`setEnv` over the bound names, `spendCredential` per
binding) and each required credential (`spendCredential`) must be
permitted by the Input's `compositionPolicy` (scoped
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
compositionPolicy does not permit`), `… requires a private /tmp
(requires.filesystem.privateTmp), which the operator policy
(--sandbox-policy-file) does not permit for this request`, `…
requires runtime v0.3.0 or newer, this is v0.2.1` and `config does not match
the module's schema: /greeting: got number, want string`. The runtime checks the
composition layer first, whole: the author closest to the fix reads their
own layer's refusal even where the operator would also deny. Only
narrowing: a manifest can make a run fail earlier; it can't make a run
possible.

**`guestfn scaffold composition [--from fn.wasm | <ref>] [--name greeter]
[--function-name function-wasm]`** prints a Composition step (or, with
`--full`, a Composition like the scaffold's `example/composition.yaml`)
whose `module` is `Path` for a file or the pinned `OCI` reference, whose
`config` is a skeleton from the schema (required keys with placeholders,
defaults where the schema has them) and whose `limits` are commented - the
module's sandbox needs are its manifest's `requires`, granted by the
policy layers, never copied into the Input. `guestfn push` prints the
`requires:` block under the `module:` block it prints today whenever the
manifest requires anything, which shows a Composition author what the
policy layers must permit.

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
- `internal/module`: `Ref.Manifest(ctx) ([]byte, bool, error)` - the OCI
  resolver fetches the artifact's manifest, picks the manifest layer
  (`ManifestLayer`), reads it through the blob store verified against its
  digest and bounded to `manifest.MaxSize`; `path` and `http` sources
  report none; `WasmLayer` skips the manifest layer when looking for the
  only layer.
- `cmd/function/fn.go`: `AdmitRequires` + `checkManifestGrants` between
  `load` and `Run`;
  `manifestFor` reads memory → the on-disk store `manifests/<digest>`
  (`cache.ManifestsDir`, opened with the other two, swept with them; an
  empty entry means "no manifest") → `Ref.Manifest`, so with a warm volume
  the registry is never contacted; the parsed manifest and compiled schema
  live in a `sync.Map` per process. `warm.go` logs a warmed module's
  requirements at debug; `validate --resolve` reads the same manifest,
  prints its `Summary()` on the resolved line and applies the same decision
  with the same refusal.
- `cmd/guestfn`: `build` validates `wasmfn.yaml` and the example config;
  `push [--manifest f] [--module-version v] [--revision r]` adds the layer
  and the annotations and prints the `requires:` block so a Composition
  author knows what the policy layers must permit;
  `inspect <ref>` and `manifest show <ref>` read the layer, `manifest
  validate [wasmfn.yaml]` checks the file; `scaffold composition [--from
  fn.wasm|<ref>] [--manifest f] [--name] [--function-name] [--full]` prints
  a step or a Composition from a manifest (`module` pinned, a `config`
  skeleton from the schema's top-level properties);
  the three template sets and examples include a `wasmfn.yaml` with a
  `config.schema` for `greeting`/`greetingUrl` and no `requires`
  (`--version` can't be a subcommand flag: `kong`'s root `--version` takes
  it, so the flag is `--module-version`).
- Tests: `internal/manifest` table tests for `Parse`, `Load`, `Validate`,
  `Check` (each `requires` entry covered and missing, pattern under pattern,
  method subset, prefix rules, `minRuntime`, the schema messages);
  `internal/module` `TestRefManifest`; `cmd/function`
  `TestRunFunctionManifest` (artifacts pushed to an in-memory registry with
  a manifest layer: granted, ungranted, ceiling-first, config, bad
  manifests, the store read with the registry gone) and
  `TestValidateResolveManifest`, and `cmd/guestfn` push/inspect/scaffold
  tests.

## Trust and threat notes

The manifest is a request both policy layers must permit, so
it can refuse a run or document a need. It never grants or widens, and it
never affects another module's run (there is no "denies" list). It travels
in the artifact whose manifest digest the Composition pins: whoever can
change it can change the module, and `--cosign-key` covers both.
`config.schema` runs a validator over Composition-authored data. The schema
is bounded (64 KiB), offline (`$ref` to URLs refused), RE2 only and
compiled once per digest, which limits what a hostile schema costs to a
bounded parse and a refusal. Reading the manifest parses JSON, never
WebAssembly. Fail-closed on unknown `requires.*` means a module built for
a newer runtime is refused with a message, not run without a capability it
counted on. These checks never read the composite resource.

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
  section, mirrored to an annotation; reading it needed a walker over
  WebAssembly sections beside Wasmtime, which the admission-tooling review
  had just ruled out. As a second layer of the artifact the manifest is
  covered by the same digest and signature, doesn't need a parser at all,
  and `oras push` can produce it. The cost accepted: `path` and `http`
  sources have no artifact layer, so they name their `wasmfn.yaml` by
  reference instead (`manifestURL`/`manifestDigest`, `manifestPath`;
  docs/one-pager-manifest-less-sources.md), and `guestfn manifest set` -
  embedding into an existing module - has no meaning and doesn't exist.
- **Environment variables aren't a `requires` entry** (Jonasz, 2026-08-17;
  **superseded** by the three-layer model, 2026-08-19): the draft let a
  module list the `env` keys it reads; dropped before release, because
  environment values were then the Composition's. The three-layer change
  reversed this on purpose: with the Input's env fields gone, the
  manifest is where a module declares its env contract - `requires.env`
  binds a variable to a step-credential key, allowed only where `setEnv`
  and `spendCredential` permit it in both Cedar layers, and the value still
  arrives at the pipeline step, never in the manifest.
- **Home of the manifest**: the artifact, source of truth; `guestfn push` derives the
  standard OCI annotations from it for registry UIs, never read by the
  runtime.
- **Writer**: `guestfn` only, from `wasmfn.yaml`; a guest's own glue never
  writes it.
- **`requires` is hard**: an unmet entry refuses the run, and an
  `optional: true` per rule can be added later without breaking anything.
- **`config.schema` enforced by the runtime**, not only by `guestfn`: a Go
  guest already fails on a bad config, only later and worse; the
  per-request cost is microseconds with the schema compiled once per
  digest, and the failure identifies the field.
- **Source filename**: `wasmfn.yaml`.
