# Admission and Inspection Tooling

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft

The runtime is the only gate a Composition's Input ever passes — Crossplane
never installs a function's Input CRD — and today that gate is reached only
by reconciling. This document adds the same checks as tools: `function
validate` runs the runtime's own admission over a Composition against an
operator's flags and policy; a pure-Go WebAssembly reader
(`internal/wasmbin`) lets `guestfn inspect`, `guestfn build`, `guestfn push`
and the runtime's load path see a module's ABI, sections and manifest
without wasmtime, so a wrong module is refused before it is compiled; and
`guestfn push` writes the `layerDigests` the CNCF wasm artifact
specification requires. No new trust surface, no new Input field. Nothing
here gates v0.1.0.


## Today

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
cached). `guestfn push` publishes whatever bytes it is given; the artifact
config it writes carries `architecture`, `os` and `created`, not the
`layerDigests` the specification lists.

## Goals and non-goals

Goals: the runtime's checks, callable without a cluster and without a
compile — same functions, same messages; a module's shape readable in pure
Go by the CLI and by the runtime before `Compile`; artifacts that conform to
the specification. Not goals: an admission webhook (a Crossplane-specific
follow-up that would wrap the same function), tag resolution or any
resolution at all in `validate` (digests stay stated), validating wasm code
(wasmtime does that at compile; the reader parses sections, it never
executes or type-checks function bodies), a second implementation of the
Input rules — `validate` calls the ones `RunFunction` calls.

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
composition.yaml: pipeline[0] greeter: OK (oci ghcr.io/example/greeter:v1@sha256:3f2a…, limits 5s/128Mi, egress api.example.com)
composition.yaml: pipeline[1] labeler: refused: sandbox.egress.http[0].host "evil.example.com" is outside the runtime's egress policy (allowed: api.example.com)
```

`--resolve` goes one step further: `Resolve` + `Verify` (with `--cosign-key`)
+ fetch through the same resolver, then the reader below — layer media type,
size, ABI verdict, imports, custom sections and the module manifest
(`docs/one-pager-module-manifest.md`), checked against the step's grant.
Pulls use the local Docker config only: a step credential lives in a
Secret the tool cannot see, so a source naming `credentials` is validated
for shape and noted, never pulled with anything but the keychain. Exit code
0 when every step is admitted, 1 when at least one is refused, 2 when the
tool itself failed (unreadable file, unparsable YAML). `--output json` emits
one object per step for CI annotations.

**`internal/wasmbin`** — a reader for the wasm binary format, no wasmtime:

```go
func Parse(wasm []byte) (*Module, error)         // sections walked, bodies skipped
type Module struct {
    Sections []Section                              // id, name (custom), offset, size
    Imports  []Import                               // module, name, kind, func type
    Exports  []Export                               // name, kind, func type
    Memories []Memory                               // min, max pages
}
func (m *Module) Custom(name string) ([]byte, bool)   // a custom section's payload
func (m *Module) HasNames() bool                      // "name" section — readable traps
func (m *Module) HasDWARF() bool                      // ".debug_*" sections
func CheckABI(m *Module) error                        // ABI v1 exports and imports
var ABIv1 = …                                          // the export/import table
```

It parses the type, import, function, memory, export and custom sections
and skips code and data by their declared sizes, so a 75 MB Go guest is a
few milliseconds of LEB128 walking. `ABIv1` is the one table of required
export names and types and admitted imports; `engine.checkABI` compares
wasmtime's types against it, `wasmbin.CheckABI` the parsed ones, and both
produce the same strings (`module does not export "wasmfn_run"`, `module
imports x.y, which the host does not provide`). The engine imports
`wasmbin`; `wasmbin` imports nothing but the standard library, so `guestfn`
stays pure Go.

**Pre-compile reject.** `engine.Cache.load` calls `wasmbin.Parse` +
`CheckABI` on the fetched bytes before taking a compile slot; a refusal is
the load's error as today (`cannot load module …: module does not export
"wasmfn_run"`), costing milliseconds instead of a compile. `checkABI` stays
after `Compile` and `Deserialize` as the authoritative check — wasmtime's
decoder is the truth about the module — so a disagreement between the two
readers is a bug in `wasmbin`, never a module that runs.

**`guestfn inspect <fn.wasm | oci-ref>`** prints size, the ABI verdict,
exports and imports, memories, custom sections (`name`, `producers`, DWARF
— and so whether traps will be readable), the module manifest if any; for
a reference: the manifest digest, media types, layer size and annotations
without pulling, and with `--pull` the same as for a file. `--output json`.
**`guestfn build`** runs `CheckABI` on its output and prints the verdict
(`Built fn.wasm (74.9 MB, ABI v1, imports wasmfn.log wasmfn.http)`);
**`guestfn push`** refuses to publish a module the runtime would refuse at
load, with the same message — modules for other hosts are `oras push`'s
business.

**`layerDigests`.** `artifact()` in `cmd/guestfn/main.go` adds
`layerDigests: ["sha256:<layer digest>"]` to the
`application/vnd.wasm.config.v0+json` config, as the specification requires.
The manifest digest stays a function of the module bytes alone; it changes
once — a re-push of the same bytes yields a new pinned reference — and
nothing already published or pinned becomes invalid: a Composition keeps
its digest, the registry keeps its manifest. Additive, whenever.

## Mechanics

- `internal/admission` (new): `Admit(in *v1beta1.Input, c Ceilings)
  (Admitted, error)` with `Ceilings{Engine engine.Config; Sandbox
  *sandbox.Ceiling; Egress *egress.Egress}` and `Admitted{Limits
  engine.RunOptions; Grant sandbox.Grant; HTTP *egress.Grant}` — steps 1–2
  of `RunFunction` moved behind one function `RunFunction` and `function
  validate` share (`--xr` then runs `FromComposite`, as `RunFunction` does);
  the refusal messages are unchanged, so `TestRunFunction`'s refusal cases
  keep passing.
- `cmd/function/main.go`: kong subcommands — `serve` (marked
  `default:"withargs"`, so `function --insecure --debug --module-dir=.` and a
  `DeploymentRuntimeConfig`'s `args` keep working with no subcommand),
  `validate` (this document), `run` (`docs/one-pager-local-loop.md`); the
  ceiling flags move into one embedded struct the three commands share, so
  the flags an operator passes to `serve` are the flags `validate` takes.
- `cmd/function/validate.go` + `validate_test.go`: goldens under
  `testdata/validate/` over Compositions that hit each refusal
  (`--enable-sandbox-*` off, egress outside a policy file, `limits` above a
  ceiling, `from` without a policy, a tag instead of a digest).
- `internal/wasmbin/{wasmbin.go,abi.go}` + tests over `testwasm.Fixed`
  modules (`SkipRun`, `RunSignature`, an `Extra` import of `foo.bar`) and,
  outside `-short`, the three example guests; a fuzz target over `Parse`.
- `internal/engine/cache.go` (the pre-compile call), `engine.go` (`checkABI`
  over `wasmbin.ABIv1`).
- `cmd/guestfn/inspect.go`, `main.go` (`build`, `push`), `main_test.go`
  (inspect golden over a fixture; push refusal); `TestRunFunctionGuests`
  gains an inspect pass over each built guest.
- CI: `render (go|tinygo|rust)` run `function validate` over the example
  Composition first — one more line, no new job.
- Docs: `docs/abi.md` (the runtime reads the ABI from the binary before
  compiling; `guestfn inspect` shows what it sees), README (a "Validate"
  paragraph next to "Render locally"; `guestfn inspect` in "Write a module").

## Trust and threat notes

`validate` is read-only and needs no credential: it reads YAML, and with
`--resolve` pulls with the local Docker config as `guestfn push` pushes with
it. The reader parses untrusted bytes: every count is checked against the
bytes left before anything is allocated (a module may declare 2^32 exports),
section sizes may not exceed the buffer, custom-section names are bounded,
and the fuzz target runs in CI; `--max-module-size` bounds the input as it
bounds a fetch. The pre-compile reject shrinks the cost of a wrong module
from a compile to a parse; a module with the right exports and invalid code
still fails at compile as before, and one with the right shape and hostile
behaviour still meets the sandbox. `guestfn push` refusing a non-ABI module
protects the author, not the runtime — the runtime never trusted the
publisher.

## Phasing

| phase | what | effort | release |
|---|---|---|---|
| 1 | `internal/wasmbin`, pre-compile reject, `guestfn build`/`push` check | S–M | v0.2 |
| 2 | `guestfn inspect` (file, reference, `--pull`) | S | v0.2 |
| 3 | `internal/admission`, subcommand split, `function validate` (`--xr`, `--output json`) | M | v0.2 |
| 4 | `--resolve` with the module manifest; a documented CI recipe (validate in a pipeline; a Kyverno/ValidatingAdmissionPolicy example calling nothing but the CRD for shape) | S | with the manifest |

Nice before v0.1.0, not required: `layerDigests` (S). Doing it before the
first release means no published module ever changes its pinned reference
on a re-push; doing it later costs exactly that — one new digest per re-push
of unchanged bytes, old references intact. Nothing here gates v0.1.0.

## Decisions for Jonasz

- **Where `validate` lives** (open question 1): the runtime binary or
  `guestfn`? Recommended: the runtime binary. The checks are the operator's
  — the same kong flags, the same env, the same policy file, the same
  version as the pod (`docker run ghcr.io/jonasz-lasut/function-wasm:vX
  validate …` works: the package image is the runtime image, entrypoint
  `/function`); `guestfn` stays pure Go for `init`/`build`/`push`/`inspect`
  (Rust and TinyGo authors install it without a C compiler), and the one
  CGo edge below `internal/sandbox` (two constants imported from
  `internal/engine`) is not worth reversing for a `guestfn validate` that
  would drift from the operator's flags.
- **`guestfn push` refuses non-ABI modules with no override.** Recommended:
  refuse. An override invites publishing what the runtime will refuse;
  `oras push` exists for other artifacts.
- **Warnings.** Should `validate` warn (exit 0) on Inputs the runtime
  accepts but that are unwise — a `Path` source in a Composition, egress
  without `--cosign-key`, `limits` equal to the ceiling? Recommended: yes,
  a short fixed list, printed after `OK` and never affecting the exit code.
- **Subcommand split.** `serve` as the default-with-args command keeps every
  existing invocation working; the alternative — a second binary for
  `validate`/`run` — doubles the image and the release surface. Recommended:
  one binary, `serve` default.
