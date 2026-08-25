# ABI v2 and the Rust Host

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft, revision 0.1

The decision `docs/one-pager-language-support.md` left open ("Is ABI v2 ever
in scope?", open question 4): yes. ABI v2 - a component-model guest contract -
is the roadmap, and the host moves to Rust to get there. This one-pager
records why the two are one decision, what v2 is, what stays fixed, and the
order of work.

## Why Rust and v2 are one decision

ABI v2 means running components, and the component gap is the Go binding, not
the engine (language-support one-pager, "What gates ABI v2"): wasmtime-go v47
can load a component but cannot call one, define host functions on the
component linker, or register WASIp2/`wasi:http` - the wrappers are missing
([#280](https://github.com/bytecodealliance/wasmtime-go/issues/280), draft
PRs #290/#291/#292 by one contributor, unmerged). The Rust `wasmtime` crate
is the reference component host: `component::bindgen!` over a WIT world, host
functions, resources, `wasmtime-wasi` (WASI 0.2), `wasmtime-wasi-http` with
an overridable `send_request`, and the WASI 0.3 async work. Committing to v2
therefore either waits on a volunteer binding effort, forks a CGo shim inside
`internal/engine`, or moves the host to the engine's own language. This
one-pager chooses the third.

What comes along by construction:

- native `cedar-policy` - the reference PDP; `internal/authz` today runs a
  subset of it through cedar-go, and the subtle parts (the boundary-correct
  `Repository` hierarchy, `HostPattern`, the `dialAddress` decision table)
  are our own code that ports 1:1;
- no CGo: a static musl binary, `FROM scratch` image, no glibc base to track -
  today's Chainguard glibc-dynamic pin, its Renovate bumps and its CVE stream
  exist only because wasmtime-go links libwasmtime;
- the wasmtime API the binding never exposed: parse-only validation, custom
  sections, `InstancePre` + the pooling allocator (cheap per-request
  instantiation for the fresh-instance-per-request design), per-run stderr
  callbacks;
- `guestfn` follows the runtime (it links the engine for its verdicts, so the
  move is all-or-nothing by design), and a Rust guestfn is a static binary
  users install without a C toolchain.

## What ABI v2 is

A WIT world (working name `wasmfn:function@2.0.0`), targeted at WASI 0.3:

- **export** `run: async func(request: list<u8>) -> result<list<u8>, string>` -
  protobuf `RunFunctionRequest`/`RunFunctionResponse` bytes, exactly v1's
  wire format. Deliberately not a typed WIT mirror of the messages: WIT has
  no recursive types, so `Struct`/`Value` cannot be expressed, and payload
  evolution stays protobuf's job (`docs/abi.md` "Compatibility"). What the
  component model removes is v1's mechanics - `wasmfn_alloc`, `ptr<<32|len`,
  the re-entrant allocator: the canonical ABI owns memory movement.
- **import** `log: func(level: log-level, msg: string, kv: list<tuple<string,
  string>>)` - typed, replacing v1's JSON payload.
- **import** `wasi:http@0.3/outgoing-handler` rather than a custom HTTP
  interface, implemented by the host over `internal/egress`'s policy
  (wasmtime's overridable outgoing-handler seam; bodies are native
  `stream<u8>`), so guests use their language's native clients (Rust
  reqwest, JS fetch under jco, Python under componentize-py). Admission is
  unchanged: `requires.egress.http` under the
  three layers decides whether the import is backed by the policy client or
  a refuser.
- **detection**: a component is ABI v2, a core module is ABI v1 - "a
  mechanics change is a new set of export names", and here the binary format
  is the name. The manifest says `abi: 2`; everything else in it is
  unchanged.

What v2 unlocks is the blocked row of the language-support matrix, arriving
as each toolchain reaches 0.3: JS first (jco is 0.3-ready today), Rust on
nightly now and on stable when `wasm32-wasip3` reaches tier 2, Python when
componentize-py's 0.3 work lands, C#/.NET eventually. TinyGo (wasip2 only
today) and mainline Go (nothing beyond wasip1 until golang/go#77141,
realistically 1-2+ years) stay on v1 - which settles the retirement
question: **v1 is served indefinitely, by the same runtime**. wasmtime runs
both shapes, and `Component::serialize` caches like `Module::serialize`
under the same `compiled/<Version()>` namespace.

Target version, decided (status checked 2026-08-25): **WASI 0.3 from the
start, accepting unstable Rust for early guests**. WASI 0.3.0 shipped
2026-06-11 ([wasi.dev](https://wasi.dev/releases/wasi-p3)); host-side,
readiness is free - wasmtime ships 0.3 with component-model async enabled
by default since v46
([Bytecode Alliance](https://bytecodealliance.org/articles/WASI-0.3)), and
the v47 line this repo already builds against has it. The alternative -
define the world against 0.2 and lift it later - was rejected because the
lift is not free: `wasi:http@0.2` and `@0.3` are different interfaces, so
"v2 on 0.2, revisited at 0.3" quietly means cutting two v2 worlds and doing
the streams/budget design twice. One world, defined once against the ABI
with native async and `stream<u8>` bodies, is worth the near-term toolchain
cost: Rust guests build on nightly with `-Zbuild-std` until
`wasm32-wasip3`'s tier-2 promotion lands (FCP-approved, gated on Rust and
wasi-libc aligning on LLVM 23:
[compiler-team#1001](https://github.com/rust-lang/compiler-team/issues/1001),
[rust#147205](https://github.com/rust-lang/rust/pull/147205); realistically
late 2026/early 2027 - the scaffold pins a nightly and drops it then), and
of the unlock languages only jco's JS path is 0.3-ready today. Affordable
because ABI v1 carries every production guest meanwhile. The world ships as
a draft (`wasmfn:function@2.0.0-draft`) and freezes at `2.0.0` no earlier
than wasip3's tier-2 promotion. p3's native async matters less here than
for most hosts - fresh-instance-per-request needs no concurrent calls into
one instance - so its benefit concentrates in the `wasi:http` import.

## What does not change

The surface a 1.0 would freeze is host-language-neutral and survives
verbatim: the Input schema, the manifest format and the three-layer
decision, the Cedar semantics of both policy layers, digest-pinned sources
and the three cache stores, the refusal strings, the flags, `/readyz` + gRPC
health, the metrics series. ABI v1 modules keep running unmodified - the
wazero probe already showed the ABI is host-agnostic, and this swap banks on
that.

Conformance is inherited rather than rewritten: `cmd/function/testdata/
validate/` diffs stdout/stderr/exit codes and can drive any binary
black-box; `TestRunFunctionGuests`'s five guests and shared expectations are
the behavioural contract; the render-check jobs are language-neutral end to
end. The Go tree's fixtures are the Rust host's acceptance suite before it
has unit tests of its own.

## Order of work

| phase | work | proves |
|---|---|---|
| 0 | v2 spike: engine crate (wasmtime), the WIT world, `wasi:http@0.3` over a ported egress admit, one nightly-Rust wasip3 guest + one jco JS guest through the greeting contract | the ABI v2 design, pre-1.0, while the Go host serves production |
| 1 | v1 in the same engine crate: core-module path, checkABI, sandbox (private /tmp, env, epoch deadline, memory limiter), the WAT fixtures via the `wat` crate | one engine, both ABIs |
| 2 | periphery: module resolution (OCI/http/path + the cosign check), admission + authz on cedar-policy, manifest (jsonschema 2020-12), caches, egress in full, the server on function-sdk-rust, metrics, warm-up | the validate fixture suite green against the Rust binary |
| 3 | guestfn port (init/build/inspect/push/manifest/scaffold; v2 templates) | shared verdicts preserved |
| 4 | swap: the Rust binary ships as the function, the Go host retires; 1.0 is cut on it | one runtime, v1+v2 |

Phase 0 before phase 1, deliberately: the novel, contract-shaping work is
v2's design, and it should be validated while it is still cheap to change -
the only part of this move the pre-1.0 window genuinely matters for. The
parity port (1-2) is large but specification-driven; nothing in it is
design.

## Port inventory

| Go | Rust | risk |
|---|---|---|
| wasmtime-go v47 | `wasmtime` (native) | none: gains components, InstancePre/pooling, parse-only |
| cedar-go | `cedar-policy` | low: authz's own hierarchy/pattern code ports 1:1 |
| function-sdk-go server | function-sdk-rust (tonic 0.14 + prost; Go parity reached 2026-08-25) | low but young; metrics still need the prometheus-client tower layer |
| go-containerregistry | `oci-client` + `oci-spec`, or a hand-rolled remote | **highest**: auth flows (dockerconfigjson, token refresh), artifact push with custom layer media types - spike early |
| hand-rolled cosign check | the same, over ring/ed25519-dalek (not sigstore-rs: the same dependency-count reasoning) | low |
| jsonschema/v6 (Go) | `jsonschema` crate (draft 2020-12) | low |
| kong | clap | none |
| afero stores | std fs + tempfile | none |
| WAT fixtures via Wat2Wasm | the `wat` crate | none: simpler |

## Risks

- **Bandwidth**: phases 1-3 are months of feature-frozen solo work. The Go
  host keeps shipping fixes during 0-2; the conformance suites bound "done".
- **Security-sensitive re-implementation**: the egress dialer judgment,
  redirect re-checks, the boundary-correct repository hierarchy. Their tests
  port with them, and the refusal strings are diffed by the validate suite.
- **function-sdk-rust maturity**: the runtime becomes its first hard
  consumer. Acceptable: same owner, and the host exercises exactly the
  server surface the SDK just reached Go parity on.
- **wasi:http budgets**: v1's egress budgets act on whole responses; under
  `wasi:http@0.3` bodies are native `stream<u8>`, so the Grant/budget layer
  counts bytes on the stream as the host forwards it. Designed once, in
  phase 0.
- **Spec and toolchain youth**: 0.3.0 is ten weeks old, the Rust target is
  tier 3, and wit-bindgen's async features are new; churn is likely.
  Mitigated by v1 carrying production, the world staying `-draft` until
  wasip3 is tier 2, and phase 0's guests being rebuilt on toolchain bumps
  rather than promised stability.

## Non-goals

- A typed WIT mirror of the RunFunction messages (blocked by WIT recursion,
  wrong evolution model).
- Waiting for or contributing to wasmtime-go #290/#291/#292, or a CGo
  component shim inside `internal/engine`.
- Keeping the Go host after phase 4: two runtimes is the transitional state,
  not the end state.
- A WASI 0.2 flavour of the v2 world: a toolchain that only reaches 0.2
  (TinyGo today) keeps ABI v1 until it reaches 0.3. This is not a wasmtime
  constraint - WASI imports are versioned, so p2 components, p3 components
  and v1 core modules coexist in one host - it is a refusal to define, test
  and keep a second world in lockstep. If a 0.2-only language ever matters
  before its toolchain moves, the bridge is compose-time adaptation: the
  guest's `@0.2` imports virtualized in terms of `@0.3` by composing an
  adapter component (plausibly a `guestfn build` step), never a second
  world. A guest that imports no WASI at all - pure compute over `run`,
  most composition functions - barely touches the difference.
- Retiring ABI v1: it outlives v2's launch by years (mainline Go guests).
- Changing any guest-facing v1 semantics during the port.

## Open questions

1. A `run-json` export in the v2 world: JS/Python authors - v2's whole
   audience - would rather not carry a protobuf codec. A second func
   carrying protojson is cheap in WIT and revives the v1 `wasmfn_run_json`
   debate (language-support one-pager) where it matters most. Decide in
   phase 0.
2. Repository layout during the transition: a cargo workspace beside the Go
   modules in this repo (shared fixtures, one CI), or a sibling repo?
   Recommendation: same repo - phases 1-2 lean on the Go tree's testdata
   daily.
3. Does guestfn keep scaffolding v1 flavours (Go/TinyGo/Zig/C) indefinitely,
   or do the v1 templates freeze at the swap?
