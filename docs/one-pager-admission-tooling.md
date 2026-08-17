# Admission and Inspection Tooling

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.0

The runtime is the only gate a Composition's Input ever passes — Crossplane
never installs a function's Input CRD — and until now that gate was reached
only by reconciling. This document adds the same checks as tools: `function
validate` runs the runtime's own admission over a Composition against an
operator's flags and policy; `guestfn build`, `guestfn push` and `guestfn
inspect` read a module's ABI, exports and imports with the runtime's own
engine, so a module the runtime would refuse is refused before it is
published; and `guestfn push` writes the `layerDigests` the CNCF wasm
artifact specification requires. No new trust surface, no new Input field.


## Before this

Every rule of the Input — `module.Validate`, `module.ValidatePolicy`,
`sandbox.Validate`, `sandbox.Ceiling.Grant`, `egress.Grant`, `runOptions` —
runs inside `RunFunction`, and nowhere else. A Composition author learns
that `sandbox.egress.http[0].host "x" is outside the runtime's egress
policy` from an XR condition after a reconcile; an operator who tightens
`--sandbox-egress-policy` learns which Compositions broke from the same
place, one XR at a time; `crossplane beta validate` can check the Input's
*shape* against `package/input/` but knows nothing of the operator's
ceilings. A wrong module — an export missing, an import outside
`wasi_snapshot_preview1`/`wasmfn`, a `wasmfn_run` with the wrong type — is
found by `engine.checkABI` after `wasmtime.NewModule` returns: for a 75 MB
Go guest that is 23–28 CPU-seconds, ~1 GB of peak memory and the compile
slot (`--max-concurrent-compiles`, default 1) spent before the refusal, and
every request for that digest spends it again (a failed load is not
cached) — a cost this design moves to `guestfn build` and `push`, where a
wrong module is now stopped before it is published (the runtime's own
refusal stays as it is; see the decision below). `guestfn push` publishes
whatever bytes it is given; the artifact config it writes carries
`architecture`, `os` and `created`, not the `layerDigests` the
specification lists.

## Goals and non-goals

Goals: the runtime's checks, callable without a cluster — same functions,
same messages; a module's shape as the runtime sees it, readable by the CLI
and by `validate`; artifacts that conform to the specification. Not goals:
an admission webhook (a Crossplane-specific follow-up that would wrap the
same function), tag resolution or any resolution at all in `validate` by
default (digests stay stated), a second WebAssembly decoder — wasmtime is
the runtime's reader and the only one (the decision below) — or a second
implementation of the Input rules: `validate` calls the ones `RunFunction`
calls.

## Shape

**`function validate`** — a subcommand of the runtime binary:

```shell
function validate composition.yaml [more.yaml… | -] \
  [--module-timeout 30s --module-memory-limit 512 --enable-sandbox-egress \
   --sandbox-egress-policy p.yaml --enable-sandbox-private-tmp --cosign-key k.pub \
   --module-dir . …]                       # the serve ceilings, same flags, same env
  [--function-name function-wasm]          # steps to consider (any function by default)
  [--xr xr.yaml]                           # materialise module.from against this composite
  [--resolve]                              # also fetch and inspect each module
  [--output text|json]
```

Input documents are found in each file: every `pipeline[].input` of kind
`Input` in `wasm.fn.crossplane.io/v1beta1` of a `Composition`, or a bare
`Input` document; multi-document YAML and `-` for stdin. For each step the
tool runs exactly what `RunFunction` runs before it resolves anything —
`sandbox.Validate`, `Ceiling.Grant`, `egress.Grant`, `runOptions`,
`module.Validate`, `ValidatePolicy` — and, with `--xr`, `FromComposite`
against that composite resource; without one, a `module.from` source is
reported as chosen by the XR and its `policy` checked for shape and for the
rule the runtime enforces (`module.from` with `OCI`/`HTTP` requires
`repositoryAllowList`). One line per step, the runtime's own strings:

```
composition.yaml: Composition/hello pipeline[0] greeter: OK (oci ghcr.io/example/greeter:v1@sha256:3f2a…, limits timeout 5s memory 128Mi, egress api.example.com)
  warning: sandbox.egress is granted to a module that is not signature-verified: no --cosign-key was given
composition.yaml: Composition/hello pipeline[1] labeler: refused: sandbox.egress.http[0].host "evil.example.com" is outside the runtime's egress policy (allowed: api.example.com)
```

`--resolve` goes one step further: `Resolve` + `Verify` (with `--cosign-key`)
+ fetch through the same resolver, then `engine.Inspect` — the module
compiled with the runtime's own wasmtime engine, its size, ABI verdict and
host imports reported (the module manifest of
`docs/one-pager-module-manifest.md`, once it exists, is checked here too).
Pulls use the local Docker config only: a step credential lives in a Secret
the tool cannot see, so a source naming `credentials` is validated for
shape and noted, never pulled with anything but the keychain. Exit code 0
when every step is admitted, 1 when at least one is refused, 2 when the tool
itself failed (unreadable file, unparsable YAML). `--output json` emits one
object per step, one per line, for CI annotations.

**`engine.Inspect` and `Module.Shape`** — the runtime's view of a module,
in `internal/engine`:

```go
func (e *Engine) Inspect(wasm []byte) (*Shape, error)  // compiles, reads, releases
func (m *Module) Shape() *Shape                        // of a compiled module, no second compile
type Shape struct {
    Exports  []Extern         // name, kind, "(i32, i32) -> (i64)"
    Imports  []Extern         // module, name, kind, type
    Memories []MemoryLimits   // min, max pages, shared, memory64
    ABIError error            // checkABI's verdict, nil for ABI v1
}
func (s *Shape) HostImports() []string  // "wasmfn.log", "wasmfn.http"
```

wasmtime reads a module only by compiling it — its Go binding offers
`NewModule` (compile), `ModuleValidate` (validity, no shape) and
`Imports()`/`Exports()` on a compiled module; there is no parse-only API and
no custom-section access — so `Inspect` costs a compile: seconds and about
a gigabyte for a large Go guest, sub-second for TinyGo and Rust. That is
the price of the runtime's exact verdict rather than a second decoder's,
and it is paid at build, push and inspect time, never on the request path.
`checkABI` after `Compile` and `Deserialize` stays the one authoritative
check; `Shape` reports its verdict rather than re-implementing it.

**No pre-compile reject.** A module without the ABI still costs the runtime
a compile before `checkABI` refuses it, as today. A cheaper reject would
need a decoder other than wasmtime; see the decision below.

**`guestfn inspect <fn.wasm | oci-ref>`** prints size, the ABI verdict,
exports and imports with their types, memory limits; for a reference: the
manifest digest, media types, layer sizes and annotations without pulling,
the layer the runtime would take, and with `--pull` the same as for a file.
`--output json`. **`guestfn build`** runs the check on its output and
prints the verdict (`Built fn.wasm (73.9 MB, ABI v1, imports wasmfn.http
wasmfn.log)`), failing on a module the runtime would refuse; **`guestfn
push`** refuses to publish such a module, with the same message — modules
for other hosts are `oras push`'s business. `guestfn` links the engine and
is therefore a CGo binary like the runtime.

**`layerDigests`.** `artifact()` in `cmd/guestfn/main.go` writes the
`application/vnd.wasm.config.v0+json` config the specification lists —
`created`, `architecture`, `os`, `layerDigests: ["sha256:<layer digest>"]`
— through a minimal `partial.CompressedImageCore`, since ggcr's
`v1.ConfigFile` has no such field. The manifest digest stays a function of
the module bytes alone; it changed once, with this release — a re-push of
the same bytes yields a new pinned reference — and nothing already published
or pinned becomes invalid: a Composition keeps its digest, the registry
keeps its manifest.

**Tar layers.** A `FROM scratch` image stores the module in a tar layer;
the resolver accepts one only when the archive holds the module at exactly
`/fn.wasm` (`COPY fn.wasm /`; `fn.wasm`, `./fn.wasm` and `/fn.wasm` name
the same root entry) — nothing is guessed from the archive's contents and
the name is not configurable. Raw `application/wasm` layers, as `guestfn
push` and `oras push` produce, stay the recommended shape.

## Mechanics

- `internal/admission` (new): `Admit(in *v1beta1.Input, c Ceilings)
  (Admitted, error)` with `Ceilings{Engine engine.Config; Sandbox
  *sandbox.Ceiling; Egress *egress.Egress}` and `Admitted{Options
  engine.RunOptions; Grant sandbox.Grant; HTTP *egress.Grant}` — steps 1–2
  of `RunFunction` behind one function `RunFunction` and `function
  validate` share (`--xr` then runs `FromComposite`, as `RunFunction` does;
  without it `module.ValidateFrom` applies the fence a `from` source
  requires); the refusal messages are unchanged, so `TestRunFunction`'s
  refusal cases keep passing. `runOptions` moved there from `cmd/function`.
- `cmd/function/main.go`: kong subcommands — `serve` (`default:"withargs"`,
  so `function --insecure --debug --module-dir=.` and a
  `DeploymentRuntimeConfig`'s `args` keep working with no subcommand) and
  `validate`; the ceiling flags are one embedded `CeilingFlags` struct
  (`--module-dir`, `--max-module-size`, `--module-timeout`,
  `--module-memory-limit`, `--cosign-key`, `--enable-sandbox-*`,
  `--sandbox-egress-policy`) with `ceilings()` and `resolver()`, so the
  flags an operator passes to `serve` are the flags `validate` takes.
  `run` (`docs/one-pager-local-loop.md`) will be a third command.
- `cmd/function/validate.go` + `validate_test.go`: fixtures under
  `testdata/validate/` that hit each refusal (`--enable-sandbox-*` off,
  egress outside a policy file, `limits` above a ceiling, `from` without a
  policy, a tag instead of a digest, an Input of the wrong shape), the
  warnings, `--xr`, `--function-name`, stdin, JSON, exit codes, and
  `--resolve` over fixture modules (an ABI v1 one, one without `wasmfn_run`,
  bytes that are not wasm, a missing file).
- `internal/engine/shape.go` (`Inspect`, `Module.Shape`, `Shape`, `Extern`,
  `MemoryLimits`) + `shape_test.go` over `testwasm.Fixed` modules;
  `Config.WithDefaults` for a ceiling without an `Engine`.
- `internal/module`: `ValidateFrom`; `WasmLayer`, `IsTarLayer` and
  `ExtractWasm` exported for `guestfn inspect`; `ScratchModulePath`.
- `cmd/guestfn/inspect.go`, `main.go` (`build`, `push`, `artifact`),
  `main_test.go` (inspect over a file, a reference and `--pull`; push
  refusal; the config's `layerDigests`; the build verdict) — the fixtures
  are two hand-assembled modules, so no toolchain is needed;
  `TestRunFunctionGuests` gains an `Inspect` pass over each built guest.
- `examples/render.sh`: `function validate --resolve` over the example
  Composition before the runtime is started — every `render (lang)` job
  runs it, no new job.
- Docs: `docs/abi.md`, README ("Validate a Composition" next to "Render
  locally"; `guestfn inspect`, the build verdict and the push refusal in
  "Write a module"; the subcommands in "Runtime flags"), AGENTS.md.

## Trust and threat notes

`validate` is read-only and needs no credential: it reads YAML, and with
`--resolve` pulls with the local Docker config as `guestfn push` pushes with
it. Untrusted module bytes are decoded by wasmtime alone — `Inspect`
compiles them, exactly as a load would, so `validate --resolve` and
`guestfn inspect` carry the compile's cost (bounded by `--max-module-size`
as a fetch is) and no new parser. `guestfn push` refusing a non-ABI module
protects the author, not the runtime — the runtime never trusted the
publisher. A tar layer yields only `/fn.wasm`, so an image cannot smuggle a
module under a name a reader might guess.

## Phasing

| phase | what | release |
|---|---|---|
| 1 | `engine.Inspect`/`Shape`, `guestfn build`/`push` check, `layerDigests` | v0.2 |
| 2 | `guestfn inspect` (file, reference, `--pull`) | v0.2 |
| 3 | `internal/admission`, subcommand split, `function validate` (`--xr`, `--output json`), `render.sh` | v0.2 |
| 4 | `--resolve` (resolve, verify, fetch, `Inspect`) | v0.2 — the module-manifest check joins it with `docs/one-pager-module-manifest.md` |

## Decisions

- **wasmtime, not a pure-Go reader** (Jonasz, 2026-08-17). A pure-Go
  reader of the binary format was built and rejected in review: it would
  have made `guestfn` pure Go and given the runtime a pre-compile reject
  costing milliseconds instead of a compile, at the price of a second
  WebAssembly decoder in the tree — one that could disagree with wasmtime,
  had to track the binary format on its own, and needed fuzzing of its own.
  Compatibility with the runtime and not reinventing wasm support won:
  every verdict is wasmtime's. Consequences accepted: `guestfn` is CGo (a C
  compiler to install it, as for the runtime), `build`/`push`/`inspect` and
  `validate --resolve` pay a compile (seconds for a large Go guest), there
  is no pre-compile reject, and custom sections (names, DWARF, a future
  manifest section) are not readable through wasmtime's Go binding — the
  manifest one-pager has to carry the section through another channel
  (OCI annotations, or a section walker of its own scoped to that one
  section) if it wants to read it before a compile.
- **`FROM scratch` images hold the module at `/fn.wasm`** (Jonasz,
  2026-08-17): the tar path stays for `COPY fn.wasm /` images, but the
  entry is that exact root path — the resolver never picks "the first
  `.wasm` file" — and it is not configurable.
- **Where `validate` lives**: the runtime binary. The checks are the
  operator's — the same kong flags, the same env, the same policy file, the
  same version as the pod (`docker run ghcr.io/jonasz-lasut/function-wasm:vX
  validate …` works: the package image is the runtime image, entrypoint
  `/function`).
- **`guestfn push` refuses non-ABI modules with no override.** An override
  invites publishing what the runtime will refuse; `oras push` exists for
  other artifacts.
- **Warnings**: a short fixed list — a `Path` source in a Composition,
  egress granted without `--cosign-key`, a limit equal to its ceiling, a
  field the runtime would silently ignore — printed after `OK`, never
  affecting the exit code.
- **Subcommand split**: one binary, `serve` the default with args; a
  second binary would double the image and the release surface.
