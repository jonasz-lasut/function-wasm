# Guest Language Support

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft, revision 0.2

Which languages a function-wasm module can be written in today, which could
come next and at what cost, which are blocked and by what, and what
"supporting a language" means in this repository. Based on a research pass
on 2026-08-16 (fifteen toolchains evaluated against ABI v1; Zig and C proven
with probe modules run through `internal/engine`) and on the Extism
evaluation recorded in AGENTS.md ("Not Extism"): the ABI is host-agnostic
and small, so breadth comes from templates, not from a different runtime.

## What the host requires

`docs/abi.md` and `internal/engine.checkABI` are the contract; a language
qualifies when its toolchain can produce this and nothing else:

- a **wasip1 core module** (not a component) built as a **reactor** — no
  `_start`, an optional `_initialize` the host calls first;
- exports `memory`, `wasmfn_alloc(u32) -> u32`, `wasmfn_run(u32, u32) -> u64`
  (`ptr<<32 | len`);
- imports only from `wasi_snapshot_preview1`, `wasmfn.log(u32, u32, u32)`
  and, if used, `wasmfn.http(u32, u32) -> u64` — the latter re-enters the
  guest's `wasmfn_alloc` while `wasmfn_run` is on the stack, so the
  allocator must be re-entrant;
- protobuf `RunFunctionRequest` / `RunFunctionResponse` on the wire, i.e. a
  protobuf codec that works under wasip1 (no reflection surprises — see the
  TinyGo/vtprotobuf note in AGENTS.md).

`internal/engine` sets no wasm proposal flags today. wasmtime-go v47
surfaces toggles for SIMD/relaxed SIMD, bulk memory, multi-value,
multi-memory, memory64, tail calls and engine-level GC support
(`Config.SetWasm*`, `SetGCSupport` — v47.0.0 `config.go`); the GC,
exceptions and threads proposals themselves are C-API properties the
binding does not expose (`wasm_gc`, `wasm_exceptions`, `wasm_threads`,
documented `false` by default in the bundled `config.h`), so a toolchain
that emits wasm-gc (Kotlin/Wasm, OCaml, Java) is a per-proposal check —
possibly a small binding change — not a settled yes. The **wasmtime-go
binding** *can* load a component but cannot call one, give it host functions
or register WASIp2 (the C-API and the Rust engine can; the Go wrappers are
missing — [wasmtime-go#280](https://github.com/bytecodealliance/wasmtime-go/issues/280),
PRs #290/#291/#292, draft), so component-only toolchains are blocked by the
binding and the ABI, not by a wasm-proposal flag. See "What gates ABI v2" below.

## What "supported" means here

A language is supported when all of this exists, and it is the definition of
done for every candidate below:

1. `examples/hello-<lang>` — the same greeting function as the other guests,
   passing `TestRunFunctionGuests` (`cmd/function/guest_test.go`) with the
   shared expectations: default and configured greeting, a greeting fetched
   through `wasmfn.http` with an egress grant and refused without one, a
   guest-side fatal result on a bad `config`, guest logs through `wasmfn.log`;
   its own native unit tests; `make build`, `make render`, `make render-check`.
2. `cmd/guestfn/internal/scaffold/templates/<lang>` — the example rendered
   for itself (`TestRenderMatchesExample` keeps them identical), a golden
   under `testdata/<lang>`, `guestfn init --lang <lang>`, `guestfn build`
   detecting the toolchain from the project (or `--lang`), the vendored
   `run_function.proto` refreshed by `vendorproto.go` where the language
   compiles it.
3. `docs/abi.md` "Examples" and the README scaffold table (toolchain, how it
   talks protobuf, module size); the ABI glue and the `wasmfn.http` helper
   in the open (~40 and ~100 lines today) — the reference for the next
   language.
4. CI: a lint/test job for the example and a `render (<lang>)` job (guest
   compiled with its toolchain, served by the runtime built from the tree,
   rendered by `crossplane render`); Renovate covering the language's
   dependency files; the toolchain in the Nix shell once
   `docs/one-pager-nix-devenv.md` lands.

Effort below is graded against the Rust flavour (S/S/S — already built) as
(a) example guest, (b) scaffold + build detection, (c) CI.

## Today

| flavour | how it talks protobuf | module | glue |
|---|---|---|---|
| Go (function-sdk-go + vendored glue) | `request`/`response`/`resource` helpers | ~75 MB (13 MB compressed) | vendored `internal/wasmfn` |
| TinyGo | protobuf-go types + vtprotobuf codecs, generated from the vendored proto | ~1.8 MB | `abi_wasip1.go` + `http.go`/`http_wasip1.go` |
| Rust | prost over the vendored proto | ~250 KB | `src/lib.rs` `abi` module + `src/http.rs` |

## Candidates

| language | evidence | module (ballpark) | protobuf | effort a/b/c | verdict |
|---|---|---|---|---|---|
| **Zig** (`wasm32-wasi`) | *Tested*: probe reactor ran a request through `internal/engine`, `checkABI` passed first compile | KB – low MB | `zig-protobuf` (maintained, listed by the protobuf project) | S / S / S | **pursue** |
| **C / C++** (wasi-sdk) | *Tested*, including a real re-entrant `wasmfn.http` round trip through a hand-written import | tens of KB | nanopb (C), protobuf-c / upb | S / S / S | **pursue** |
| **AssemblyScript** | docs: reactor build documented; `as-proto`, `protobuf-as` exist | tens of KB (inferred) | two generators, unverified under this ABI | M / S / S | **spike, then pursue** |
| **Swift** (swift.org WASI SDK) | docs: official toolchain, `-mexec-model=reactor`, `-Xlinker --export=<name>` | low MB (inferred) | swift-protobuf (inferred buildable) | M / M / M | spike; can overtake AssemblyScript |
| JavaScript (Javy / QuickJS) | *Tested*: shipped `javy build` output is a Command/component shape | ~1.3–1.4 MB if embedded | shifts to the embedding side (prost) | L–XL / L / S | blocked as shipped; unblock = a Rust or C reactor embedding javy-core/QuickJS with our exports |
| Python (CPython wasip1, componentize-py, py2wasm) | *Tested*: the official wasip1 build is a Command; componentize-py is component-only | 30 MB+ | pure-Python protobuf (inferred) | XL / XL / M | blocked; unblock = embed CPython + a frozen/zipped stdlib in a reactor, and reconcile with the no-filesystem sandbox |
| C# / .NET | docs: no reactor mode in mainline .NET; componentize-dotnet is Windows-only and component-only | — | — | blocked | wait for upstream |
| Kotlin/Wasm (`wasm-wasi`) | compiler source + docs: `@WasmExport` experimental, target Beta | MB (inferred) | none verified | L / L / M | not now |
| Java (TeaVM) | source + docs: WASI lives in a fork that calls itself experimental | — | protobuf-java (risk) | L–XL / L / M | not now |
| Haskell (GHC wasm backend) | docs: reactor + named exports confirmed | several MB+ | Template-Haskell codegen unverified under GHC-wasm | L–XL / L / L | not now |
| Ruby (ruby.wasm) | docs: reactor confirmed | — | no pure-Ruby protobuf found | L–XL / L / M | not now |
| Grain, MoonBit, OCaml (wasm_of_ocaml), Lua | thin docs / component-oriented / WASI runtime PR open / embed the C VM | — | none, none, none, nanopb via C | unscored / M | not now (Lua only if a use case appears) |

The blocked group shares one cause, worth stating precisely because it is
easy to misattribute. There are **two WASI execution models**: **Preview 1**
(WASI 0.1, `wasip1`) is a *core module* — functions imported from the
`wasi_snapshot_preview1` namespace, a linear-memory raw-pointer ABI — and the
**Component Model** (WASI 0.2, and the async 0.3) is a different binary format
of WIT-typed worlds over the Canonical ABI. ABI v1 is a Preview 1 contract; the
blocked toolchains emit **components** (`componentize-py`,
`componentize-dotnet`, a JS→component path) or **Command** modules (the
official CPython `wasip1` build, `javy build`) — the wrong *shape*, one layer
below the payload, so the JSON payload mode below does nothing for them.

## What gates ABI v2 (and what does not)

The unlock is a component-shaped **ABI v2**: a WIT world mirroring
`RunFunctionRequest`/`Response`, `wasi:http` behind the same host policy. What
gates it is a common source of confusion, so, precisely:

- **Not Wasmtime the engine.** Wasmtime *is* the reference component host: the
  Rust `wasmtime` crate has full component support (`wasmtime::component`,
  `bindgen!` from a WIT world, resources, async, WASI 0.2 via `wasmtime-wasi`,
  and the WASI 0.3 async work). The engine has done this for years.
- **The wasmtime-go binding** — the whole host-side gap. function-wasm hosts
  through wasmtime-go (CGo over libwasmtime); the bundled C-API already exposes
  the component calls (`wasmtime_component_func_call`,
  `..._linker_instance_add_func`, `..._add_wasip2`, `add_wasi_http`), but the
  **Go binding does not wrap them yet**. A binding-maturity issue, not an engine
  or language one. Tracked upstream as
  [wasmtime-go#280](https://github.com/bytecodealliance/wasmtime-go/issues/280)
  and delivered by #290 (component calls + values), #291 (host functions +
  resources) and #292 (synchronous WASIp2 registration), all draft as of
  2026-08-18. Fallback: a CGo shim against the bundled headers inside
  `internal/engine` (the one package allowed to import wasmtime) — possible, but
  it forks the binding surface.
- **Not Go-the-guest-language, for the host.** A host *runs* components by
  lifting/lowering through the C-API; Go never needs a component target to be a
  component host. Go's component support gates only **Go guests under ABI v2** —
  optional, since Go guests keep ABI v1 and lose nothing. Even there, TinyGo
  (wasip2 + wit-bindgen-go) is ahead of mainline Go, and a mainline Go guest
  could reach v2 by wrapping a `wasip1` core module with the preview1 adapter
  plus bindings tooling.

So ABI v2 is a deliberate design decision gated on the **binding**, not on
Wasmtime and not (for the host) on Go. `docs/one-pager-sandbox.md` phase 4 names
the same dependency for `wasi:http`. It is not taken here.

## Proposal

### Order

1. **Zig** — proven, smallest expected modules (a good "why WASM" artifact),
   `zig` is a single binary and packaged in nixpkgs. Cost: pre-1.0 churn
   across minor versions becomes this repository's maintenance surface, the
   same shape as TinyGo's vtprotobuf workaround.
2. **C via wasi-sdk** — proven including the hardest mechanic; the boring,
   stable choice for teams that already write C; smallest real guest with
   nanopb. Cost: wasi-sdk is a GitHub release tarball (not in nixpkgs), so
   `guestfn build` needs `WASI_SDK_PATH` detection and CI a pinned download.
   If only one of the two: C for stability, Zig for ergonomics (`export fn`
   beats `__attribute__((export_name(...)))` and hand-packed protobuf).
3. **AssemblyScript** after a half-day spike (reactor build, `wasmfn.http`
   re-entrancy, one of the two protobuf generators against the vendored
   proto). Best DX of the unverified candidates (`npm install`, no per-OS
   binary; nixpkgs has `assemblyscript`). If the spike fails on a mechanic,
   **Swift** takes the slot after its own spike.

### The optional JSON payload mode

Every language above except Zig/C/AS/Swift would rather not carry a
protobuf codec, and JavaScript/Python authors in particular expect JSON. An
optional second entry point — for example `wasmfn_run_json(u32, u32) -> u64`
carrying `protojson` of the same messages, chosen by the host from the
exports present, `checkABI` accepting either — would let a hand-written
guest skip the codec entirely at the cost of a larger, slower payload
(protojson of a `RunFunctionRequest` with `structpb` observed state is
roughly 1.5–3× the protobuf bytes and a `json.Unmarshal` per call). It is
an ABI v1 *extension* (a new export name, `docs/abi.md` "Compatibility":
mechanics changes are new names), so it is additive and gates no release —
a module without the export is unaffected whenever it lands; the
implementation is S (host: `checkABI` + a `protojson` branch in `run.go`;
docs; a WAT fixture). Recommendation: reserve the name in `docs/abi.md`
when the first language that wants it is scheduled (Zig and C do not need
it), and decide then whether it is worth carrying at all.

### Phasing and effort

| phase | work | effort |
|---|---|---|
| 0 | decide on `wasmfn_run_json` (reserve it in `docs/abi.md`, or reject it) | S, any time — additive |
| 1 | `examples/hello-zig` + template + `guestfn build` (zig) + CI render job + Renovate + Nix shell entry | S + S + S |
| 2 | `examples/hello-c` (wasi-sdk + nanopb) + template + `WASI_SDK_PATH` detection + CI pinned wasi-sdk download | S + S + S |
| 3 | AssemblyScript spike → template if green; else Swift spike | M (+ S/S if adopted) |
| later | JSON payload mode when a language needs it; JS via an embedded QuickJS reactor only on demand (L–XL); ABI v2 when wasmtime-go and Go allow | — |

Each phase is one PR shape: example + template + goldens + CI + docs, the
way `hello-tinygo` and `hello-rust` landed. Nothing here gates `v0.1.0`:
every step adds a flavour or an export name and changes nothing a published
module or Composition relies on.

## Non-goals

- Switching runtimes for language breadth (Extism/wazero — AGENTS.md "Not
  Extism"): the ABI ran unmodified under wazero, so breadth was never a
  runtime property.
- A shared multi-language SDK repository or a "wasmfn for language X" crate
  before a language exists in the examples; the glue stays in the open in
  each example until there is a second consumer.
- Supporting a language whose toolchain cannot produce a reactor with named
  exports (today: C#/.NET) or only produces components (Javy, componentize-py,
  MoonBit's documented path) — until ABI v2.
- Guaranteeing behavioural parity beyond `TestRunFunctionGuests`: the
  greeting function is the contract, not every SDK feature.

## Open questions

1. Is there a specific voiced demand (a user asking for Python, JavaScript
   or C#)? It would reorder the list even though the effort is higher —
   Python is likely the most-requested "why can't I just write…" language
   in the Crossplane audience.
2. Appetite for owning Zig's pre-1.0 churn — the technical case is the
   strongest, the maintenance case the weakest of the two proven candidates.
3. Carry an optional JSON payload mode (`wasmfn_run_json`) at all, or keep
   ABI v1 protobuf-only for good?
4. Is ABI v2 (component world) ever in scope? "Yes, eventually" turns the
   blocked verdicts into "pending a decision"; "no" makes them final. Its
   host-side dependency is concrete and tracked: wasmtime-go component calls +
   host functions ([#280](https://github.com/bytecodealliance/wasmtime-go/issues/280),
   PRs #290/#291/#292), not anything Wasmtime or Go-the-guest-language lacks
   (see "What gates ABI v2").
5. Timebox the AssemblyScript and Swift spikes before any scaffold work
   (recommended: half a day each), or fold the spike into the first PR?
6. How should wasi-sdk be distributed to contributors and CI (README
   install + pinned CI download now; a `fetchurl` in the Nix flake later)?
