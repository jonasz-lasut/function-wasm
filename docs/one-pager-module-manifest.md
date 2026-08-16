# Module Manifest

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft

A module declares what it needs — the sandbox grants it cannot run without,
the shape of the `config` it reads, the oldest runtime it works on — inside
itself, as a `wasmfn.manifest` custom section, mirrored into OCI
annotations when it is pushed. The runtime reads the section before the run
and fails fast, with a message naming the module and the missing grant,
when the Composition granted less than the module requires; registries and
`guestfn inspect` show what a module wants before anyone runs it; `guestfn
scaffold composition` writes the Input from it. A manifest never widens
anything: grants stay the Composition's within the operator's ceiling. It
depends on the reader of `docs/one-pager-admission-tooling.md`. Nothing
here gates v0.1.0.


## Today

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

1. **A manifest is a requirement, not a grant.** Its only effects are an
   earlier, clearer refusal, documentation and scaffolding. The
   Composition still asks, the operator still caps.
2. **Inside the signed bytes.** The section is part of the module, so it is
   covered by the layer digest, the manifest digest, `http.digest` and a
   cosign signature; the annotations are a copy for people and UIs.
3. **Absent is fine.** A module without a manifest behaves exactly as
   today; unknown top-level fields are ignored, an unknown *requirement*
   fails closed.
4. **Language-agnostic.** `guestfn` writes it from a file next to the
   project; no SDK, no toolchain feature, so Go, TinyGo and Rust guests are
   equal, and a module built without `guestfn` gets one with `guestfn
   manifest set`.

## Shape

The source is `wasmfn.yaml` in the project (scaffolds ship one):

```yaml
abi: 1                                   # required; the export set of docs/abi.md
name: greeter                            # → org.opencontainers.image.title
version: 0.1.0                           # → …image.version; guestfn build --version overrides
source: https://github.com/example/greeter   # → …image.source (optional)
requires:                                # what the run must be granted; each optional
  egress:
    http:                                # the Input's SandboxHTTPRule shape, verbatim
    - host: api.example.com
      methods: [GET]
      pathPrefix: /v1/
  filesystem: {privateTmp: true}
  env: [GREETING_STYLE]                  # keys the guest reads
config:
  schema:                                # JSON Schema 2020-12, inline; validates the Input's config
    type: object
    properties: {greeting: {type: string}, greetingUrl: {type: string, format: uri}}
    additionalProperties: false
minRuntime: v0.2.0                       # oldest function-wasm runtime that serves this module
```

**The custom section.** Name `wasmfn.manifest`, payload the manifest as
UTF-8 JSON, at most one per module, at most 64 KiB; `docs/abi.md` reserves
it and the whole `wasmfn.*` custom-section namespace for the ABI (a module
may carry other custom sections freely). The reserved name is a line in the
ABI document — additive, no module changes — and can land any time.
`requires.egress.http` reuses `v1beta1.SandboxHTTPRule`, so the check
compares like with like and `scaffold composition` copies it verbatim;
`requires.env` lists keys only — values are the Composition's.

**Annotations** (`guestfn push`, on the manifest, covered by its digest and
therefore by cosign): `io.crossplane.fn.wasm.manifest` — the same JSON —
plus `org.opencontainers.image.{title,version,source,revision,description}`
from the manifest and `--revision`. The runtime never reads annotations;
they serve registry UIs and `guestfn inspect <ref>` without a pull.

**Who writes it.** `guestfn build` reads `wasmfn.yaml` when present,
validates it (strictly: unknown fields refused, egress rules through the
same checks as `internal/sandbox.Validate`), removes any earlier
`wasmfn.manifest` section and appends the new one — pure Go, a custom
section is `0x00`, a LEB128 size, a LEB128 name length, the name, the
payload, appended after the last section; no `wasm-tools`. `guestfn
manifest set fn.wasm [-f wasmfn.yaml]` does the same to a module built any
other way, `guestfn manifest show` prints it. Not `pkg/wasmfn`: Go offers
no path from a value in code to a custom section, TinyGo and Rust guests
do not use `wasmfn`, and anything declared in code would need instantiation
to read — the cost this design exists to avoid.

**Checked against the grant.** In `RunFunction`, after the grants are
settled (step 1) and the module loaded (step 5), before `Run`: with `G` the
run's grant — the Composition's `sandbox` already admitted by the
ceiling — every requirement must be covered by `G`:

- an egress rule: a granted rule with the same `host`, or a granted
  `hostPattern` covering it (a required pattern must sit under a granted
  pattern, the rule `internal/egress` already applies between a
  Composition's pattern and the policy's), whose `methods` include the
  required ones and whose `pathPrefix` is a prefix of the required one
  (empty admits all);
- `filesystem.privateTmp` → `G.PrivateTmp`; each `env` key → a key of
  `G.Env`;
- `abi` == 1; `minRuntime` ≤ the runtime's version from its build info (a
  `(devel)` runtime passes);
- `config.schema` → the Input's `config` (absent = `{}`) validates.

A miss is a fatal result, outcome `refused`, before the module runs:
`module oci ghcr.io/example/greeter@sha256:3f2a… requires
sandbox.egress.http host api.example.com methods [GET] pathPrefix /v1/,
which the Composition does not grant`; `… requires
sandbox.filesystem.privateTmp, which the Composition does not grant`; `…
requires env GREETING_STYLE, which the Composition does not set`; `…
requires runtime v0.3.0 or newer, this is v0.2.1`; `config does not match
the module's schema: /greeting: expected string, got number`. Ordering
matters and is already right: a Composition that grants what the module
requires on a runtime whose ceiling refuses it fails at step 1 with the
grant-and-flag message; the manifest check sees only grants the operator
admitted, so it can neither widen nor leak what the ceiling would refuse.
Only narrowing: a manifest can make a run fail earlier; it cannot make a
run possible.

**`guestfn scaffold composition [--from fn.wasm | <ref>] [--name greeter]
[--function-name function-wasm]`** prints a Composition step (or, with
`--full`, a Composition like the scaffold's `example/composition.yaml`)
whose `module` is `Path` for a file or the pinned `OCI` reference, whose
`sandbox` is `requires` copied, whose `config` is a skeleton from the schema
(required keys with placeholders, defaults where the schema has them) and
whose `limits` are commented. `guestfn push` also prints the `sandbox:`
block under the `module:` block it prints today whenever the manifest
requires anything.

## Mechanics

- `internal/manifest` (new, pure Go): `Manifest` types with JSON tags,
  `Parse(raw []byte, strict bool)`, `Validate` (egress rules through
  `egress.ValidHost`/`ValidHostPattern`/`NormalizedPath`, methods from the
  Input's enum, env keys as identifiers, schema compiled once), `Check(m,
  Grants{PrivateTmp bool; Env map[string]string; HTTP
  []v1beta1.SandboxHTTPRule}, config *runtime.RawExtension, runtimeVersion
  string) error` — plain data in, so the package depends on
  `input/v1beta1`, `internal/egress`, the JSON-Schema library and nothing
  that imports wasmtime; `guestfn` and the runtime both import it.
  `internal/egress` exports the pattern-covers-host and pattern-under-pattern
  helpers it already has.
- JSON Schema: `github.com/santhosh-tekuri/jsonschema/v6` (pure Go, draft
  2020-12, no Kubernetes dependency), compiled with a loader that admits
  only the inline document — a `$ref` to a URL is a validation error of the
  manifest, never a fetch; patterns are Go RE2, so no pathological regexp.
- `internal/wasmbin`: `Custom("wasmfn.manifest")`, `AppendCustom`,
  `RemoveCustom` (the reader of the admission one-pager, plus two writers).
- `internal/engine`: `Module.Manifest() []byte` — the raw section, taken
  from the bytes in `Cache.load` before `Compile`. wasmtime's serialized
  artifact does not carry custom sections and the artifact-hit path must
  stay free of registry I/O, so the section is persisted next to the
  artifact: a third store `manifests/<digest>` under
  `/tmp/function-wasm-cache` (`cache.ManifestsDir`; an empty entry means
  "no manifest"; swept with the others, kilobytes at most). On an artifact
  hit without a sidecar — an artifact an older runtime wrote — the module
  bytes come from the blob store (verify on read, 25–50 ms for a Go guest)
  or the source, are parsed once, and the sidecar is written.
- `cmd/function/fn.go`: `checkManifest` between `load` and `Run`; the
  parsed manifest and compiled schema cached per digest in the `Function`
  (a `sync.Map`, entries of a few KB, bounded by the digests a process
  serves like the compiled store is); `warm.go` logs a module's
  requirements at debug; `validate --resolve` reports a mismatch statically,
  with the same message.
- `cmd/guestfn`: `manifest.go` (`ManifestCmd{Set, Show}`), `build`
  (embed, `--version`, `--revision`), `push` (annotations, the printed
  `sandbox:` block), `scaffold.go` (`ScaffoldCmd{Composition}`); the three
  template sets and examples gain `wasmfn.yaml` — a `config.schema` for
  `greeting`/`greetingUrl` and no `requires`, since the examples fetch a
  greeting only when `greetingUrl` is set and must keep rendering without a
  grant; `TestRenderMatchesExample` keeps them in sync.
- Tests: `internal/manifest` table tests for `Check` (each requirement
  covered, missing, pattern under pattern, method subset, prefix); a
  `testwasm.Fixed` module with an `Extra` custom section carrying a manifest
  through `engine` and `cmd/function` (`TestRunFunction` cases: required
  egress granted / not granted, bad config); `guestfn build` on the scaffold
  followed by `inspect` in `main_test.go`; the sidecar path in
  `cache_test.go` (artifact present, sidecar missing).
- Docs: `docs/abi.md` ("Custom sections": `wasmfn.manifest` and the
  reserved namespace, the JSON shape), README ("Write a module": the file;
  "Input reference": what a refusal looks like), the sandbox one-pager (a
  line under each grant: a module may declare it as required).

## Trust and threat notes

The manifest is compared against a grant the ceiling already admitted, so
it can refuse a run or document a need — never grant, never widen, never
touch another module's run (there is no "denies" list). It travels inside
the module bytes: whoever can change it can change the code, and
`--cosign-key` covers both; the annotations are an unverified copy the
runtime ignores. `config.schema` runs a validator over Composition-authored
data: the schema is bounded (64 KiB), offline (`$ref` to URLs refused), RE2
only, compiled once per digest, so a hostile schema costs a bounded parse
and a refusal. Parsing the section uses the hardened reader of the
admission one-pager. Fail-closed on unknown `requires.*` means a module
built for a newer runtime is refused with a message, not run without a
capability it counted on. Nothing here reads anything from the composite
resource.

## Phasing

| phase | what | effort | release |
|---|---|---|---|
| 0 | reserve `wasmfn.manifest` and `wasmfn.*` custom sections and the annotation key in `docs/abi.md` — a documentation line, additive, any time | S | whenever |
| 1 | `internal/manifest` types, `wasmfn.yaml`, `guestfn build`, `manifest set`/`show`, `inspect`, `push` annotations and the printed `sandbox:` block | M | v0.2 |
| 2 | runtime: `Module.Manifest`, the sidecar store, `checkManifest` for `requires`/`abi`/`minRuntime`, `validate --resolve` | M | v0.2–v0.3 |
| 3 | `config.schema` validation in the runtime and in `guestfn` (`build` validates the example config, `scaffold composition` uses it) | S–M | v0.3 |
| 4 | `guestfn scaffold composition` | S | v0.3 |

Nice before v0.1.0, not required: the phase-0 line in `docs/abi.md`.
Reserving the name early only means no third party picks `wasmfn.manifest`
for something else in the meantime; a new custom section never invalidates
a published module, and a runtime without phase 2 ignores it. Nothing here
gates v0.1.0.

## Decisions for Jonasz

- **Home of the manifest** (open question 2): custom section as the source
  of truth, annotations as a mirror. Recommended: yes. A section survives
  `path` and `http` sources and a `crane copy` between registries, is
  covered by every digest and signature the runtime already checks, and is
  the one thing `guestfn`, the runtime and `inspect` all read; annotations
  alone survive nothing but OCI, sit outside the layer, and would still
  need the sidecar plus a manifest GET for a module already on the volume.
- **Writer**: `guestfn` only, from `wasmfn.yaml`; `pkg/wasmfn` stays out.
  Recommended: yes — reasons above; a Go type for the manifest can be
  re-exported for guest tests later if anyone asks.
- **`requires` is hard**: an unmet requirement refuses the run. The
  alternative — a warning in the log and the run proceeds — makes the
  manifest advisory and every guest keep its own checks. Recommended: hard;
  an `optional: true` per rule can be added later without breaking anything.
- **`config.schema` enforced by the runtime**, not only by `guestfn`.
  Recommended: the runtime — a Go guest already fails on a bad config, only
  later and worse, and the per-request cost is microseconds with the schema
  compiled once per digest; the failure names the field.
- **Source file name**: `wasmfn.yaml`, after the ABI's host module and the
  section name. `manifest.yaml` reads as a Kubernetes manifest,
  `guestfn.yaml` ties it to the tool rather than the contract.
