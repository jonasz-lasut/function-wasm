# Module manifest

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.4 (rewritten in the present tense over the
  Rust workspace; merges in the manifest-less-sources record, revision 0.1,
  whose manifests by reference are a section here)

A module declares what it needs in a manifest: the sandbox capabilities it
can't run without, the schema of the `config` it reads and the oldest runtime
it works on, plus, advisory only, the kinds it composes. For an OCI artifact
the manifest is a second layer of the artifact. A `Path` or `HTTP` source
names its `wasmfn.yaml` by reference. The runtime reads it before the run and fails
fast, with a message that identifies the module and the refusing policy layer,
when a `requires` entry isn't permitted or the `config` doesn't match the
module's schema. Registries and `guestfn inspect` show what a module needs
before anyone runs it, and `guestfn scaffold composition` writes the Input
from it.

A manifest never widens anything. It's the request of the
[three-layer decision](one-pager-three-layer-authz.md), granted only where the
Input's `compositionPolicy` and the operator's `--sandbox-policy-file` both
permit. The manual page is [Module manifests](guests/module-manifests.md).

## Principles

1. **A manifest is a request, never a grant.** Its only effects are an
   earlier, clearer refusal, documentation and scaffolding. The composition
   and operator policy layers still grant, and either can refuse.
2. **Beside the module, inside the artifact.** The manifest is a layer of the
   same OCI artifact as the module, so the manifest digest a Composition pins
   names it and a cosign signature of that digest covers it. The module bytes
   are untouched, and nothing parses WebAssembly to find it (Wasmtime is the
   only decoder of a module).
3. **Absent is fine.** A module without a manifest gets the default sandbox.
   Unknown top-level fields are ignored in a published manifest, and an
   unknown entry under `requires` fails closed.
4. **Language-agnostic.** `guestfn` writes it from a file next to the project,
   independent of any SDK or toolchain feature, so every guest language is
   equal.

## Shape

The source is `wasmfn.yaml` in the project (every scaffold includes one):

```yaml
abi: 2                                   # required; the world of docs/abi-v2.md
name: greeter                            # → org.opencontainers.image.title
version: 0.1.0                           # → …image.version; guestfn push --module-version overrides
source: https://github.com/example/greeter   # → …image.source (optional)
requires:                                # what the run must be granted; each optional
  egress:
    http:                                # the same rule shape as the Input's egress rules
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
composes:                                # advisory: what the module writes
- kind: policy/PodDisruptionBudget
  changes: [add]
```

**The layer.** `guestfn push` adds the manifest as UTF-8 JSON in a layer of
media type `application/vnd.wasmfn.manifest.v1+json` after the
`application/wasm` layer and lists both digests in the artifact config's
`layerDigests`. It also sets the standard `org.opencontainers.image.*`
annotations (`title`, `version`, `source`, `description`, `revision`) from the
manifest and `--revision`, for registry UIs. The runtime never reads them. A
manifest is at most 64 KiB. `requires.egress.http` takes the same rules as the
Input's egress rules, so a request and a compiled grant compare like with
like. `requires.env` binds variable names to step-credential keys and
`requires.credentials` names the step credentials the module reads whole from
its request. The values arrive at the pipeline step, never in the manifest,
and the request the module receives contains only the credentials these two
name: the runtime edits every other one out of it. `oras push
fn.wasm:application/wasm
wasmfn.json:application/vnd.wasmfn.manifest.v1+json` produces the same
artifact.

**Who writes it.** `guestfn build` reads `wasmfn.yaml` when present and
validates it, so a project with a bad manifest doesn't build. The parse is
strict and refuses unknown fields, and the egress rules and env bindings pass
the runtime's own checks. `guestfn push` reads it again and
adds the layer. The manifest is a data file, not something declared in the
guest's code: anything in code would need instantiating the module to read,
and the manifest must be readable without running the module. A guest's own glue never writes it.

**Decided by the policy layers.** In the request path, after admission and the
load and before the run, `admission::admit_requires` decides every `requires`
entry by the three-layer rule. Each egress rule (once per rule and method, so
a policy can key on `context.method`), `filesystem.privateTmp`, each env
binding (`setEnv` over the bound names, `spendCredential` per binding) and
each required credential (`spendCredential`) must be permitted by the Input's
`compositionPolicy` (scoped default-permit: it narrows only the actions it
writes rules for) and by the operator's `--sandbox-policy-file`
(default-deny). Then `Manifest::check` holds the rest of the manifest against
the run:

- `abi` is 2, and `minRuntime` is no newer than the runtime's stamped version
  (a development build, which has none, passes);
- the Input's `config` (absent is `{}`) validates against `config.schema`.

A miss is a fatal result, counted as `refused`, before the module runs:

```text
module oci ghcr.io/example/greeter@sha256:3f2a… requires egress GET to host "api.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit
… which the compositionPolicy does not permit
… requires a private /tmp (requires.filesystem.privateTmp), which the operator policy (--sandbox-policy-file) does not permit for this request
… requires runtime v0.3.0 or newer, this is v0.2.1
… config does not match the module's schema: /greeting: got number, want string
```

The runtime checks the composition layer first, whole: the author closest to
the fix reads their own layer's refusal even where the operator would also
deny. A manifest can make a run fail earlier. It can't make a run possible.

**Declared output.** `composes` lists the `<group>/<Kind>` a module writes and
the changes it makes (`add`, `modify`, `remove`, all three when omitted). It's
advisory (#174). The runtime never reads it and judges the module's actual
output against the policy layers' `composeResource` rules, since a
refusal before the run would be unsound for rules conditioned on a resource's
name. The tooling reads it: `function validate --resolve` warns where a
declared change would be refused, `guestfn inspect` shows it and `guestfn
scaffold composition` writes the matching permits.

**`guestfn scaffold composition`** prints a Composition step, or with `--full`
a Composition, whose `module` is a `Path` for a file or the pinned `OCI`
reference, whose `config` is a skeleton from the schema (required keys with
placeholders, defaults where the schema has them) and whose `limits` are
commented. The module's sandbox needs are its manifest's `requires`, granted
by the policy layers and never copied into the Input. `guestfn push` prints
the `requires:` block under the `module:` block it prints, which shows a
Composition author what the policy layers must permit.

## Manifests by reference

A `Path` source (a `.wasm` file under `--module-dir`) or an `HTTP` source (a
file fetched by URL and digest) has no artifact to hold a layer, and an OCI
artifact pushed without a manifest layer has none either. With no manifest the
request is empty, so by the model's default-deny the module gets only the
default sandbox. That's safe, but it leaves an operator who legitimately runs a
`Path` or `HTTP` module that needs egress, a private `/tmp` or a
credential-sourced env var with no way to request it (tracked in
[#30](https://github.com/jonasz-lasut/function-wasm/issues/30)).

A manifest is only a request. A manifest-less source misses nothing but a
place to put its `wasmfn.yaml`: the document the module author already writes
and `guestfn push` embeds, stored beside the module instead of inside the
artifact. Such a source names it by reference:

```yaml
module:
  type: HTTP
  http:
    url: https://example.com/fn.wasm
    digest: sha256:...
    manifestURL: https://example.com/fn-manifest.yaml   # optional
    manifestDigest: sha256:...                            # pins it
---
module:
  type: Path
  path: fn.wasm
  manifestPath: fn-manifest.yaml                          # optional, under --module-dir
```

- The referenced file is a `wasmfn.yaml`. The resolver normalizes it to the
  JSON an OCI manifest layer contains, so `Manifest::parse` is the one decoder.
  It verifies `manifestURL` against `manifestDigest`, bounds the file to the
  manifest size limit and reads `manifestPath` under `--module-dir` with the
  same confinement as the module file.
- It arrives through the same `Resolver::manifest` call as an OCI manifest
  layer, so `admit_requires`, `Manifest::check` and `function validate
  --resolve` are unchanged. A referenced manifest meets the same three layers
  as an embedded one, and it can only refuse a run earlier, never make one
  possible. A file the runtime can't read or parse is a fatal result, `cannot
  read the manifest of module <desc>: …` or `module <desc> has an invalid
  manifest: …`.
- Caching follows the manifest's own identity, not the module's: the same
  module file may run with one manifest, another, or none. An `HTTP` manifest
  is content-addressed by `manifestDigest` and kept in the blob store. A
  `manifestPath` manifest is read fresh on every request and never cached, so
  the local loop (`--module-dir` and `crossplane render`) reflects an edit
  immediately.
- `manifestPath` is allowed with `type: Path` only. An OCI artifact takes no
  reference: its manifest is a layer.

### The `from` fence

When the composite resource chooses an `HTTP` source (`module.from`), a
`compositionPolicy` `pullModule` rule must permit the manifest's normalized
location, as it must the module URL; otherwise the XR author could point the
runtime at any host to `GET`. The refusal is `module.from: <field> of the
composite resource names manifestURL "<location>", which the compositionPolicy
does not permit (pullModule)`. A `manifestPath` has no host, so the fence
doesn't apply to it, and it comes from the Input only: `module.from` can't set
it, since an XR author chooses the module file, not what its manifest
requests.

## Mechanics

- `crates/function/src/manifest.rs` holds the `Manifest` types. `load` reads
  the `wasmfn.yaml` of a project and refuses unknown fields, `parse` decodes a layer (at most 64
  KiB, unknown top-level fields ignored, an unknown `requires` field refused),
  `validate` checks the egress rules, env bindings and credentials through the
  same functions the Input's use, `minRuntime` as semver and the schema
  compiled as JSON Schema 2020-12 with the `jsonschema` crate. `check` holds
  the manifest against the run's `Grants`, and `summary` is the line the CLIs
  print.
- `crates/function/src/resolver.rs`: `Resolver::manifest` returns the
  manifest as JSON bytes, empty when the source has none. The OCI case reads
  the artifact's manifest, picks the manifest layer and fetches it through the
  blob store, verified against the layer's digest and bounded to the size
  limit. The other two cases are in the preceding section.
- `crates/function/src/runner.rs` reads and parses the manifest between the
  load and the run, then calls `admit_requires` and `Manifest::check` in that
  order. `function validate --resolve` (`validate.rs`) reads the same manifest,
  prints its summary on the resolved line and applies the same decision with
  the same refusal.
- `crates/guestfn`: `build` validates `wasmfn.yaml`; `push [--manifest f]
  [--module-version v] [--revision r]` adds the layer and annotations;
  `inspect <ref>` and `manifest show <ref>` read the layer; `manifest validate
  [wasmfn.yaml]` checks the file; `scaffold composition` writes a step or a
  Composition. The flag is `--module-version` because the root `--version` is
  taken by the CLI's own.

## Trust and threat notes

The manifest is a request both policy layers must permit, so it can refuse a
run or document a need. It never grants or widens, and it never affects
another module's run (there is no "denies" list). In an artifact it travels
under the manifest digest the Composition pins: whoever can change it can
change the module, and `--cosign-key` covers both. A manifest by reference is
pinned where it can be: an `HTTP` manifest by `manifestDigest`. `config.schema`
runs a validator over Composition-authored data, and a manifest, and so its
schema, is bounded to 64 KiB. Reading the manifest parses JSON, never
WebAssembly. Failing closed on unknown `requires.*` means a module built for a
newer runtime is refused with a message, not run without a capability it
counted on. These checks never read the composite resource.

## Decisions

- **An OCI layer, not a custom section** (Jonasz, 2026-08-17). The manifest
  is a second layer of the artifact, so the same digest and signature cover
  it, no parser is needed, and `oras push` can produce it. The cost accepted:
  `Path` and `HTTP` sources have no layer, so they name their `wasmfn.yaml` by
  reference, and a `guestfn manifest set` that embeds into an existing module
  has no meaning and doesn't exist.
- **Environment variables are a `requires` entry** (Jonasz, 2026-08-19, which
  reversed 2026-08-17). The first design left environment values to the
  Composition. The three-layer change removed the Input's env fields, so the
  manifest is where a module declares its env contract: `requires.env` binds
  a variable to a step-credential key, allowed only where `setEnv` and
  `spendCredential` permit it in both Cedar layers.
- **`requires` is hard.** An unmet entry refuses the run. An `optional: true`
  per rule can be added later without breaking anything.
- **`config.schema` is enforced by the runtime**, not only by `guestfn`. A
  guest already fails on a bad config, only later and with a worse message;
  the runtime's refusal identifies the field.
- **The module author is the one request author for every source kind.** A
  manifest by reference keeps one author and one schema
  for what a module needs, at the cost of a second small file to publish or
  place.
- **The home of the manifest is the artifact or the referenced file.**
  `guestfn push` derives the OCI annotations from it for registry UIs and the
  runtime never reads them.
- **The source file is `wasmfn.yaml`.**

## Alternatives

- **Manifest-less means the default sandbox only.** The simplest option, and
  it nudges everyone to OCI and `guestfn push`. Rejected as the only answer:
  it breaks the local loop for any module that needs egress, and a module
  hosted over HTTP has no path to a capability at all.
- **An inline `request` block in the Input, valid only for manifest-less
  sources.** The Composition would author the request for these sources only,
  so a module would have two possible request authors and two schemas to keep
  aligned.
- **A sidecar `wasmfn.yaml` discovered beside a `Path` module.** Rejected for
  an explicit `manifestPath`: a source states what it loads, nothing is picked
  up implicitly, and one field pattern (`manifestURL`, `manifestPath`) covers
  both kinds.
- **The manifest as a `wasmfn.manifest` custom section of the module.**
  Reading it needed a walker over WebAssembly sections beside Wasmtime, which
  the admission-tooling design had ruled out (a second reader of a module).
- **Environment variables listed under `requires` as keys the module reads.**
  Dropped in the first design, when environment values were the
  Composition's, and superseded by the env bindings in the decisions.

## Non-goals

- Letting a referenced manifest grant anything. It's a request; the two Cedar
  layers decide. A permissive manifest on a runtime with no operator policy
  still gets the default sandbox.
- An inline `request` in the Input. If a use case appears for a
  Composition-authored request on a manifest-less source, it can be added
  later over the same `Requires` schema.
- A reference and an embedded manifest on the same OCI artifact.
  `manifestURL` and `manifestPath` are for the sources with no layer.
