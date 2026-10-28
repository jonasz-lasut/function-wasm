# ABI v2 and the Rust host

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 0.6 (phases 0-4 delivered: the Rust host
  since v0.3.0, phase 0 closed with #65, the world frozen at
  `wasmfn:function@2.0.0` on 2026-10-07, ABI v1 removed in 1.0.0; what
  remains is #106, publishing the world, which waits on the `rust`
  scaffold's stable toolchain)

The decision `docs/one-pager-language-support.md` left open (open question
4, "Is ABI v2 ever in scope?"): yes. ABI v2 - a component-model guest
contract - is the roadmap, and the host moves to Rust to get there. This
one-pager records why the two are one decision, what v2 is, what remains
unchanged, and the order of work.

## Why Rust and v2 are one decision

ABI v2 means running components, and the component gap is the Go binding
rather than the engine (language-support one-pager, "What gates ABI v2").
wasmtime-go v47 can load a component but can't call one. It can't define
host functions on the component linker or register wasip2/`wasi:http`
either: the wrappers are missing
([#280](https://github.com/bytecodealliance/wasmtime-go/issues/280), and
draft PRs #290/#291/#292 by one contributor are unmerged). The Rust
`wasmtime` crate is the reference component host: `component::bindgen!`
over a WIT world, host functions, resources, `wasmtime-wasi` (WASI 0.2),
`wasmtime-wasi-http` with an overridable `send_request`, and the WASI 0.3
async work. Committing to v2 means either waiting on a volunteer binding
effort, forking a CGo shim inside `internal/engine` or moving the host to
the engine's own language. This one-pager chooses the third.

What comes along by construction:

- native `cedar-policy` - the reference PDP; `internal/authz` today runs a
  subset of it through cedar-go, and the subtle parts (the boundary-correct
  `Repository` hierarchy, `HostPattern`, the `dialAddress` decision table)
  are the project's own code that ports 1:1;
- no CGo: a static musl binary, `FROM scratch` image, no glibc base to track -
  today's Chainguard `glibc-dynamic` pin, its Renovate bumps and its CVE stream
  exist only because wasmtime-go links libwasmtime;
- the Wasmtime API the binding never exposed: parse-only validation, custom
  sections, `InstancePre` + the pooling allocator (cheap per-request
  instantiation for the fresh-instance-per-request design), per-run stderr
  callbacks;
- `guestfn` follows the runtime (it links the engine for its verdicts, so the
  move is all-or-nothing by design), and a Rust guestfn is a static binary
  users install without a C toolchain.

## What ABI v2 is

A WIT world (working name `wasmfn:function@2.0.0`), targeted at WASI 0.3:

- **export** `run: async func(request: list<u8>) -> result<list<u8>, string>` -
  Protobuf `RunFunctionRequest`/`RunFunctionResponse` bytes, exactly v1's
  wire format. By design it's not a typed WIT mirror of the messages: WIT
  has no recursive types, so `Struct`/`Value` can't be expressed, and
  payload evolution stays Protobuf's job (`docs/abi.md` "Compatibility").
  The component model removes v1's mechanics - `wasmfn_alloc`,
  `ptr<<32|len`, the re-entrant allocator: the canonical ABI owns memory
  movement.
- **import** `log: func(level: log-level, msg: string, kv: list<tuple<string,
  string>>)` - typed, replacing v1's JSON payload.
- **import** `wasi:http/client@0.3.0` (`send: async func(request) ->
  result<response, error-code>`) rather than a custom HTTP interface -
  `wasi:http` 0.3 replaced 0.2's `outgoing-handler` with `client`, corrected
  here in rev 0.2 - implemented by the host over the egress policy through
  wasmtime-wasi-http's `WasiHttpHooks::send_request` seam (built with
  `default-send-request` off, so the host must supply one; bodies are
  native `stream<u8>`), so guests use their language's native clients (Rust
  reqwest, JS fetch under jco, Python under componentize-py). Admission is
  unchanged: whether the import is backed by the policy client or a refuser
  follows from `requires.egress.http` under the three layers.
- **detection**: a component is ABI v2, a core module is ABI v1 - "a
  mechanics change is a new set of export names", and here the binary format
  is the name. The manifest says `abi: 2` and nothing else in it
  changes.

What v2 unlocks is the blocked row of the language-support matrix, arriving
as each toolchain reaches 0.3 - JS first (jco is 0.3-ready today), Rust on
nightly now and on stable when `wasm32-wasip3` reaches tier 2, Python when
componentize-py's 0.3 work is merged, C#/.NET eventually. TinyGo (wasip2
only today) and mainline Go (nothing beyond wasip1 until golang/go#77141,
realistically 1-2+ years) stay on v1 - which settles the retirement
question: **v1 is served indefinitely, by the same runtime**. Wasmtime runs
both binary formats, and `Component::serialize` caches like
`Module::serialize` under the same `compiled/<Version()>` namespace.
(Superseded, rev 0.4: #114's spike moved every scaffold, Go included, to v2
on 2026-10-07 through `guestfn build`'s embed and wrap, so v1 was
deprecated in v0.6.0 and removed in 1.0.0. The runtime refuses a core
module at load.)

Target version, decided (status checked 2026-08-25): **WASI 0.3 from the
start, accepting unstable Rust for early guests**. WASI 0.3.0 was released
2026-06-11 ([the WASI 0.3 release](https://wasi.dev/releases/wasi-p3));
host-side, readiness is free - Wasmtime has had 0.3 with component-model
async enabled by default since v46
([Bytecode Alliance](https://bytecodealliance.org/articles/WASI-0.3)), and
the v47 line this repo already builds against has it. The alternative -
define the world against 0.2 and lift it later - was rejected because the
lift isn't free: `wasi:http@0.2` and `@0.3` are different interfaces, so
"v2 on 0.2, revisited at 0.3" means cutting two v2 worlds and doing the
streams/budget design twice. One world, defined once against the ABI with
native async and `stream<u8>` bodies, justifies the near-term toolchain
cost - Rust guests build on nightly with `-Zbuild-std` until
`wasm32-wasip3`'s tier-2 promotion is merged (FCP-approved, blocked on Rust
and wasi-libc aligning on LLVM 23:
[compiler-team#1001](https://github.com/rust-lang/compiler-team/issues/1001),
[`rust#147205`](https://github.com/rust-lang/rust/pull/147205);
realistically late 2026/early 2027 - the scaffold pins a nightly and drops
it then), and of the unlock languages only jco's JS path is 0.3-ready
today. Affordable because ABI v1 serves every production guest meanwhile.
The world was first released as a draft and froze at `2.0.0` on 2026-10-07
(#77), ahead of wasip3's tier-2 promotion: its publication (#106) waits
for the `rust` scaffold's stable toolchain instead, and a Wasmtime move of
the WASI 0.3 interfaces before then is re-vendored and taken on `2.0.0` in
place (`docs/abi-v2.md`, Versioning). p3's native async matters less here
than for most hosts - fresh-instance-per-request doesn't need concurrent
calls into one instance - which concentrates its benefit in the
`wasi:http` import.

## What doesn't change

What a 1.0 would freeze is host-language-neutral and survives verbatim.
That's the Input schema, the manifest format and the three-layer
decision, the Cedar semantics of both policy layers, digest-pinned sources
and the three cache stores, the refusal strings, the flags, `/readyz` + gRPC
health, and the metrics series. ABI v1 modules kept running unmodified through
the port, until 1.0.0 removed the ABI - the wazero probe already showed the
ABI is host-agnostic, and this swap banks on that.

Conformance is inherited rather than rewritten: `cmd/function/testdata/
validate/` diffs stdout/stderr/exit codes and can drive any binary
black-box; `TestRunFunctionGuests`'s five guests and shared expectations are
the behavioural contract; the render-check jobs are language-neutral end to
end. The Go tree's fixtures are the Rust host's acceptance suite before it
has unit tests of its own.

## Order of work

Rev 0.2 status (2026-08-26): phases 1-4 were delivered by the Rust port
(released as v0.3.0); phase 0 - by design the contract-shaping work - runs
now as issue #65, three stacked PRs (world + engine path, `wasi:http`
egress, example guests). Its first PR showed that Wasmtime 48 doesn't need
an **engine split** for the two ABIs - async-ness is per-store since
Wasmtime 42, so the sync v1 path and async v2 components share one engine
and one compiled-artifact namespace (the artifacts self-describe their
kind) - and that the canonical ABI accepts a **sync-lifted** `run` where
the world declares an async one, which spares the WAT fixtures an async
lift and keeps pre-wasip3 toolchains usable (the example guests still
commit to real async: the Rust p3 guest awaits `wasi:http/client.send`).

Rev 0.6 status (2026-10-28): every phase is delivered. Phase 0 closed with
#65 on 2026-08-26, the world froze at `2.0.0` on 2026-10-07 (#77), every
scaffold moved to v2 the same day (#114) and 1.0.0 removed ABI v1 (#129).
The Go tree is gone; its `validate` fixtures live on as the conformance
goldens (AGENTS.md, "Conformance goldens").

| phase | work | proves |
|---|---|---|
| 0 | v2 spike: engine crate (Wasmtime), the WIT world, `wasi:http@0.3` over a ported egress admit, one nightly Rust wasip3 guest + one jco JS guest through the greeting contract | the ABI v2 design, pre-1.0, while the Go host serves production |
| 1 | v1 in the same engine crate: core-module path, checkABI, sandbox (private /tmp, env, epoch deadline, memory limiter), the WAT fixtures via the `wat` crate | one engine, both ABIs |
| 2 | periphery - module resolution (OCI/HTTP/path + the cosign check), admission + authz on cedar-policy, manifest (JSON Schema 2020-12), caches, egress in full, the server on function-sdk-rust, metrics, warm-up | the `validate` fixtures green against the Rust binary |
| 3 | guestfn port (init/build/inspect/push/manifest/scaffold; v2 templates) | shared verdicts preserved |
| 4 | swap: the Rust binary is released as the function and the Go host retires, then 1.0 is cut on it | one runtime, v1+v2 |

Phase 0 before phase 1, on purpose: the novel, contract-shaping work is
v2's design, and it should be validated while it's still cheap to change -
the only part of this move the pre-1.0 window matters for. The
parity port (1-2) is large but specification-driven, and contains no
design work.

## Port inventory

| Go | Rust | risk |
|---|---|---|
| wasmtime-go v47 | `wasmtime` (native) | none: gains components, InstancePre/pooling, parse-only |
| cedar-go | `cedar-policy` | low: authz's own hierarchy/pattern code ports 1:1 |
| function-sdk-go server | function-sdk-rust (tonic 0.14 + prost; Go parity reached 2026-08-25) | low but young; metrics still need the prometheus-client tower layer (closed 2026-08-26: the runtime serves the `grpc_server_*` series through its own layer, `crates/function/src/grpcmetrics.rs`) |
| go-containerregistry | `oci-client` + `oci-spec`, or a hand-rolled remote | **highest**: auth flows (`dockerconfigjson` and token refresh) and artifact push with custom layer media types - spike early |
| hand-rolled cosign check | the same, over ring/ed25519-dalek (not sigstore-rs: the same dependency-count reasoning) | low |
| jsonschema/v6 (Go) | `jsonschema` crate (draft 2020-12) | low |
| `kong` | `clap` | none |
| `afero` stores | `std::fs` + `tempfile` | none |
| WAT fixtures via Wat2Wasm | the `wat` crate | none: simpler |

## Risks

- **Bandwidth**: phases 1-3 are months of feature-frozen solo work. The Go
  host keeps releasing fixes during 0-2. The conformance suites bound what
  "done" means.
- **Security-sensitive ports**: the egress dialer judgment,
  redirect re-checks, the boundary-correct repository hierarchy. Their tests
  port with them, and the refusal strings are diffed by the `validate` suite.
- **function-sdk-rust maturity**: the runtime becomes its first hard
  consumer. Acceptable: same owner, and the host exercises exactly the
  server API on which the SDK just gained Go parity.
- **`wasi:http` budgets**: v1's egress budgets act on whole responses; under
  `wasi:http@0.3` bodies are native `stream<u8>`, so the Grant/budget layer
  counts bytes on the stream as the host forwards it. Designed once, in
  phase 0.
- **Specification and toolchain youth**: 0.3.0 is ten weeks old, the Rust
  target is tier 3, and wit-bindgen's async features are new, so churn is
  likely. Mitigated by v1 carrying production and phase 0's guests being
  rebuilt on toolchain bumps rather than promised stability while the world
  was a draft; since the freeze (2026-10-07, #77) Wasmtime's 0.3 disclaimer
  is accepted explicitly: a WASI 0.3 move is re-vendored and taken on
  `2.0.0` in place until the world is published.

## Non-goals

- A typed WIT mirror of the RunFunction messages (blocked by WIT recursion,
  wrong evolution model).
- Waiting for or contributing to wasmtime-go #290/#291/#292, or a CGo
  component shim inside `internal/engine`.
- Keeping the Go host after phase 4: two runtimes is the transitional state,
  not the end state.
- A WASI 0.2 flavour of the v2 world: a toolchain that only reaches 0.2
  (TinyGo today) keeps ABI v1 until it reaches 0.3. This isn't a Wasmtime
  constraint - WASI imports are versioned, so p2 components, p3 components
  and v1 core modules coexist in one host - it's a refusal to define, test
  and keep a second world synchronized with the first. If a 0.2-only
  language ever matters before its toolchain moves, the bridge is
  compose-time adaptation: the guest's `@0.2` imports virtualized over
  `@0.3` by composing an adapter component (plausibly a `guestfn build`
  step), never a second world. A guest that doesn't import WASI at all -
  pure compute over `run`, most composition functions - barely touches the
  difference.
- Retiring ABI v1: it's served for years after v2's launch (mainline Go
  guests). (Superseded, rev 0.4: mainline Go reached v2 through `guestfn
  build`'s embed and wrap, and 1.0.0 removed v1.)
- Changing any guest-facing v1 semantics during the port.

## Open questions

1. A `run-json` export in the v2 world: the JS/Python authors - v2's whole
   audience - would rather not include a Protobuf codec. A second `func`
   with a protojson payload is cheap in WIT and revives the v1
   `wasmfn_run_json` debate (language-support one-pager) where it matters
   most. Rev 0.2: deferred (Jonasz, 2026-08-26) until the jco JS guest
   exists and its codec pain is measurable. Rev 0.3: **decided against**
   (Jonasz, 2026-08-26) - the pain didn't materialize. The TypeScript guest
   was built comfortably on protobuf-es (typed js+dts codec and clean
   bundling) and the Python guest on the official Protobuf package's
   pure-Python fallback, while a second export would double what
   conformance has to cover. Reversible before the world freezes at 2.0.0
   if a future toolchain hurts.
2. Repository layout during the transition: a Cargo workspace beside the Go
   modules in this repo (shared fixtures and one CI), or a sibling repo?
   Recommendation: same repo - phases 1-2 depend on the Go tree's
   `testdata` daily. Rev 0.6: **answered** - the same repo, a Cargo
   workspace beside the Go modules until the port had passed their
   fixtures; the Go tree was then removed and the harness became the
   conformance goldens.
3. Does guestfn keep scaffolding v1 flavours (Go/TinyGo/Zig/C) indefinitely,
   or do the v1 templates freeze at the swap? Rev 0.6: **moot** - every
   scaffold moved to v2 on 2026-10-07 (#114; TinyGo and AssemblyScript
   retired) and 1.0.0 removed ABI v1, so no v1 template is left.
