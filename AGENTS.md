# Function-WASM Agent Guide

This document provides orientation for AI agents and developers working with the function-wasm codebase.

## What is Function-WASM?

### Purpose

function-wasm is a Crossplane composition function that runs a user-supplied WebAssembly module in a [wasmtime](https://wasmtime.dev) sandbox. A module implements the same contract as a native function — `RunFunction(RunFunctionRequest) → RunFunctionResponse` — so users write an ordinary function-sdk-go function (or a TinyGo/Rust/Zig/C guest over the vendored proto), compile it to `wasip1`, publish it (usually as an OCI artifact) and reference it from a Composition step's Input. One installed function serves any number of modules.

The implementation is a Rust workspace (it was ported from Go in 2026-08; the Go tree is gone, but its behaviour is the contract — see "Parity with the Go runtime" below). The repository ships three things:

| deliverable | where | what |
|---|---|---|
| the runtime (host) | `crates/function` (binary `function`) + `crates/engine` | the gRPC function: resolves the module named by the Input, compiles and caches it, runs it per request; `function validate` runs the same admission offline |
| the guest glue | vendored per guest (the Go scaffold writes `internal/wasmfn`; the example is `examples/hello-go/internal/wasmfn`) | linked into a user's `.wasm`: the ABI exports, request/response codec, a `logging.Logger` over the host, `GetConfig`, `HTTPClient`. Not a published module - each guest owns its copy, like TinyGo/Rust/Zig/C own theirs |
| the CLI | `crates/guestfn` (binary `guestfn`) | `guestfn init` scaffolds a guest project (with its `wasmfn.yaml` manifest), `guestfn build` compiles it (and checks its ABI with the runtime's engine, and its manifest), `guestfn inspect` shows what the runtime sees in a module or an artifact, `guestfn push` publishes module and manifest (refusing a module the runtime would refuse), `guestfn manifest validate\|show`, `guestfn scaffold composition` writes a Composition step from a manifest |

## Architecture Overview

### Request Processing Flow

```
Crossplane RunFunctionRequest (raw bytes - the gRPC codec is pass-through)
    ↓
┌──────────────────────────────────────────────────────────────────────────────┐
│ crates/function/src/runner.rs: WasmFunction::handle_raw()                    │
│  1. decode a typed copy for admission; admission::admit(input, ceilings) -   │
│       compositionPolicy compiled (content-hash cached, malformed Cedar →     │
│       fatal); limits → engine RunOptions, or a fatal naming the limit and    │
│       the ceiling flag; module shape checked                                 │
│  2. from::from_composite(input.module, compositionPolicy, observed XR)       │
│       type + from → the XR field decoded into the type's object → concrete;  │
│       the composition layer fences it (pullModule for the location,          │
│       spendCredential for a named credential - default-deny) or refuses      │
│  3. oci::auth_for(req, module) → registry auth        (step credential)      │
│  4. resolver.resolve(module) → Resolved{digest, source}     resolver.rs      │
│       no I/O: oci manifest digest from the ref, http.digest from the Input,  │
│       path hashed by content (stamped by size+mtime); then cosign::Verifier  │
│       - the signature check, before any cache, once per digest per process   │
│  5. cache.get(digest, fetch)                                cache.rs         │
│       memory (idle TTL) → compiled artifact on disk → fetch (oci: manifest   │
│       GET → layer digest; blob store on disk → source, verified against the  │
│       blob digest; a tar layer yields /fn.wasm exactly) + engine.compile     │
│       (checkABI after wasmtime decodes it — the one ABI check) + serialize   │
│  6. resolver.manifest(resolved) → the module's manifest (an OCI layer, or a  │
│       wasmfn.yaml a path/http source names by reference, else none);        │
│       admission::admit_requires(requires, ceilings, compositionPolicy,       │
│       principal) - the three-layer AND: the manifest requests, the           │
│       composition layer permits (scoped default-permit), the operator layer  │
│       permits (default-deny) → Capabilities{private tmp, HTTP grant, env},   │
│       or a fatal "module <desc> requires …, which the … does not permit";    │
│       manifest.check (minRuntime vs the stamped version, config schema);     │
│       sandboxenv::materialize(env bindings, step credentials - the pull      │
│       credential withheld) → the run's env                                   │
│  7. step_slots.acquire(digest, limits.concurrency)   per-step semaphore      │
│       (0 = skip); waits are capped by the request's gRPC deadline            │
│  8. engine.run(module, raw request bytes, opts)             crates/engine    │
│       run slot (fair round-robin by digest) and memory reservation first     │
│       when bounded; fresh Store: WASI argv=["function"], no net; fs and env  │
│       only as granted (a private /tmp made before and removed after the      │
│       store), epoch deadline capped by the gRPC deadline, memory limiter;    │
│       _initialize → wasmfn_alloc → copy req → wasmfn_run → packed ptr/len    │
│       host import wasmfn.log → tracing with module/digest attached           │
│       host import wasmfn.http → the run's egress client (grant, block list,  │
│       budgets, audit line) or an in-band refusal; answer written via the     │
│       guest's wasmfn_alloc (re-entrant)                                      │
│  9. return the guest's raw response bytes verbatim (meta appended at the     │
│     wire level only when the guest omitted it); the caller's raw request     │
│     bytes were forwarded verbatim too, with only the withheld pull           │
│     credential edited out (protowire.rs) - fields newer than the vendored    │
│     proto survive in both directions                                         │
│     trap / timeout / OOM / bad ABI / fetch / compile → fatal result          │
└──────────────────────────────────────────────────────────────────────────────┘
    ↓
Crossplane RunFunctionResponse (whatever the module produced)
```

### Key Components

```
crates/function/            the runtime crate: a library (everything below) + the `function` binary
  src/main.rs                 clap CLI: serve (default) + validate subcommand; every ceiling flag
                              (module-dir, max-module-size, module-timeout, module-memory-limit,
                              sandbox-policy-file, cosign-key, egress-rate-limit-*, cache and
                              concurrency bounds, max-cache-size, warm-modules, ttl, health-address,
                              metrics-address); opens the three disk stores, compiles the operator
                              policy and IP rules (malformed → exit), starts the sweeps (10 min:
                              cache LRU to --max-cache-size + cache_bytes gauges, idle step slots,
                              idle rate limiters), the /metrics and /livez//readyz listeners, warm-up
                              (flips /readyz and gRPC health), and serves through grpc.rs
  src/runner.rs               WasmFunction::handle_raw - the nine steps above, on raw request bytes;
                              fatal() logs the outcome and counts requests_total
  src/grpc.rs                 the raw-codec gRPC transport: RawCodec (bytes in/out), RawFunctionServer
                              (routes /apiextensions.fn.proto.v1.FunctionRunnerService/RunFunction,
                              parses grpc-timeout into the run deadline), and serve() - mTLS from the
                              certs dir, v1+v1alpha reflection, gRPC health handed back NOT_SERVING
                              for warm-up to flip; ~50 lines of transport built from public pieces
                              (a deliberate copy - see the function-sdk-rust decision below)
  src/protowire.rs            wire-level protobuf surgery: strip_credential (drops one field-7 map
                              entry), append_meta (field 1) - how the transparent proxy edits raw
                              bytes without decoding them
  src/validate.rs             function validate: multi-doc YAML/JSON (- stdin) → per step: strict
                              decode (unknown fields → warnings), admit, --xr → from_composite,
                              --resolve → resolve + verify + fetch + engine.inspect + admit_requires;
                              text or --output json; exit 0 admitted / 1 refused / 2 tool failure
  src/admission.rs            admit (step 1) and admit_requires (step 6, the three-layer AND) -
                              shared verbatim with validate
  src/authz.rs                Cedar PDP (cedar-policy): CompositionPolicy (pullModule/spendCredential
                              fences over a boundary-correct Repository hierarchy, scoped
                              default-permit sandbox narrowing) and OperatorPolicy (default-deny
                              grants, requireSignature per repository, dialAddress → IpRules);
                              refusal strings stay in the callers
  src/resolver.rs             the three sources: Path under --module-dir (content-hashed, stamped),
                              HTTP with stated digests, OCI by manifest digest; manifests by
                              reference (manifestPath, manifestURL/manifestDigest); fetch timed into
                              fetch_duration_seconds, the blob store counted as cache events
  src/oci.rs                  the distribution client: manifest/blob GET, raw_manifest, push_blob +
                              push_manifest (guestfn), anonymous/Basic/Bearer auth, the local Docker
                              config (keychain_auth); wasm_layer/manifest_layer/extract_wasm rules;
                              testregistry (feature "testutil") serves and accepts artifacts in tests
  src/location.rs             go-containerregistry-compatible reference normalization: pinned refs
                              (the runtime), any refs with tag/digest (guestfn), http locations
  src/cosign.rs               cosign key-based verification (sigstore-rs crypto over the runtime's
                              own registry client); keyless deliberately unsupported
  src/cache.rs                the module cache: memory (idle TTL, LRU bound) over the compiled-
                              artifact store, single-flight loads with compile slots; cache events
  src/store.rs                the content-addressed disk stores (modules, compiled/<version>,
                              manifests), verify-on-read, the LRU sweep to --max-cache-size,
                              stale-version reaping
  src/egress.rs               HTTP egress through the host: SSRF block list judged per resolved
                              address (operator dialAddress rules punch holes), redirects re-checked
                              per hop, fixed budgets, the process-wide rate limit, one audit line
                              and http_requests_total per request
  src/manifest.rs             the module manifest: parse (artifact layer), load (wasmfn.yaml,
                              unknown top-level fields refused), validate, check (grants, config
                              schema via jsonschema, minRuntime vs runtime_version()), summary, json
  src/from.rs, input.rs,      module.from fencing, the Input types, egress rule and env binding
  egress_rules.rs,            shapes, quantity/duration parsing, env materialization
  sandboxenv.rs, quantity.rs
  src/ops.rs                  /livez + /readyz (warm-up gated) and /metrics on plain HTTP; warm()
  tests/conformance.rs        the golden conformance suite (see "Conformance goldens" below)
  tests/guests.rs             the five-guest behavioural suite (see "Testing")
  tests/raw_client.rs         a raw-codec gRPC client proving byte transparency end to end
  testdata/validate/          the validate fixture corpus (one file per refusal family, policies, XR)
  testdata/conformance/       the recorded goldens
crates/engine/              the wasmtime engine - the only crate that imports wasmtime
  src/lib.rs                  Engine (compile + checkABI, inspect → the full module shape, leases,
                              fair scheduler, memory pool, on-demand epoch ticker), RunOptions
                              (limits + private tmp/env + HTTP + deadline), version() namespacing
                              the compiled cache (build.rs reads wasmtime's version from Cargo.lock)
  src/run.rs                  one run: slots and memory first (waits capped by the deadline), fresh
                              store, WASI config, epoch deadline, ABI calls, run_duration by outcome
  src/abi.rs, sandbox.rs,     checkABI (exports and both imports' exact types), the private /tmp and
  hostlog.rs, hosthttp.rs,    env wiring, the wasmfn.log and wasmfn.http host imports, the JSON
  wire.rs, duration.rs,       payload shapes, Go-style duration parse/format, per-digest step slots
  concurrency.rs, metrics.rs  and the Prometheus series (same names/labels/buckets as the Go runtime)
crates/guestfn/             the CLI crate (binary `guestfn`)
  src/main.rs                 clap CLI: init/build/push/inspect/manifest/scaffold; shared helpers
  src/scaffold.rs             template rendering ([[ ]] delimiters, zigid/zigfp helpers), write with
                              overwrite refusal; the golden and render-matches-the-examples tests
  src/buildcmd.rs             toolchain detection (Cargo.toml → rust, build.zig → zig/c, vtprotobuf
                              in go.mod → tinygo, else go), the builds, the ABI verdict, wasmfn.yaml
                              validation, the example-config warning
  src/push.rs                 the CNCF wasm OCI artifact (wasm layer, manifest layer, layerDigests
                              config, OCI annotations, SOURCE_DATE_EPOCH-reproducible), upload
  src/inspect.rs              file → engine.inspect; reference → manifest, layers, annotations, the
                              module-layer rule, the manifest summary; --pull; text/json
  src/manifestcmd.rs          manifest validate <file> / manifest show <ref>
  src/composition.rs          scaffold composition: the step, a config skeleton from the schema, the
                              commented compositionPolicy skeleton from the manifest's requires
  templates/<lang>            the five template sets (each is its example rendered for itself)
  testdata/<lang>             the golden scaffolds (UPDATE_GOLDENS=1 cargo test regenerates)
examples/hello-go           the Go example guest — separate go.mod; vendors its ABI glue under
                            internal/wasmfn (no external SDK); built by tests, CI's render job and
                            local rendering, never published
examples/hello-tinygo       the same guest, TinyGo + vtprotobuf — separate go.mod, `make generate`
examples/hello-rust         the same guest, Rust + prost — Cargo crate (excluded from the workspace:
                            guests depend on nothing in this repository)
examples/hello-zig          the same guest, Zig + zig-protobuf — build.zig
examples/hello-c            the same guest, C + nanopb + cJSON, compiled by zig cc — build.zig
examples/render.sh          shared: cargo-build the runtime serving an example dir, crossplane
                            render its example/, optionally --check
package/                    crossplane.yaml + the checked-in Input CRD (documentation for tooling;
                            Crossplane never installs a function's Input CRD - the CRD is maintained
                            by hand now that the Go types that generated it are gone)
docs/abi.md                 the language-agnostic host/guest contract
```

## Key Concepts

### Input

The function receives an `Input` (`wasm.fn.crossplane.io/v1beta1`) — a KRM-like object (`crates/function/src/input.rs`):

- `module` — `type: OCI|HTTP|Path` (required) + exactly one of `oci{ref, credentials}`, `http{url, digest, manifestURL, manifestDigest}`, `path` (+ `manifestPath`), or `from` (the XR field holding that object)
- `compositionPolicy` — raw Cedar, the composition author's layer: fences `from` sources (`pullModule`/`spendCredential`, default-deny) and may narrow sandbox capabilities (scoped default-permit); read from the Input only, never from the XR
- `limits` — `timeout`, `memory`, `concurrency`, each ≤ the runtime's ceiling flag (concurrency silently capped)
- `config` — opaque; the guest reads it via `GetConfig` - non-secret module configuration lives here

There is no `sandbox` field: what a module gets beyond the default sandbox is decided per capability by three AND-combined layers (`docs/one-pager-three-layer-authz.md`) - the module's manifest requests it (`requires.filesystem.privateTmp`, `requires.egress.http`, `requires.env` credential bindings), the Input's `compositionPolicy` permits it, and the operator's Cedar `--sandbox-policy-file` permits it (default-deny: no policy file, no capability). The user-facing field reference lives in `README.md` ("Input reference"); keep it in sync with `input.rs` and the CRD under `package/input/`.

### ABI v1

Guest exports `memory`, `wasmfn_alloc(u32)->u32`, `wasmfn_run(u32,u32)->u64`, optional `_initialize`; host imports `wasmfn.log(u32,u32,u32)` with a JSON `{"msg","kv"}` payload and `wasmfn.http(u32,u32)->u64` with a JSON request answered through the guest's `wasmfn_alloc` **re-entrantly** and returned as `ptr<<32|len`; protobuf `RunFunctionRequest`/`RunFunctionResponse` on the wire. `docs/abi.md` is authoritative; `engine::abi::check_abi` enforces it at load, over wasmtime's decoded types — the one ABI check, whose verdict `engine.inspect` reports to `guestfn build`/`push`/`inspect` and `function validate --resolve`. Payload evolution is protobuf's (and, for imports, JSON's) job; a mechanics change is a new set of export names. ABI v2 (the component model) is the plan in `docs/one-pager-abi-v2.md` — this Rust host is its phase 1.

### The transparent proxy is wire-level

The host forwards the whole request and returns the whole response — requirements/extra-resource round trips work with no runtime knowledge. In Rust this needs care: prost drops protobuf fields it does not know, so the gRPC layer uses a raw pass-through codec (`grpc.rs`), the runtime decodes only a typed *copy* for admission, and the guest receives the caller's exact bytes with the withheld pull credential edited out at the wire level (`protowire.rs`). The guest's response bytes travel back untouched; `meta` is appended as raw bytes only when the guest omitted it. `tests/raw_client.rs` proves an unknown field survives the whole served stack. Never route the forwarded payload through prost structs.

### Parity with the Go runtime

The Go implementation was the reference until 2026-08; the contract is **logical compatibility**: the same Inputs admitted, the same requests refused for the same reasons, nothing running wider than the Go runtime would allow. Most admission and policy refusal strings match Go verbatim (the conformance goldens hold them); wording-only divergences are accepted (alpha), the recorded ones being: guest log kv rendered as one JSON field, egress transport-error text (reqwest's words, no 64KiB response-header cap, no HTTP/2 attempt), and limits parse-error wording. Anything the runtime does not carry is refused with a message naming it, never silently ignored.

### Conformance goldens

`crates/function/tests/conformance.rs` runs `function validate` over the fixture corpus and generated modules/servers/registries and compares stdout, stderr and exit codes against goldens under `testdata/conformance/`. The goldens were recorded from this runtime the day it last diffed **byte-identical** against the Go runtime's own `function validate` (the original differential harness, retired with the Go tree), so they carry the Go runtime's words wherever parity held. A change fails the suite until re-recorded deliberately with `UPDATE_CONFORMANCE=1 cargo test` — treat a re-record as a user-visible behaviour change and say so in the commit.

### Admission and validation

Crossplane never installs a function's Input CRD, so every rule of the Input is enforced by the runtime on every request — `admission::admit`, then `from::from_composite`, and once the module's manifest is read, `admission::admit_requires` — and nowhere else in a cluster. `function validate` runs the same functions over Compositions offline against the same ceiling flags, printing the runtime's own refusal strings; `--resolve` adds resolve → verify → fetch → `engine.inspect` → `admit_requires`. Keep the two in lockstep: a new Input rule goes into `admit` (or the resolver) so both paths apply it, and a new refusal gets a fixture under `testdata/validate/` plus a conformance golden; a new ceiling flag goes into both `serve` and `validate` in `main.rs`/`validate.rs`.

### Error semantics

A guest's returned error becomes a fatal result on a fresh response, a panic in the host is caught into a fatal result, and anything that stops the instance (trap, exit, deadline, memory limit) or the load (fetch, digest, compile, exports) is a fatal result from the host naming the module. The host never returns a gRPC error for guest problems and never crashes on them.

### Caches

Three on-disk stores under `/tmp/function-wasm-cache` (fixed; `store.rs`): `modules/<digest>` — every fetched blob, verified on read, never held in memory; `compiled/<engine::version()>/<digest>` — wasmtime artifacts (`version()` = the wasmtime crate version from Cargo.lock + OS/arch, distinct from the Go runtime's namespace on purpose; other version dirs reaped at startup once a day old); `manifests/<digest>`. In memory: compiled modules only, idle TTL 10 min, LRU-bounded by `--max-cached-modules`, single-flight loads with `--max-concurrent-compiles` slots, or nothing with `--enable-memory-cache=false`. Keys are digests stated in the Input; `--max-cache-size` LRU-sweeps the disk stores to nine tenths at startup and every ten minutes. Full design: `docs/one-pager-cache.md`.

### Readiness, warm-up and the run bound

Readiness is answered twice — gRPC health on the function port and plain-HTTP `/readyz` (`/livez` always 200) on `--health-address` `:8081` — and starts as NOT_SERVING/503; warm-up loads every `--warm-modules` entry through the request's own path (at most `--max-concurrent-compiles` at once, failures logged, never fatal) and then both flip — while the server already listens, so a probe reads not-ready rather than a refused connection and an early request is served cold or joins the load in flight. `--max-concurrent-runs` is a fair round-robin slot scheduler keyed by module digest in the engine; slot, memory-pool and step-slot waits are all capped by the request's own gRPC deadline, and a request cut short while waiting is a fatal result that is not counted as a run.

### Metrics

`crates/engine/src/metrics.rs` registers the same series as the Go runtime — `function_wasm_module_{compile,fetch,run}_duration_seconds`, `runs_in_flight`, `cache_events_total`, `cache_bytes`, `http_requests_total{outcome}`, `requests_total{outcome}` — on the prometheus default registry, served at `/metrics` on `--metrics-address` (default `:8080`, function-sdk-go's port). Never add a module/digest/host label - unbounded cardinality. `metrics::sample` reads one series back for tests.

### Signatures

`--cosign-key` loads PEM public keys into `cosign::Verifier` (sigstore-rs crypto, key-based only); verification runs **before** the caches, once per manifest digest per process, over the runtime's own registry client (same auth path as the pull); non-OCI sources are refused when a signature is required. Keyless (Fulcio/Rekor) is deliberately unsupported.

### One-pagers

Design documents under `docs/one-pager-*.md` follow one pattern: the H1 is the feature name, then `* Owner / * Reviewers / * Status: Draft | Implemented, revision x.y`, then the body. Bump the revision when the design changes. They describe the design in the Go era's file layout; the design decisions hold, the paths map to `crates/` as described above.

## Development Guide

### Building

```bash
cargo build --workspace                # engine, runtime, guestfn
cargo run -p function-wasm -- --insecure --module-dir=examples/hello-go
cargo run -p guestfn -- inspect examples/hello-go/fn.wasm

# The example guest (with its vendored internal/wasmfn glue) must also build for wasm
(cd examples/hello-go && GOOS=wasip1 GOARCH=wasm go build -buildmode=c-shared -o /dev/null .)

# The runtime image (multi-arch; FUNCTION_WASM_VERSION stamps a release)
docker buildx build --platform linux/amd64,linux/arm64 --target image .

# A Crossplane package
crossplane xpkg build -f package --embed-runtime-image=runtime
```

### Testing

```bash
cargo test --workspace                    # everything below
cargo test -p function-wasm-engine        # the engine over WAT fixtures
cargo test -p function-wasm               # runtime units, conformance goldens, five-guest suite, raw client
cargo test -p guestfn                     # scaffold goldens, render-matches-the-examples, CLI against an in-memory registry
(cd examples/hello-go && go test -race ./...)
(cd examples/hello-tinygo && go test -race ./...)
(cd examples/hello-rust && cargo test)
(cd examples/hello-zig && zig build test)
(cd examples/hello-c && zig build test)
```

`crates/function/tests/guests.rs` builds all five example guests and runs each through the whole host with the same expectations — default and configured greeting, a greeting fetched through the host's egress (via an OCI manifest layer and via `module.manifestPath`) and refused without a grant, guest-side fatal on a bad config, guest logs — the five guests must stay behaviourally identical. A guest whose toolchain is not on PATH is skipped. Toolchains: `rustup target add wasm32-wasip1`; TinyGo ≥ 0.41; Zig 0.16; `protoc` only for the codec regeneration targets; `nanopb_generator` (`pip install nanopb==0.4.9.1`) only for `zig build gen-proto` in hello-c.

Goldens: `UPDATE_CONFORMANCE=1 cargo test -p function-wasm --test conformance` re-records the conformance goldens (deliberate behaviour changes only); `UPDATE_GOLDENS=1 cargo test -p guestfn` regenerates the scaffold goldens after a template change.

### Test Patterns

Unit tests live in `#[cfg(test)] mod tests` blocks beside the code; integration suites under `tests/`. Guest modules for tests are WAT fixtures assembled with the `wat` crate implementing ABI v1 (the engine's tests carry fixtures that misbehave in one way each; the sandbox is tested through raw WASI — a path escape is `module exited with status 63` (EPERM), a missing file 44 (ENOENT), no pre-open 8 (EBADF)). Registry-backed tests use `oci::testregistry` (feature `testutil`): an in-memory distribution registry that serves and accepts artifacts, optionally behind the Bearer token flow. Expected `RunFunctionResponse`s are constructed whole and compared with `assert_eq!` on the prost types — including fatal cases, whose per-guest message wording is blanked before the comparison.

### Linting

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
(cd examples/hello-go && golangci-lint run ./...)   # the Go guests keep Go lint
(cd examples/hello-rust && cargo fmt --check && cargo clippy --all-targets -- -D warnings)
(cd examples/hello-zig && zig fmt --check build.zig src/main.zig)
(cd examples/hello-c && zig fmt --check build.zig)
```

### Coding Conventions

- Rust edition 2024; comments explain *why*, as the existing ones do; prefer self-documenting code.
- English only; inclusive terminology (allowlist/blocklist, primary/replica, main branch).
- Conventional commits: `<type>(<scope>): <subject>`, imperative, ≤ 50 chars, one logical change per commit.
- Only `crates/engine` imports `wasmtime`/`wasmtime-wasi` — the engine is the seam if the runtime ever changes.
- Only `crates/function/src/authz.rs` imports `cedar-policy`.
- Errors that reach users are `String`s carrying the runtime's exact refusal wording — the wording is contract (conformance goldens); route new refusals through the same phrasing patterns.
- The `internal/wasmfn` glue must stay buildable natively (portable `Register`/`NewLogger`/`GetConfig`/`HTTPClient`) so a guest's own tests run natively; only the exports and the two host imports are `//go:build wasip1`. Edit it in `examples/hello-go/internal/wasmfn`, then mirror to `crates/guestfn/templates/go/internal/wasmfn/*.go.tmpl`; the render-matches-the-examples test keeps them in step.

## Common Development Tasks

### Adding an Input Field

1. Add the field to `crates/function/src/input.rs` (serde) and enforce its rules at runtime — `admission::admit`, the resolver, or `from.rs` for something the XR may choose (never read `compositionPolicy` or `limits` from the composite). A new *capability* is not an Input field at all: it is a manifest requirement (`manifest.rs` `Requires` + its shape check) decided by `admission::admit_requires` under both Cedar layers (`authz.rs`: a new action in the shared schema, permits on both policies) → engine `RunOptions` + the sandbox wiring
2. Everything `admit`/`admit_requires` checks is what `function validate` checks — add a fixture under `testdata/validate/` and a conformance golden for the new refusal; a new ceiling flag goes into `main.rs` so `serve` and `validate` share it
3. Update the hand-maintained CRD under `package/input/` and the README's "Input reference" table

### Adding a host import (ABI)

`wasmfn.http` (`crates/engine/src/hosthttp.rs`, the guest side in `examples/hello-go/internal/wasmfn/http*.go` and the matching template) is the worked example:

1. Define it in the engine (`linker.func_wrap(HOST_MODULE, name, fn)`), allow it in `check_abi` with its exact type; reach per-run state through the store's `CallState` and pass what it needs in `RunOptions`. To hand bytes back, call the guest's `wasmfn_alloc` re-entrantly — copy the request out first and re-read memory afterwards, the guest may grow it — and return `ptr<<32|len`; keep failures inside the payload (never a trap for a refusal)
2. Add the guest side to the glue (a `//go:wasmimport wasmfn <name>` in a `_wasip1.go` file, a portable fallback behind a swappable package var so the codec is testable natively)
3. Document it in `docs/abi.md`; cover it with a WAT fixture in the engine's tests and, through the five-guest suite, in every guest

### Changing the scaffold

Edit the example **and** its template set under `crates/guestfn/templates/<lang>` (templates use `[[ ]]` delimiters so source braces survive; the examples are the templates rendered for themselves; `wasmfn.yaml.tmpl` is the manifest every flavour ships; the zig and c `build.zig.zon.tmpl` take the project's identifier and fingerprint from the `zigid`/`zigfp` template helpers), then `UPDATE_GOLDENS=1 cargo test -p guestfn` to refresh the goldens; the render-matches-the-examples test keeps each pair in sync (everything but `go.mod`; the examples' `Makefile`, `Cargo.lock` and the go example's glue tests are extra). The four vendored `run_function.proto` copies must stay identical below their per-language header (a scaffold test enforces it); after a proto bump, update the copies in the templates **and** examples, regenerate the checked-in codecs (`make -C examples/hello-tinygo generate`, `zig build gen-proto` in hello-zig and hello-c), and refresh the goldens; CI's render jobs fail on drift.

### Rendering Locally

```bash
make -C examples/hello-go render          # build fn.wasm (guestfn via cargo), serve it, crossplane render example/
make -C examples/hello-go render-check    # same, asserting the output — what CI's render job runs
make -C examples/hello-tinygo render      # the TinyGo guest (tinygo on PATH)
make -C examples/hello-rust render        # the Rust guest (cargo + wasm32-wasip1 + protoc)
make -C examples/hello-zig render         # the Zig guest (zig on PATH)
make -C examples/hello-c render           # the C guest (zig on PATH: zig cc builds it)
```

By hand: `cargo run -p function-wasm -- --insecure --debug --module-dir=examples/hello-go`, then in the example: `cargo run -p guestfn -- build` and `crossplane render example/xr.yaml example/composition.yaml example/functions.yaml --include-function-results`. `functions.yaml` uses the Development runtime, so the function must be running locally; the render engine itself runs in Docker.

## Key Dependencies

- `wasmtime` / `wasmtime-wasi` — the sandbox (Cranelift, pure Rust). Each major may change APIs; only `crates/engine` touches them, and `engine::version()` re-namespaces the compiled cache automatically on a bump.
- `function-sdk-rust` — the gRPC/protobuf types (prost), `request`/`response`/`resource` helpers and the CLI `Args`; the generated FunctionRunnerService *client* types serve tests, while serving goes through the raw codec.
- `cedar-policy` — both policy layers.
- `sigstore` (features `cosign`, `rustls-tls`, no default features) — cosign key verification crypto only; fetching stays on the runtime's own registry client.
- `reqwest` (blocking, rustls) — the egress client and the registry client.
- `prometheus` — the metrics registry.
- `clap` — both CLIs.

## Important Design Decisions

The Go-era decisions below still describe the product's behaviour; the runtime that enforces them is now Rust.

- **Rust host for ABI v2** (Jonasz, 2026-08-25, `docs/one-pager-abi-v2.md`): the host moved to Rust (native wasmtime, native Cedar) to enable ABI v2 on the component model with wasip3 from the start; ABI v1 is served indefinitely. The port was built against a differential conformance harness that byte-diffed `function validate` against the Go binary over shared fixtures and live servers/registries; when the Go tree was removed the harness became the golden suite. The five example guests passed the ported behavioural suite unchanged - the ABI is host-agnostic.
- **The transparent proxy is wire-level** (2026-08-25): prost has no unknown-field retention, so the gRPC layer keeps raw bytes (a pass-through codec), admission decodes a typed copy, and credential withholding/meta fill are wire-level edits (`protowire.rs`). The alternative - typed round-trips plus prompt proto bumps - was rejected: the response direction (a guest built with a newer proto than the deployed runtime) cannot be fixed by vendoring speed, and the failure is silent.
- **The gRPC transport is ~50 owned lines, not an SDK abstraction** (Jonasz, 2026-08-26): function-sdk-rust keeps `serve` alone; a serve_service/builder API was built, reworked and reverted (function-sdk-rust PRs #1-#4) because an abstraction with one caller cannot anticipate the next customization and the whole transport is small, stable glue over public crates (tonic, tonic-health, tonic-reflection, the SDK's `FILE_DESCRIPTOR_SET`). grpc.rs owns it, which is also what lets warm-up flip gRPC health (NOT_SERVING until warmed, Go's readiness shape).
- **Conformance is a ratchet** (2026-08): during the port, every validate case either matched Go byte-for-byte or sat on a known-gaps list required to keep differing (a gap that closed had to be removed). The golden suite keeps the ratchet's spirit: recorded outputs are the contract, and changing them is an explicit, reviewed act.
- **wasmtime is the only reader of a module** (Jonasz, 2026-08-17): no second wasm decoder; `engine.inspect` compiles with wasmtime and reports the shape and checkABI's verdict, so every verdict `guestfn` or `validate` prints is the runtime's.
- **`FROM scratch` images hold the module at `/fn.wasm`** (Jonasz, 2026-08-17): the tar path stays for `COPY fn.wasm /` images, but the resolver never picks "the first `.wasm` file"; raw `application/wasm` layers (`guestfn push`, `oras push`) are the recommended shape.
- **The module manifest is a layer of the OCI artifact, not a custom section** (Jonasz, 2026-08-17): covered by the pinned manifest digest and a cosign signature, written by `guestfn push` from `wasmfn.yaml`, parsed with no wasm walker. It is a request, never a grant: `admit_requires` runs after admission and load, before the run, so a manifest can only refuse earlier - never make a run possible. `path`/`http` sources may name a `wasmfn.yaml` by reference instead (`docs/one-pager-manifest-less-sources.md`).
- **Three-layer authorization** (Jonasz, 2026-08-19): a capability is granted iff the module's manifest requests it, the Input's `compositionPolicy` permits it and the operator's `--sandbox-policy-file` permits it - three AND-combined layers, each able only to narrow the one above. The composition layer is scoped default-permit for sandbox actions and default-deny for `from`-source fencing; static sources bypass it. `docs/one-pager-three-layer-authz.md`.
- **Cedar-only sandbox enablement** (Jonasz, 2026-08-19): the operator's Cedar `--sandbox-policy-file` is the sole authority that enables a sandbox capability, evaluated default-deny. With no policy file every sandbox grant is refused - which is all most modules need.
- **Sandbox filesystem and env are WASI, not ABI** (Jonasz, 2026-08-16): the private `/tmp` is a per-run temp dir under the OS temp dir ($TMPDIR is the operator's quota knob), pre-opened at `/tmp` and removed after the store; env is exactly the materialized bindings. Nothing new for guests to import, so all five languages are equal.
- **No host mounts** (Jonasz, 2026-08-16): a module's inputs come through the request; mapping any part of the pod's filesystem into a module is a boundary the runtime does not offer; the private `/tmp` is the only directory a guest ever gets.
- **HTTP egress goes through the host, never a socket** (Jonasz, 2026-08-16): the guest asks (`wasmfn.http`), the host resolves, judges every resolved address against the default block list (operator `allowedCIDRs` punch holes, `blockedCIDRs` add, an explicit block wins), dials the checked address, applies the module's admitted rules on the first request and every redirect hop, enforces per-run budgets, and writes one audit line plus an outcome-labelled metric. A refusal is a JSON error the guest reads, never a trap. The response travels back through the guest's own `wasmfn_alloc`, re-entrantly.
- **Guest error → fatal result** instead of a gRPC error: crossplane treats both as a failed step, fatal results are visible in `crossplane render --include-function-results`, and the wire stays one message.
- **Memory-export ABI, not stdin/stdout**; **fresh instance per request** (hermetic, no reentrancy; the expensive compile is cached by content digest in memory and as a wasmtime artifact on disk); **digests are stated, not discovered** (OCI refs `@sha256:`-pinned, `http.digest` required, no tags alone, no request-time resolution).
- **Disk caches are bounded by LRU sweep, not per-entry policy**: `--max-cache-size` (off by default) removes least recently used entries across the stores at startup and every ten minutes; entries are immutable and reproducible, so removal is always safe.
- **No shared guest SDK module; every guest owns its glue** (Jonasz, 2026-08-19): the Go scaffold vendors package `wasmfn` into each project under `internal/wasmfn`; TinyGo/Rust/Zig/C carry the vendored proto and their own glue. No published module, no version pin, no lockstep tags. A glue fix reaches existing guests only by re-scaffolding or copying - fine for stable ABI-v1 plumbing.
- **C guests build with `zig cc`, not wasi-sdk, and talk protobuf through nanopb with `fallback_type:FT_POINTER`** (Jonasz, 2026-08-19); **TinyGo guests use vtprotobuf** (protobuf-go's codec panics under TinyGo); **WASI argv is always `["function"]`** (an empty argv traps at `_initialize` because klog's init indexes `os.Args[0]`).
- **Warm-up runs while the server listens, health NOT_SERVING until it is done**: a closed port for minutes of compiling would fail a liveness probe and tell a probe nothing; an early request is simply cold. Failures never hold readiness back.
- **The run bound is the engine's, taken after the load and outside the run metric**; fair round-robin per module digest so one hot module cannot take every slot; a request cut short while waiting never ran and is not counted.
- **`function validate` lives in the runtime binary**: the checks are the operator's — the same flags, env, policy file and version as the pod (`docker run <package image> validate …` works: the package image is the runtime image).
- **No local-loop machinery** (Jonasz, 2026-08-17): the loop is `guestfn build` + `crossplane render` against `cargo run -p function-wasm -- --insecure --module-dir=.` (`examples/render.sh`, `make render`) and `function validate`. Guest stderr stays the pod's.
- Historical (Go era, superseded by the port but kept for context): wasmtime-go over wazero; not Extism; the Chainguard `glibc-dynamic` base over distroless (still the runtime image base - it scanned clean); the Input's `policy`/`sandbox` fields deleted for the three-layer model; sandbox types before behaviour.

## Releasing

Releases are driven by two skills; use them rather than improvising the branch/tag/publish sequence:

- **`/cut-release`** — a new minor or major version from `main` HEAD: new `release-X.Y` branch, tag (`.github/workflows/tag.yml`), GitHub release, package publish (`publish-pkg.yml` → `ghcr.io/jonasz-lasut/function-wasm`, mirrored to `xpkg.upbound.io/jonasz-lasut/function-wasm`; the version is stamped into the binary as `FUNCTION_WASM_VERSION` — what module manifests' `minRuntime` rules are checked against), signing/attestation (`supplychain.yml`). The bump size is the user's choice, never inferred.
- **`/remediate-cves`** — a patch release on the current `release-X.Y` branch for CVEs found by `grype-scan.yml` (weekly against the latest release). A wasmtime crate bump is in scope there (it is the sandbox's own security fix) — `crates/engine` only.

## Troubleshooting

- **Fatal `_initialize failed: trap` from a Go guest**: the guest panicked during package init; its stack is in the function pod's stderr.
- **`module imports X.Y, which the host does not provide`**: the module needs an import outside `wasi_snapshot_preview1`, `wasmfn.log` and `wasmfn.http`; it was built for another host or uses sockets/threads.
- **`does not export "wasmfn_run"`**: not built as a reactor with the exports — for Go, `-buildmode=c-shared` and `wasmfn.Register` in an `init`.
- **First request slow, then fast**: expected — compile is per digest; the artifact under `/tmp/function-wasm-cache/compiled` makes the next process fast too if that path is on a volume.
- **`module.oci.ref … tags are not supported`**: pin the reference to the manifest digest — `repo@sha256:…` or `repo:tag@sha256:…`, as `guestfn push` prints it.
- **`module layer is a tar archive without /fn.wasm`**: a `FROM scratch` image must `COPY` the module to `/fn.wasm` exactly. Prefer `guestfn push` / `oras push` (a raw `application/wasm` layer).
- **`guestfn build` says `built fn.wasm, but the runtime would refuse it: …`** (or `guestfn push` refuses): the module lacks the ABI; the message is the runtime's own load-time refusal. `guestfn inspect fn.wasm` lists what the module exports and imports.
- **`module oci … requires egress GET to host "x" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit`** (or `… which the compositionPolicy does not permit`; the same pair for the private /tmp and env forms; or `requires runtime vX or newer, this is vY`): the module's manifest declares a need the named policy layer does not permit - add a `permit` to that layer or use a module that needs less.
- **`module oci … config does not match the module's schema: /greeting: got number, want string`**: the Input's `config` fails the module's `config.schema`.
- **`function validate` exits 1**: at least one step is refused - the line names the runtime's reason; exit 2 is the tool's own failure. Run it with the flags the runtime is started with.
- **`module.path is refused`**: the runtime was started without `--module-dir`.
- **`module.from: … names a OCI source, but the Input has no compositionPolicy`**: a `module.from` OCI/HTTP source requires a `compositionPolicy` whose `pullModule` permits its repository; add the policy, or name the source statically.
- **`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`** (or `limits.timeout … --module-timeout`): lower the limit or raise the flag.
- **`module … requires a private /tmp (requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities`** (and the env/egress forms): mount a Cedar `--sandbox-policy-file` with a matching permit or use a module that requires nothing.
- **`the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot create one under …`** at startup: point `TMPDIR` at a writable directory (an `emptyDir`; tmpfs with `sizeLimit` bounds what a module may write).
- **Guest gets `EPERM` under `/tmp`**: its path left the private `/tmp`; there is no other directory to reach.
- **`cannot verify module oci …: the operator policy requires a cosign signature, but the runtime has no --cosign-key to verify it`**: add `--cosign-key`, or narrow the `requireSignature` rule. The runtime warns loudly at startup when `--cosign-key` is set but the policy requires nothing.
- **`operator policy …: dialAddress rule "…" must scope the action as == Action::"dialAddress"`** (or another Cedar/ip-rule load error): the `--sandbox-policy-file` is malformed and refused at load. A `dialAddress` condition accepts only `context.ip.isInRange(ip("CIDR"))`, `context.ip.isLoopback()`, or a `||` of them.
- **A guest's request fails with `sandbox.egress: <host> resolves to an address the egress policy blocks`**: the host refuses private, loopback, link-local and cluster ranges by default; the address and block-list entry stay operator-side in the audit line. Add the range to the policy's `allowedCIDRs` to permit an in-cluster service.
- **A guest's request fails with `wasmfn: sandbox.egress: HTTP egress is not granted to this module`**: the module calls the HTTP helper but its manifest requires no egress; the import always exists, the grant decides.
- **A guest's request fails with `sandbox.egress: the module's request rate exceeds the egress policy's rateLimit`**: raise `--egress-rate-limit-per-minute`/`-burst`, or reduce the module's request frequency.
- **`module imports wasmfn.http with the wrong type, ABI v1 requires (i32, i32) -> (i64)`**: a hand-written import declaration has the wrong signature.
- **Cannot create /tmp/function-wasm-cache at startup**: the pod's filesystem is read-only there — mount an emptyDir at that path through a `DeploymentRuntimeConfig`.
- **`module … failed: waiting for a run slot: deadline exceeded`** (or the step-slot / run-memory forms): the named bound is set and the request's deadline passed while waiting; nothing ran. Raise the bound, shorten runs, or read `function_wasm_module_runs_in_flight`.
- **`Cannot warm module` at startup**: a `--warm-modules` entry did not load — the log line carries the entry and the reason. The pod serves anyway; that module is loaded on its first request.
- **Readiness probe fails for a while after start**: the pod is warming `--warm-modules`; gRPC health and `/readyz` flip when warm-up ends.

## Key Reference Documents

- `README.md` — user-facing behaviour, the Input reference, runtime flags, trust model
- `docs/abi.md` — the host/guest contract
- `docs/one-pager-abi-v2.md` — ABI v2 on the component model and this Rust host (phase 1 delivered by the port)
- `docs/one-pager-three-layer-authz.md`, `docs/one-pager-trust-model.md`, `docs/one-pager-sandbox.md` — the authorization and sandbox model
- `docs/one-pager-cache.md`, `docs/one-pager-resource-governance.md`, `docs/one-pager-governance-perf.md` — caches, bounds, fairness
- `docs/one-pager-module-source-schema.md`, `docs/one-pager-module-manifest.md`, `docs/one-pager-manifest-less-sources.md` — the Input and the manifest
- `docs/one-pager-admission-tooling.md` — validate and inspection
- `docs/one-pager-language-support.md` — the guest language matrix
- `crates/function/src/input.rs` — authoritative Input schema
- `.claude/skills/cut-release/SKILL.md`, `.claude/skills/remediate-cves/SKILL.md` — releasing
