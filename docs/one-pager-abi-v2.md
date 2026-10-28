# ABI v2 and the Rust host

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 0.7 (rewritten in the present tense over the
  Rust workspace. The Rust host has served since v0.3.0, the world froze at
  `wasmfn:function@2.0.0` on 2026-10-07 and ABI v1 was removed in 1.0.0;
  publishing the world, #106, waits on the `rust` scaffold's stable
  toolchain)

function-wasm runs guests as WebAssembly components against one WIT world,
and its host is written in Rust. This record explains why those are one
decision, what ABI v2 is at design level, what the move left alone, and how
the risks turned out. The contract itself, the world and its rules, is
[the ABI v2 reference](abi-v2.md).

## Why Rust and v2 are one decision

ABI v2 means running components, and the component gap was in the Go binding,
not in the engine. wasmtime-go could load a component but couldn't call one,
define host functions on the component linker or register WASI 0.2 and
`wasi:http` ([#280](https://github.com/bytecodealliance/wasmtime-go/issues/280)),
and the draft PRs #290 to #292 were still open. The Rust `wasmtime` crate is the
reference component host: `component::bindgen!` over a WIT world, host
functions, `wasmtime-wasi`, `wasmtime-wasi-http` with an overridable
`send_request` and the WASI 0.3 async work. Committing to v2 meant either waiting on a volunteer binding effort or
leaving Go. A CGo shim for components was the middle road, and the other
road was moving the host to the engine's own language. The project took the
latter (Jonasz, 2026-08-25).

The move brings these with it:

- Native `cedar-policy`, Cedar's reference engine. The hierarchy
  and pattern code the Go runtime wrote around cedar-go (the
  boundary-correct `Repository` hierarchy, `dialAddress` handling) ported
  across and now sits in `crates/function/src/authz.rs`.
- The Wasmtime API the Go binding never exposed: parse-only inspection, a
  pre-linked `InstancePre` per module so a run only instantiates, and the
  engine's pooled memory budget, which suit a fresh instance per request.
- `guestfn` follows the runtime, since it links the engine for its verdicts,
  so one move covered both.

## What ABI v2 is

A guest is a component targeting the WIT world `wasmfn:function@2.0.0`:

- It exports `run: async func(list<u8>) -> result<list<u8>, string>`.
  Protobuf `RunFunctionRequest` bytes go in and `RunFunctionResponse` bytes
  come out, the same wire format ABI v1 used. The world isn't a typed WIT
  mirror of the messages: WIT has no recursive types, so it can't express
  `Struct` or `Value`, and payload evolution stays Protobuf's job. The
  canonical ABI moves the memory, so a guest has no allocator export.
- It imports a typed `log`, and WASI 0.3 and 0.2 under the sandbox. Egress
  is `wasi:http`, 0.3 `client.send` and 0.2 `outgoing-handler` alike, which
  the host bridges onto one grant held to the egress policy. Guests use their
  language's own HTTP client, and whether the import is backed by the egress
  client follows from `requires.egress.http` under the three layers.
- The binary format is the version: a component is ABI v2, and the runtime
  refuses a core module at load.

The world targets WASI 0.3 from the start (Jonasz, 2026-08-25), accepting
young toolchains for early guests. `wasi:http@0.2` and `@0.3` are different
interfaces, so defining the world against 0.2 and lifting it later would
have meant cutting two worlds and designing the streams and budgets twice.
Wasmtime enabled component-model async by default in v46, so the host had
nothing to wait for. A `run` lifted sync satisfies the async world,
which is what the WAT test fixtures and the `ts`, `python`, `c`, `zig` and
`go` scaffolds use. Wasmtime calls its 0.3 support experimental, so a release that
moves those interfaces is re-vendored and taken on `2.0.0` in place until
the world is published (`abi-v2.md`, "Versioning").

Async matters less here than for most hosts. A fresh instance per request
doesn't need concurrent calls into one instance, which leaves the benefit in
the `wasi:http` import. Its bodies are native streams, and the egress
budgets count bytes as the host forwards them.

## What didn't change

Everything host-language-neutral moved over verbatim: the Input schema and
manifest format, the Cedar semantics of both policy layers, digest-pinned
sources, the three cache stores, the refusal strings, the flags, `/readyz`
and gRPC health, and the metrics series. AGENTS.md ("Parity with the Go
runtime") lists the recorded divergences. The ABI is host-agnostic: the five example guests passed the
ported behavioural suite unchanged.

The port was built against a differential harness that diffed
`function validate` output and exit codes against the Go binary over shared
fixtures and live servers and registries. When the Go tree was removed, the
recorded outputs became the conformance goldens, so a behaviour change is a
deliberate re-record.

## Risks and how they turned out

- Bandwidth. The port was months of feature-frozen solo work while the Go
  host kept getting fixes. The conformance suites bounded what done meant,
  and the Rust host replaced the Go one in v0.3.0.
- Security-sensitive ports: the egress dialer judgment, redirect re-checks
  and the repository hierarchy. Their tests ported with them, and the
  fixtures diff the refusal strings.
- function-sdk-rust maturity: the runtime became its first hard consumer.
  The gRPC layer ended up a raw-codec transport the runtime owns
  (`crates/function/src/grpc.rs`), with the SDK supplying types and helpers.
- Registry and signing clients. The registry client is hand-written over
  `reqwest` (`crates/function/src/oci.rs`) rather than a published OCI crate.
  Signature checking moved to cosign 3 bundles over `sigstore-verify`, so
  the Go runtime's cosign 2 `.sig` format isn't read, the one divergence
  beyond wording.
- `wasi:http` budgets. Budgets that counted whole responses count bytes on
  the stream (`crates/engine/src/wasihttp.rs`).
- Specification and toolchain youth. Churn did land: ABI v1 was removed
  rather than served indefinitely, because Go reached v2 through
  `guestfn build`'s embed and wrap (#114), and the `rust` scaffold still
  pins a beta toolchain, which holds back the world's publication (#106).
- The static musl binary on a `FROM scratch` image, once expected from
  dropping CGo, didn't happen: the image stays on the Chainguard
  `glibc-dynamic` base.

## Non-goals

- A typed WIT mirror of the RunFunction messages.
- A second world for WASI 0.2 alone. A toolchain that reaches only 0.2 would
  need a second world defined, tested and kept consistent with the first.
- Changing a guest-facing semantic during the port.

## Alternatives

- A Go host with a CGo component shim, or waiting on wasmtime-go's binding
  PRs. Both put the project's critical path in someone else's work, and the
  shim would have to track Wasmtime's component API by hand.
- wazero. A probe showed the guest ABI is host-agnostic, which is what made
  the host swap safe, but wazero doesn't run components, so it couldn't carry
  v2.
- ABI v1 kept alongside v2, served indefinitely. The first plan, because
  mainline Go had no WASI beyond wasip1. The spike for #114 moved Go onto v2
  by embedding the world into a wasip1 reactor and wrapping it with an
  adapter, so no scaffold needed v1 and 1.0.0 removed it.
- componentize-go for Go guests. It proved the approach, but `guestfn build`
  does the same embed and wrap with the runtime's own adapter, so users
  don't install anything extra.
- A `run-json` export beside `run`, for guests that would rather skip a
  Protobuf codec. Decided against (Jonasz, 2026-08-26): the TypeScript guest
  came together on protobuf-es and the Python guest on the Protobuf
  package's pure-Python fallback, and a second export would double what
  conformance covers.
- A sibling repository for the Rust host. The workspace lived beside the Go
  modules in this repo while the port ran against their fixtures; the Go tree
  was then removed.
