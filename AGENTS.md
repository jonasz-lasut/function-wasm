# Function-WASM Agent Guide

This document provides orientation for AI agents and developers working with the function-wasm codebase.

## What is Function-WASM?

### Purpose

function-wasm is a Crossplane composition function that runs a user-supplied WebAssembly module in a [wasmtime](https://wasmtime.dev) sandbox. A module implements the same contract as a native function, `RunFunction(RunFunctionRequest) → RunFunctionResponse`, so users write an ordinary function-sdk-go function (or a Rust/Zig/C guest over the vendored proto), compile it to `wasip1`, publish it (usually as an OCI artifact) and reference it from a Composition step's Input. One installed function serves any number of modules.

The implementation is a Rust workspace (it was ported from Go in 2026-08; the Go tree is gone, but its behaviour is the contract — see "Parity with the Go runtime" below). The repository ships three things:

| deliverable | where | what |
|---|---|---|
| the runtime (host) | `crates/function` (binary `function`) + `crates/engine` | the gRPC function: resolves the module named by the Input, compiles and caches it, runs it per request; `function validate` runs the same admission offline |
| the guest glue | vendored per guest (the Go scaffold writes `internal/wasmfn`; the example is `examples/pdb-addon/internal/wasmfn`) | linked into a user's `.wasm`: the ABI exports, request/response codec, a `logging.Logger` over the host, `GetConfig`, `HTTPClient`. Not a published module - each guest owns its copy, like Rust/Zig/C own theirs |
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
│       spendCredential for a named credential - default-deny) or refuses;     │
│       an unset field under module.allowEmpty ends the request here: the      │
│       caller's desired state and context echoed at the wire level            │
│       (protowire::noop_response), requests_total{outcome="skipped"}          │
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
│       wasmfn.yaml a path/http source names by reference, else none);         │
│       admission::admit_requires(requires, ceilings, compositionPolicy,       │
│       principal) - the three-layer AND: the manifest requests, the           │
│       composition layer permits (scoped default-permit), the operator layer  │
│       permits (default-deny) → Capabilities{private tmp, HTTP grant, env,    │
│       credentials}, or a fatal "module <desc> requires …, which the … does   │
│       not permit"; manifest.check (minRuntime vs the stamped version, config │
│       schema); sandboxenv::materialize(env bindings, step credentials) →     │
│       the run's env, sandboxenv::check_credentials(requires.credentials) -   │
│       the request must carry each, and neither may name the pull credential; │
│       protowire::retain_credentials → the forwarded request: the caller's    │
│       bytes less every step credential not granted (spendCredential)         │
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
│     bytes were forwarded verbatim too, with only the step credentials the    │
│     module was not granted - the pull credential always - edited out         │
│     (protowire.rs) - fields newer than the vendored proto survive in both    │
│     directions                                                               │
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
                              module-stack-limit, sandbox-policy-file, cosign-key,
                              egress-rate-limit-*, cache and concurrency bounds, max-cache-size,
                              warm-modules, ttl, health-address, metrics-address); opens the three
                              disk stores, compiles the operator
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
  src/protowire.rs            wire-level protobuf surgery: retain_credentials (drops every field-7 map
                              entry the module was not granted, failing closed), append_meta (field
                              1), noop_response (the skipped step's reply:
                              the request's desired and context re-tagged under a meta) - how the
                              transparent proxy edits raw bytes without decoding them
  src/validate.rs             function validate: multi-doc YAML/JSON (- stdin) → per step: strict
                              decode (unknown fields → warnings), admit, --xr → from_composite,
                              --resolve → resolve + verify + fetch + engine.inspect + admit_requires
                              and the credentials the module's request carries (credentials:);
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
  src/oci.rs                  the distribution client: manifest/blob GET, OCI 1.1 referrers (the API,
                              then the sha256-<hex> tag schema), raw_manifest, push_blob +
                              push_manifest (guestfn), anonymous/Basic/Bearer auth, the local Docker
                              config (keychain_auth); wasm_layer/manifest_layer/extract_wasm rules;
                              testregistry (feature "testutil") serves and accepts artifacts and
                              referrers in tests
  src/location.rs             go-containerregistry-compatible reference normalization: pinned refs
                              (the runtime), any refs with tag/digest (guestfn), http locations
  src/cosign.rs               cosign 3 Sigstore bundle verification, key-based (bundles found among
                              the manifest's referrers over the runtime's own registry client, DSSE
                              + in-toto parsed here, sigstore-rs crypto); legacy .sig not read,
                              keyless not yet (#116); testutil signs cosign-3-shaped bundles
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
  src/from.rs, input.rs,      module.from fencing, the Input types, egress rule, env binding and
  egress_rules.rs,            required-credential shapes, quantity/duration parsing, env
  sandboxenv.rs, quantity.rs  materialization and the required credentials held against the request
  src/ops.rs                  /livez + /readyz (warm-up gated) and /metrics on plain HTTP; warm()
  tests/conformance.rs        the golden conformance suite (see "Conformance goldens" below)
  tests/guests.rs             the guest behavioural suite over every scaffold (see "Testing")
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
  src/component.rs            the ABI v2 host: the wasmfn:function world bindgen, binary-format
                              detection, the world typecheck (v2's checkABI), the typed log import,
                              the async run path (driven on the tokio runtime), the v2 store data
                              (WASI 0.3+0.2 + the same limiter/deadline/call-hook as v1)
  src/componentize.rs         guestfn build's wrap: a core module carrying wit-bindgen's
                              component-type section (what its C generator links in) becomes a
                              component through wit-component, the wasip1 reactor adapter
                              (wasi-preview1-component-adapter-provider, published at wasmtime's
                              version) linked in only when it imports wasi_snapshot_preview1; it
                              lives here so the adapter is pinned beside the wasmtime-wasi that
                              serves it (carries_component_type is the cheap wasmparser detection)
  src/abi.rs, sandbox.rs,     checkABI (exports and both imports' exact types), the private /tmp and
  hostlog.rs, hosthttp.rs,    env wiring, the wasmfn.log and wasmfn.http host imports, the JSON
  wire.rs, duration.rs,       payload shapes, Go-style duration parse/format, per-digest step slots
  concurrency.rs, metrics.rs  and the Prometheus series (same names/labels/buckets as the Go runtime)
crates/guestfn/             the CLI crate (binary `guestfn`)
  src/main.rs                 clap CLI: init/build/push/inspect/manifest/scaffold; shared helpers
  src/scaffold.rs             template rendering ([[ ]] delimiters, zigid/zigfp helpers), write with
                              overwrite refusal; the golden and examples-share-the-scaffold-plumbing
                              tests
  src/buildcmd.rs             toolchain detection (Cargo.toml → rust - wasip3 with a wit/ dir,
                              wasip1 without; package.json sans asconfig.json → ts via npm - npm ci
                              with a package-lock.json, npm install without; requirements.txt →
                              python via componentize-py; build.zig → zig/c; go.mod → go), the
                              builds, the wrap (a built core module carrying wit-bindgen's
                              component-type section goes through the engine's componentize and
                              is written back as a component, one line saying so, adapter or
                              not; a component or a plain core module passes through; a failed
                              wrap is an error with wit-component's words), the ABI verdict,
                              wasmfn.yaml validation, the example-config warning
  src/push.rs                 the CNCF wasm OCI artifact (wasm layer, manifest layer, layerDigests
                              config, OCI annotations, SOURCE_DATE_EPOCH-reproducible), upload
  src/inspect.rs              file → engine.inspect; reference → manifest, layers, annotations, the
                              module-layer rule, the manifest summary; --pull; text/json
  src/manifestcmd.rs          manifest validate <file> / manifest show <ref>
  src/composition.rs          scaffold composition: the step, a config skeleton from the schema, the
                              commented compositionPolicy skeleton from the manifest's requires
  templates/<lang>            the seven template sets: minimal greeting projects, whose plumbing
                              (glue, codecs, proto, WIT) the paired examples carry byte for byte
  testdata/<lang>             the golden scaffolds (UPDATE_GOLDENS=1 cargo test regenerates)
examples/pdb-addon          #112's Go use case on the go scaffold's plumbing (examples-share-the-
                            scaffold-plumbing: internal/wasmfn, the vendored ABI glue, no external
                            SDK) - separate go.mod: a tenant add-on hook. A WebApp XR names the
                            module in spec.addOn, read by the Composition's last step through
                            module.from with allowEmpty (no add-on: the step is skipped) and fenced
                            by its compositionPolicy's pullModule; the module adds a
                            PodDisruptionBudget for every desired apps/v1 Deployment of 2+ replicas
                            that the function-go-templating step before it composed, in the
                            default sandbox (its manifest requires nothing). Its render runs that
                            step in Docker; its xprin suite renders with and without an add-on.
                            Its example/ is also the package's examples (publish-pkg.yml). ~75 MB
examples/hello-zig          the same guest, Zig + zig-protobuf — build.zig
examples/hello-c            the same guest, C + nanopb + cJSON, compiled by zig cc — build.zig
examples/cloudflare-origin  #112's Rust use case, an ABI v2 component on the rust scaffold's
                            plumbing (examples-share-the-scaffold-plumbing): an Origin XR's AWS
                            security group plus, for exposure: cloudflare, one ingress rule per
                            range Cloudflare publishes, fetched at reconcile time by awaiting
                            wasi:http/client through the host's egress (its manifest requires
                            GET api.cloudflare.com; a failed fetch is fatal, so existing rules
                            stay). async Rust 1.100+ (wasm32-wasip3 + wit-bindgen;
                            rust-toolchain.toml pins 1.100's beta until that release); its own
                            world (wit/world.wit, package local:guest) includes the contract
                            vendored byte-identical under wit/deps/. Its render runs under
                            example/policy.cedar against a fixture of the IP list
                            (example/fixtures, module.manifestPath naming the fixture host) and
                            its xprin suite validates the output against the vendored
                            provider-upjet-aws CRDs (example/crds). ~315 KB
examples/policy-gate        #112's TypeScript use case, an ABI v2 component on the ts scaffold's
                            plumbing (examples-share-the-scaffold-plumbing): the organisation's
                            policy as the last pipeline step, after function-go-templating
                            composes a Database XR's RDS Instance. It asks for the
                            EnvironmentConfig named after spec.environment through the
                            response's requirements (the wire-level proxy carries the round
                            trip), then adds the environment's mandatory tags without
                            overwriting the team's, refuses a disallowed region or instance
                            class with a fatal result (unit-tested only: a fatal fails the
                            render), and records warnings and counts in status.policy.
                            protobuf-es codec (js+dts, checked in), tsc --noEmit gate, esbuild
                            bundle, componentize-js built without wasi:http (-d http -d
                            fetch-event: it requires nothing); sync-lifted run (jco cannot
                            async-lift a custom world yet - the world accepts sync); root-world
                            imports arrive as default imports (import log from "log", kept
                            external in the bundle). Its render supplies the EnvironmentConfigs
                            from example/required-resources.yaml and its xprin suite validates
                            against the vendored provider-upjet-aws Instance CRD (example/crds).
                            ~14 MB (SpiderMonkey)
examples/team-tags          #112's Python use case, an ABI v2 component on the python scaffold's
                            plumbing (examples-share-the-scaffold-plumbing): after
                            function-go-templating composes a Bucket XR's S3 bucket, the module
                            reads the CMDB record of the team the XR's namespace names (GET
                            <cmdbUrl>/teams/<namespace>, Authorization: Bearer the key of that
                            name in the step credential cmdb, one shared Secret of every team's
                            token, read from the request; the namespace, never spec, so an XR
                            author cannot spend another team's token) and stamps
                            team/owner/cost-center tags on every composed managed resource,
                            keeping other tags, plus status.cmdb; a missing credential or key,
                            an unknown team or a failed request is fatal. The manifest requires
                            the credential (requires.credentials: [cmdb]), so the request
                            carries it only where both policy layers permit spendCredential
                            and no other step credential; the tokens' boundary from there is
                            egress: the manifest requires GET cmdb.example.org /teams/, and
                            the step's compositionPolicy fences the credential and the request
                            alike. componentize-py, sync-lifted
                            run, pure-Python protobuf runtime bundled, wasi:http@0.2 on
                            componentize-py's poll loop; requirements.txt is a template pin.
                            Its render passes example/function-credentials.yaml, runs under
                            example/policy.cedar against fixture records (example/fixtures,
                            module.manifestPath naming the fixture host) and its xprin suite
                            validates the output against the vendored provider-upjet-aws S3
                            Bucket CRD (example/crds). ~21 MB (CPython)
examples/dashboard-bundle   #112's C# use case (example only: no scaffold, guestfn build does not
                            detect it; make build), an ABI v2 component from componentize-dotnet
                            on NativeAOT-LLVM with wit-bindgen's generated bindings: a
                            DashboardBundle XR pins a zip of Grafana dashboards by sha256; the
                            module fetches it over wasi:http@0.2's outgoing-handler, refuses a
                            digest mismatch, extracts its .json dashboards with
                            ZipArchiveEntry.ExtractToFile into its private /tmp
                            (requires.filesystem.privateTmp - the only example that uses it),
                            skipping any other file with a Warning result, and composes one
                            sidecar-labelled ConfigMap per dashboard.
                            The csproj links NativeAOT-LLVM's wasi-wasm zlib and compression
                            shim (shipped, not linked by default); System.Formats.Tar and
                            System.Security.Cryptography are PlatformNotSupported on WASI, so the
                            pin is a managed SHA-256 (src/Sha256.cs). sync-lifted run (the C#
                            async bindings do not compile for this world yet); the .NET runtime
                            itself imports WASI 0.2.6. Google.Protobuf with protoc's codec checked
                            in, versions in Directory.Packages.props. No macOS NativeAOT-LLVM
                            compiler: make builds in the .NET SDK container there. Its render
                            runs under example/policy.cedar against fixture bundles
                            (example/fixtures, packed from example/bundles by make bundles). ~4.5 MB
test/e2e/                   end-to-end scenarios against real infrastructure (README.md: the tiers
                            and conventions): oci/ pushes examples/pdb-addon to a registry behind
                            basic auth, signs it with cosign and renders a WebApp that names it
                            through module.from (pull by digest with a step credential, the
                            pullModule fence, requireSignature); run.sh sets the stage, xprin and
                            function validate's JSON assert; the /e2e job e2e (oci) runs it
examples/render.sh          shared: cargo-build the runtime, function validate every example/xr*.yaml,
                            serve the example dir (example/policy.cedar as the operator policy,
                            example/fixtures/ on a local HTTP server for egress), then crossplane
                            render example/xr.yaml - or, with --check, run example/xprin.yaml
                            (xprin, crossplane-contrib's render test framework) against it
package/                    crossplane.yaml + the checked-in Input CRD (documentation for tooling;
                            Crossplane never installs a function's Input CRD - the CRD is maintained
                            by hand now that the Go types that generated it are gone)
docs/abi.md                 the language-agnostic host/guest contract
```

## Key Concepts

### Input

The function receives an `Input` (`wasm.fn.crossplane.io/v1beta1`) — a KRM-like object (`crates/function/src/input.rs`):

- `module` — `type: OCI|HTTP|Path` (required) + exactly one of `oci{ref, credentials}`, `http{url, digest, manifestURL, manifestDigest}`, `path` (+ `manifestPath`), or `from` (the XR field holding that object; with `allowEmpty: true` a field the XR leaves unset - absent or null - skips the step: no module runs and the desired state goes back unchanged; refused without `from`)
- `compositionPolicy` — raw Cedar, the composition author's layer: fences `from` sources (`pullModule`/`spendCredential`, default-deny) and may narrow sandbox capabilities (scoped default-permit); read from the Input only, never from the XR
- `limits` — `timeout`, `memory`, `concurrency`, each ≤ the runtime's ceiling flag (concurrency silently capped)
- `config` — opaque; the guest reads it via `GetConfig` - non-secret module configuration lives here

There is no `sandbox` field: what a module gets beyond the default sandbox is decided per capability by three AND-combined layers (`docs/one-pager-three-layer-authz.md`) - the module's manifest requests it (`requires.filesystem.privateTmp`, `requires.egress.http`, `requires.env` credential bindings, `requires.credentials` - the step credentials it reads whole from its request), the Input's `compositionPolicy` permits it, and the operator's Cedar `--sandbox-policy-file` permits it (default-deny: no policy file, no capability). The user-facing field reference lives in `README.md` ("Input reference"); keep it in sync with `input.rs` and the CRD under `package/input/`.

### ABI v1

Guest exports `memory`, `wasmfn_alloc(u32)->u32`, `wasmfn_run(u32,u32)->u64`, optional `_initialize`; host imports `wasmfn.log(u32,u32,u32)` with a JSON `{"msg","kv"}` payload and `wasmfn.http(u32,u32)->u64` with a JSON request answered through the guest's `wasmfn_alloc` **re-entrantly** and returned as `ptr<<32|len`; protobuf `RunFunctionRequest`/`RunFunctionResponse` on the wire. `docs/abi.md` is authoritative; `engine::abi::check_abi` enforces it at load, over wasmtime's decoded types — the one ABI check, whose verdict `engine.inspect` reports to `guestfn build`/`push`/`inspect` and `function validate --resolve`. Payload evolution is protobuf's (and, for imports, JSON's) job; a mechanics change is a new set of export names.

### ABI v2

The component-model contract, served by the same runtime (`docs/abi-v2.md`, design in `docs/one-pager-abi-v2.md`, tracking issue #65): a guest is a WebAssembly **component** targeting the WIT world `wasmfn:function@2.0.0-draft` (`wit/wasmfn-function.wit`) — export `run: async func(list<u8>) -> result<list<u8>, string>` (v1's protobuf payload), a typed `log` import, WASI 0.3+0.2 linked under the same sandbox. Detection is the binary format (the layer bytes): a component is v2, a core module is v1; the manifest's `abi:` is cross-checked against it. v2's checkABI is the world typecheck at load (`engine/src/component.rs`, the bindgen `FunctionPre`); a sync-lifted `run` satisfies the async world (what the WAT fixtures use). v2 egress is wasi:http - 0.3 `client.send` and 0.2 `outgoing-handler` both - bridged onto the same `HttpRequester` grant (`engine/src/wasihttp.rs`, the `WasiHttpHooks::send_request` seam, unified across the two generations, with wasmtime-wasi-http's default client compiled out); host-reported failures reach the guest as `internal-error` carrying v1's exact wire error string, and send waits are credited back to the compute deadline. One wasmtime engine serves both ABIs (async-ness is per-store), and both artifact kinds share `compiled/<version()>` (they self-describe). The v1 `i32::MAX` request cap does not apply to v2; a component reserves nothing from the memory pool up front (no top-level memory) — growth is charged incrementally. A guest `err(string)` from `run` becomes `module <desc> failed: run returned an error: <s>`. The example guests land with the rest of issue #65.

### The transparent proxy is wire-level

The host forwards the whole request and returns the whole response - requirements/extra-resource round trips work with no runtime knowledge. In Rust this needs care: prost drops protobuf fields it does not know, so the gRPC layer uses a raw pass-through codec (`grpc.rs`), the runtime decodes only a typed *copy* for admission, and the guest receives the caller's exact bytes with the step credentials it was not granted edited out at the wire level (`protowire.rs`: only those `credentials` map entries go, the pull credential always among them; a module receives only the credentials its admitted `requires.env` bindings and `requires.credentials` name). The guest's response bytes travel back untouched; `meta` is appended as raw bytes only when the guest omitted it. `tests/raw_client.rs` proves unknown fields survive the whole served stack, in place, while the credentials around them are withheld. Never route the forwarded payload through prost structs.

### Parity with the Go runtime

The Go implementation was the reference until 2026-08; the contract is **logical compatibility**: the same Inputs admitted, the same requests refused for the same reasons, nothing running wider than the Go runtime would allow. Most admission and policy refusal strings match Go verbatim (the conformance goldens hold them); wording-only divergences are accepted (alpha), the recorded ones being: guest log kv rendered as one JSON field, egress transport-error text (reqwest's words, no 64KiB response-header cap, no HTTP/2 attempt), limits parse-error wording, a cold module load (fetch + compile) not being cut short by the request's deadline - the compile completes and caches where Go bounded loads with a load timeout; the run that follows still gets only the remaining budget - and the run deadline metering guest compute: time blocked in `wasmfn.http` is credited back to the epoch deadline (the gRPC deadline stays the hard cap) where Go spent `limits.timeout` on it. One recorded divergence is more than wording - the signature format: the Go runtime read cosign 2's legacy `sha256-<hex>.sig` signatures, this runtime reads only cosign 3's key-based Sigstore bundles among the manifest's OCI 1.1 referrers (the same `--cosign-key` keys decide, in the format cosign 3 writes by default), so a module signed only the legacy way is refused as unsigned until re-signed with cosign 3. Another is step credential forwarding: the Go runtime forwarded a module every step credential but the pull credential, this runtime forwards only the ones it was granted - those its manifest's `requires.env` bindings and `requires.credentials` name, each permitted by `spendCredential` in both Cedar layers - so a module that read a credential it did not declare stops seeing it (and a required credential the step does not carry is refused, as an env binding's always was); `function validate --resolve` lists what each module receives. Anything the runtime does not carry is refused with a message naming it, never silently ignored.

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

`crates/engine/src/metrics.rs` registers the same series as the Go runtime — `function_wasm_module_{compile,fetch,run}_duration_seconds`, `runs_in_flight`, `cache_events_total`, `cache_bytes`, `http_requests_total{outcome}`, `requests_total{outcome}` (its `skipped` outcome, a step with no module to run under `module.allowEmpty`, is this runtime's addition) — plus two additive series the Go runtime did not carry: `hostcall_duration_seconds` (the host-import slice of a run, split by call_hook) and `memory_denials_total{reason}` (guest memory growths denied at the per-run ceiling or the pool) — registered with prometheus-client (the OpenMetrics-native client; the runtime's own registry, prometheus-client has no default), served at `/metrics` on `--metrics-address` (default `:8080`, function-sdk-go's port) - OpenMetrics 1.0 as the main format straight from prometheus-client's encoder, the classic text format derived from it (`metrics::classic_from_openmetrics`: counter families renamed with `_total`, `# EOF` dropped) only for an Accept header that asks for `text/plain` without accepting OpenMetrics (`ops.rs::wants_classic_text`); both renderings carry identical series. The `Labeled{Counter,Gauge,Histogram}` adapters in metrics.rs keep the prometheus crate's `with_label_values` call shape, so metric call sites never name prometheus-client; counters register named without their `_total` (the encoder appends it) and helps without their trailing period (prometheus-client appends one). `crates/function/src/grpcmetrics.rs` adds the Go runtime's gRPC server series (`grpc_server_{started,handled,msg_received,msg_sent}_total`, same names/labels/help strings as function-sdk-go's grpc-prometheus interceptor) as a tower layer around the whole router in grpc.rs — unary-only like Go's interceptor, every served method pre-created at zero like its InitializeMetrics, no handling-time histogram (Go never enabled it). Never add a module/digest/host label - unbounded cardinality. `metrics::sample` reads one series back for tests.

### Signatures

`--cosign-key` loads PEM public keys into `cosign::Verifier` (sigstore-rs crypto, key-based only); verification runs **before** the caches, once per manifest digest per process, over the runtime's own registry client (same auth path as the pull); non-OCI sources are refused when a signature is required. The format is cosign 3's: a Sigstore bundle (v0.3) in an OCI 1.1 referrer of the module's manifest, discovered through the referrers API or, on a 404 (GHCR, `registry:3`), the `sha256-<hex>` referrers tag index; bundles are recognised by the layer media type, never by `artifactType` (cosign 3.0 sets none). A referrer counts only if its `subject` is the pinned digest; the bundle is fetched bounded and verified against its digest; a DSSE envelope signature over the PAE must verify with a configured key, and only then is the in-toto statement read: `Statement/v1`, `predicateType` exactly `https://sigstore.dev/cosign/sign/v1` (SLSA provenance and other attestations never count, whoever signed them) and a subject with the manifest digest. The keys are the trust root - the bundle's verification material (key hint, certificate, tlog entries) is ignored. Refusal reasons are sorted and deduplicated so the wording is deterministic whatever order a registry lists referrers in. cosign 2's legacy `.sig` signatures are not read (no lookup, no hint). Keyless (Fulcio/Rekor) is not implemented yet (#116, waiting on sigstore-rs): a keyless bundle matches no key and never fails the check for a key bundle beside it. `cosign::testutil` (feature `testutil`) signs cosign-3-shaped bundles and attaches them to `oci::testregistry`.

### One-pagers

Design documents under `docs/one-pager-*.md` follow one pattern: the H1 is the feature name, then `* Owner / * Reviewers / * Status: Draft | Implemented, revision x.y`, then the body. Bump the revision when the design changes. They describe the design in the Go era's file layout; the design decisions hold, the paths map to `crates/` as described above.

## Development Guide

### Building

```bash
cargo build --workspace                # engine, runtime, guestfn
cargo run -p function-wasm -- --insecure --module-dir=examples/pdb-addon
cargo run -p guestfn -- inspect examples/pdb-addon/fn.wasm

# The example guest (with its vendored internal/wasmfn glue) must also build for wasm
(cd examples/pdb-addon && GOOS=wasip1 GOARCH=wasm go build -buildmode=c-shared -o /dev/null .)

# The runtime image (multi-arch; FUNCTION_WASM_VERSION stamps a release)
docker buildx build --platform linux/amd64,linux/arm64 --target image .

# A Crossplane package
crossplane xpkg build -f package --embed-runtime-image=runtime
```

### Testing

```bash
cargo test --workspace                    # everything below
cargo test -p function-wasm-engine        # the engine over WAT fixtures
cargo test -p function-wasm               # runtime units, conformance goldens, guest suite, raw client
cargo test -p guestfn                     # scaffold goldens, the examples' shared plumbing, CLI against an in-memory registry
(cd examples/pdb-addon && go test -race ./...)
(cd examples/hello-zig && zig build test)
(cd examples/hello-c && zig build test)
make -C examples/dashboard-bundle test    # dotnet test (in the .NET SDK container off Linux)
```

`crates/function/tests/guests.rs` builds every language's scaffold - its golden under `crates/guestfn/testdata`, copied fresh into the OS temp dir (outside the workspace), so the suite proves what `guestfn init` writes and runs each through the whole host with the same expectations: default and configured greeting, a greeting fetched through the host's egress (via an OCI manifest layer and via `module.manifestPath`) and refused without a grant, guest-side fatal on a bad config, guest logs. The guests must stay behaviourally identical; the examples under `examples/` are free to solve their own use cases and are checked by their render jobs - C#, whose only guest is the dashboard-bundle example (it does not greet), has no place in the suite and is covered by that example's unit tests and render job. A scaffold has no lockfile, so the suite resolves dependencies the way a user's first build does (`go mod tidy`, `npm install`, a fresh `Cargo.lock`). A guest whose toolchain is not on PATH is skipped. Toolchains: the rust scaffold's `rust-toolchain.toml` brings its own pinned toolchain with `wasm32-wasip3`, installed by rustup on first use; Zig 0.16; `protoc` for the rust scaffold's prost-build and the codec regeneration targets; `nanopb_generator` (`pip install nanopb==0.4.9.1`) only for `zig build gen-proto` in hello-c.

Goldens: `UPDATE_CONFORMANCE=1 cargo test -p function-wasm --test conformance` re-records the conformance goldens (deliberate behaviour changes only); `UPDATE_GOLDENS=1 cargo test -p guestfn` regenerates the scaffold goldens after a template change.

CI runs lint and the toolchain-free workspace tests on every push and PR (`ci.yml`); the render jobs and the full guest behavioural suite live in `e2e.yml` (its `build-tools` job compiles the release runtime and `guestfn` once per run, fetches xprin pinned by checksum, and hands all three to every render job as an artifact, which `examples/render.sh` and pdb-addon's Makefile pick up from `FUNCTION_BIN`, `GUESTFN` and `XPRIN`; without them they build from the tree and take xprin from PATH), hand-triggered by commenting `/e2e` on a pull request - the run acknowledges the comment with a reaction and reports one `e2e` commit status on the PR's head. The per-guest codec drift checks ride with the render jobs, so they too run on `/e2e`, not on every push. `e2e.yml`'s `e2e (oci)` job runs `test/e2e/oci/run.sh` (`test/e2e/README.md`: unit and integration tests use in-memory stand-ins for registries and signatures; a scenario there is where the real thing must agree with them).

### Test Patterns

Unit tests live in `#[cfg(test)] mod tests` blocks beside the code; integration suites under `tests/`. Guest modules for tests are WAT fixtures assembled with the `wat` crate implementing ABI v1 (the engine's tests carry fixtures that misbehave in one way each; the sandbox is tested through raw WASI: a path escape is `module exited with status 63` (EPERM), a missing file 44 (ENOENT), no pre-open 8 (EBADF)). Registry-backed tests use `oci::testregistry` (feature `testutil`): an in-memory distribution registry that serves and accepts artifacts, optionally behind the Bearer token flow, with OCI 1.1 referrers through the referrers API (`referrers_api: true`) or the tag schema (`push_referrer` maintains the `sha256-<hex>` index); `cosign::testutil` signs cosign-3-shaped Sigstore bundles into it. Expected `RunFunctionResponse`s are constructed whole and compared with `assert_eq!` on the prost types, including fatal cases, whose per-guest message wording is blanked before the comparison.

### Linting

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
(cd examples/pdb-addon && golangci-lint run ./...)  # the Go guests keep Go lint
(cd examples/hello-zig && zig fmt --check build.zig src/main.zig)
(cd examples/hello-c && zig fmt --check build.zig)
make -C examples/dashboard-bundle lint              # dotnet format (in the .NET SDK container off Linux)
```

### Coding Conventions

- Rust edition 2024; comments explain *why*, as the existing ones do; prefer self-documenting code.
- English only; inclusive terminology (allowlist/blocklist, primary/replica, main branch).
- Conventional commits: `<type>(<scope>): <subject>`, imperative, ≤ 50 chars, one logical change per commit.
- Only `crates/engine` imports `wasmtime`/`wasmtime-wasi` — the engine is the seam if the runtime ever changes.
- Only `crates/function/src/authz.rs` imports `cedar-policy`.
- Errors that reach users are `String`s carrying the runtime's exact refusal wording — the wording is contract (conformance goldens); route new refusals through the same phrasing patterns.
- The `internal/wasmfn` glue must stay buildable natively (portable `Register`/`NewLogger`/`GetConfig`/`HTTPClient`) so a guest's own tests run natively; only the exports and the two host imports are `//go:build wasip1`. Edit it in `examples/pdb-addon/internal/wasmfn`, then mirror to `crates/guestfn/templates/go/internal/wasmfn/*.go.tmpl`; the examples-share-the-scaffold-plumbing test keeps them in step.

## Common Development Tasks

### Adding an Input Field

1. Add the field to `crates/function/src/input.rs` (serde) and enforce its rules at runtime — `admission::admit`, the resolver, or `from.rs` for something the XR may choose (never read `compositionPolicy` or `limits` from the composite). A new *capability* is not an Input field at all: it is a manifest requirement (`manifest.rs` `Requires` + its shape check) decided by `admission::admit_requires` under both Cedar layers (`authz.rs`: a new action in the shared schema, permits on both policies) → engine `RunOptions` + the sandbox wiring
2. Everything `admit`/`admit_requires` checks is what `function validate` checks — add a fixture under `testdata/validate/` and a conformance golden for the new refusal; a new ceiling flag goes into `main.rs` so `serve` and `validate` share it
3. Update the hand-maintained CRD under `package/input/` and the README's "Input reference" table

### Adding a host import (ABI)

`wasmfn.http` (`crates/engine/src/hosthttp.rs`, the guest side in `examples/pdb-addon/internal/wasmfn/http*.go` and the matching template) is the worked example:

1. Define it in the engine (`linker.func_wrap(HOST_MODULE, name, fn)`), allow it in `check_abi` with its exact type; reach per-run state through the store's `CallState` and pass what it needs in `RunOptions`. To hand bytes back, call the guest's `wasmfn_alloc` re-entrantly — copy the request out first and re-read memory afterwards, the guest may grow it — and return `ptr<<32|len`; keep failures inside the payload (never a trap for a refusal)
2. Add the guest side to the glue (a `//go:wasmimport wasmfn <name>` in a `_wasip1.go` file, a portable fallback behind a swappable package var so the codec is testable natively)
3. Document it in `docs/abi.md`; cover it with a WAT fixture in the engine's tests and, through the guest suite, in every scaffold

### Changing the scaffold

Edit the template set under `crates/guestfn/templates/<lang>` (templates use `[[ ]]` delimiters so source braces survive; each template set is a minimal greeting project; `wasmfn.yaml.tmpl` is the manifest every flavour ships; the zig and c `build.zig.zon.tmpl` take the project's identifier and fingerprint from the `zigid`/`zigfp` template helpers), then `UPDATE_GOLDENS=1 cargo test -p guestfn` to refresh the goldens; the guest suite builds the goldens, so `/e2e` proves the change. A change to the plumbing - the vendored ABI glue (`internal/wasmfn`), the generated codecs, the proto, the vendored WIT - goes into the paired example too: the examples-share-the-scaffold-plumbing test holds each example's copy of those files identical to its template (its list of shared paths per language is in `scaffold.rs`); everything else in an example is its own. The sixteen vendored `run_function.proto` copies (five templates, five goldens, six examples) are one wire contract: `crossplane/crossplane`'s `proto/fn/v1/run_function.proto`, stated in each file's header as `Vendored from crossplane/crossplane vX.Y.Z.` and tracked by Renovate through that header - the same tripwire function-sdk-rust carries. A Renovate bump moves the version in every copy at once but **does not re-download the file** or regenerate the codecs checked in beside it: treat the PR as the signal to run `make vendor-proto` on its branch (`VERSION=vX.Y.Z` vendors ahead of Renovate), which fetches the upstream file at that release into every copy under its own header, regenerates every guest's codec with the protoc and nanopb_generator the root `Makefile` pins (it needs Go, Zig, npm, python3 and cargo), mirrors them into the templates and refreshes the goldens; then comment /e2e, whose render jobs fail on codec drift. Per-language identity of the plumbing (template ↔ golden ↔ example) is enforced by the goldens and the examples-share-the-scaffold-plumbing test. The rust scaffold's `wit/deps/wasmfn-function.wit` is the root `wit/wasmfn-function.wit` (the world the engine compiles) byte for byte, enforced by the render-matches-the-runtime-world test: change the world at the root, copy it into `templates/rust/wit/deps/` and `examples/cloudflare-origin/wit/deps/` (and move the version in `wit/world.wit`'s `include` if the package version changed), then refresh the goldens; the guest's own world, `wit/world.wit`, is a `local:guest` package, so a scaffold never redefines the contract's package (the ts and python scaffolds' `wit/world.wit` restates the world with a sync `run` instead of including it, so only the contract's copies carry its name). The vendored WASI WIT under `wit/deps/` (wasi:*@0.3 from wasmtime-wasi-http's `src/p3/wit/deps` in the rust template, its golden and cloudflare-origin, the p3 `http.wit` trimmed of its service/middleware worlds; wasi:*@0.2 from its `wit/deps` in the python template, its golden, team-tags and dashboard-bundle) carries a `Vendored from wasmtime-wasi-http X.Y.Z` header under the same tripwire, grouped into Renovate's wasmtime PR: that PR moves the headers with the crates, and is the signal to re-vendor the files by hand from the new crate (keeping the trim).

### Rendering Locally

Each example's `example/xprin.yaml` is its render test: [xprin](https://github.com/crossplane-contrib/xprin) cases (an XR plus any required resources, observed resources or function credentials) with declarative assertions on what the module composes - `Count`, `Exists`/`NotExists`, `FieldValue` - never string matching over the render output. A suite asserts outcomes as composed resources or XR status, never function results (they carry no name, so xprin collapses them onto one `Result/` key); a fatal path is tested in the guest's own unit tests, because a fatal result fails the render. `render.sh` owns the runtime's side: `function validate` over every `example/xr*.yaml`, the operator policy and the fixture server.

```bash
make -C examples/pdb-addon render         # build fn.wasm (guestfn via cargo), serve it, crossplane render example/
make -C examples/pdb-addon render-check   # same, running example/xprin.yaml (xprin on PATH, or XPRIN) - what the /e2e render job runs
make -C examples/hello-zig render         # the Zig guest (zig on PATH)
make -C examples/hello-c render           # the C guest (zig on PATH: zig cc builds it)
```

By hand: `cargo run -p function-wasm -- --insecure --debug --module-dir=examples/pdb-addon`, then in the example: `cargo run -p guestfn -- build` and `crossplane render example/xr.yaml example/composition.yaml example/functions.yaml --include-function-results`. `functions.yaml` uses the Development runtime, so the function must be running locally; the render engine itself runs in Docker.

## Key Dependencies

- `wasmtime` / `wasmtime-wasi` — the sandbox (Cranelift, pure Rust). Each major may change APIs; only `crates/engine` touches them, and `engine::version()` re-namespaces the compiled cache automatically on a bump.
- `wit-component` / `wasi-preview1-component-adapter-provider` — `guestfn build`'s componentizing (`engine::componentize`): the wrap of a core module carrying wit-bindgen's component-type section, and the wasip1 reactor adapter it links in. Pinned in `crates/engine` beside wasmtime: the adapter is published at wasmtime's version and must be the one the engine's wasmtime-wasi was released with, and wit-component (with wasmparser) is the version wasmtime's own bindgen already pulls, so no second copy is compiled; Renovate's wasmtime group moves them together.
- `function-sdk-rust` — the gRPC/protobuf types (prost), `request`/`response`/`resource` helpers and the CLI `Args`; the generated FunctionRunnerService *client* types serve tests, while serving goes through the raw codec.
- `cedar-policy` — both policy layers.
- `sigstore` (features `cosign`, `rustls-tls`, no default features): cosign key verification crypto only; fetching stays on the runtime's own registry client and the bundle, DSSE and in-toto shapes are parsed by `cosign.rs`'s own serde structs (no `bundle`/`verify` features).
- `reqwest` (blocking, rustls) — the egress client and the registry client.
- `prometheus-client` — the metrics registry and OpenMetrics encoder (the official OpenMetrics-native client; the classic text format is derived from its output in `metrics.rs`).
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
- **A module sees only the step credentials it was granted** (issue #122, 2026-10): the forwarded request carries the credential each admitted `requires.env` binding reads and each `requires.credentials` entry, every one permitted by `spendCredential` in both Cedar layers; every other credentials map entry is edited out at the wire level, failing closed, and the pull credential is never forwarded (a manifest naming it is refused). Forwarding everything, as Crossplane does to a native function, made `spendCredential` gate a convenience - a module denied an env binding still read the key from its request. A required credential the step does not carry is a fatal result, as an env binding's is, so a module that cannot run as declared fails before it runs.
- **Guest error → fatal result** instead of a gRPC error: crossplane treats both as a failed step, fatal results are visible in `crossplane render --include-function-results`, and the wire stays one message.
- **Memory-export ABI, not stdin/stdout**; **fresh instance per request** (hermetic, no reentrancy; the expensive compile is cached by content digest in memory and as a wasmtime artifact on disk); **digests are stated, not discovered** (OCI refs `@sha256:`-pinned, `http.digest` required, no tags alone, no request-time resolution).
- **Disk caches are bounded by LRU sweep, not per-entry policy**: `--max-cache-size` (off by default) removes least recently used entries across the stores at startup and every ten minutes; entries are immutable and reproducible, so removal is always safe.
- **No shared guest SDK module; every guest owns its glue** (Jonasz, 2026-08-19): the Go scaffold vendors package `wasmfn` into each project under `internal/wasmfn`; Rust/Zig/C carry the vendored proto and their own glue. No published module, no version pin, no lockstep tags. A glue fix reaches existing guests only by re-scaffolding or copying - fine for stable ABI-v1 plumbing.
- **C guests build with `zig cc`, not wasi-sdk, and talk protobuf through nanopb with `fallback_type:FT_POINTER`** (Jonasz, 2026-08-19); **TinyGo guests used vtprotobuf** (protobuf-go's codec panics under TinyGo; the tinygo scaffold retired 2026-10-07, #114); **WASI argv is always `["function"]`** (an empty argv traps at `_initialize` because klog's init indexes `os.Args[0]`).
- **Warm-up runs while the server listens, health NOT_SERVING until it is done**: a closed port for minutes of compiling would fail a liveness probe and tell a probe nothing; an early request is simply cold. Failures never hold readiness back.
- **The run bound is the engine's, taken after the load and outside the run metric**; fair round-robin per module digest so one hot module cannot take every slot; a request cut short while waiting never ran and is not counted.
- **`function validate` lives in the runtime binary**: the checks are the operator's — the same flags, env, policy file and version as the pod (`docker run <package image> validate …` works: the package image is the runtime image).
- **No local-loop machinery** (Jonasz, 2026-08-17): the loop is `guestfn build` + `crossplane render` against `cargo run -p function-wasm -- --insecure --module-dir=.` (`examples/render.sh`, `make render`) and `function validate`. Guest stderr stays the pod's.
- Historical (Go era, superseded by the port but kept for context): wasmtime-go over wazero; not Extism; the Chainguard `glibc-dynamic` base over distroless (still the runtime image base - it scanned clean); the Input's `policy`/`sandbox` fields deleted for the three-layer model; sandbox types before behaviour.

## Releasing

Releases are driven by two skills; use them rather than improvising the branch/tag/publish sequence:

- **`/cut-release`** — a new minor or major version from `main` HEAD: new `release-X.Y` branch, tag (`.github/workflows/tag.yml`), GitHub release, package publish (`publish-pkg.yml` → `ghcr.io/jonasz-lasut/function-wasm`, mirrored to `xpkg.upbound.io/jonasz-lasut/function-wasm`; the version is stamped into the binary as `FUNCTION_WASM_VERSION` — what module manifests' `minRuntime` rules are checked against), signing/attestation (`supplychain.yml`). The bump size is the user's choice, never inferred.
- **`/remediate-cves`** — a patch release on the current `release-X.Y` branch for CVEs found by `grype-scan.yml` (weekly against the latest release). A wasmtime crate bump is in scope there (it is the sandbox's own security fix) — `crates/engine` only.

`publish-pkg.yml` also runs `publish-wit`: it publishes the ABI v2 world (`wit/wasmfn-function.wit`, built with a pinned `wkg`) to `ghcr.io/jonasz-lasut/wasmfn/function:<world version>`, signs and attests it, and attaches `wasmfn-wit-<world version>.tar.gz` to the release - but only for a final world version not yet in the registry, so it does nothing while the world is `2.0.0-draft` and never overwrites a published tag (#106). It also runs `publish-examples` (guestfn built once by `build-guestfn`): the use-case examples that work as published (`cloudflare-origin`, `policy-gate`, `pdb-addon`) are built and pushed with their manifests to `ghcr.io/jonasz-lasut/wasmfn/examples/<name>:v<version>` (the version in each `wasmfn.yaml`, independent of the runtime's), signed keyless and attested - only a version not yet in the registry, so bump an example's `wasmfn.yaml` version to publish a change - and `examples-release-asset` attaches `function-wasm-examples-<release>.yaml` with every published example's digest-pinned reference (`docs/one-pager-use-case-examples.md`). `SECURITY.md` is the dependency CVE policy these follow: the runtime is patched through `/remediate-cves`, the scaffold templates' pins are refreshed by `/cut-release` at every release, and the examples are refreshed by Renovate's monthly examples batch (`.github/renovate.json5`), gated by `/e2e`.

## Troubleshooting

- **Fatal `_initialize failed: trap` from a Go guest**: the guest panicked during package init; its stack is in the function pod's stderr.
- **`module imports X.Y, which the host does not provide`**: the module needs an import outside `wasi_snapshot_preview1`, `wasmfn.log` and `wasmfn.http`; it was built for another host or uses sockets/threads.
- **`does not export "wasmfn_run"`**: not built as a reactor with the exports — for Go, `-buildmode=c-shared` and `wasmfn.Register` in an `init`.
- **First request slow, then fast**: expected — compile is per digest; the artifact under `/tmp/function-wasm-cache/compiled` makes the next process fast too if that path is on a volume.
- **`module.oci.ref … tags are not supported`**: pin the reference to the manifest digest — `repo@sha256:…` or `repo:tag@sha256:…`, as `guestfn push` prints it.
- **`module layer is a tar archive without /fn.wasm`**: a `FROM scratch` image must `COPY` the module to `/fn.wasm` exactly. Prefer `guestfn push` / `oras push` (a raw `application/wasm` layer).
- **`guestfn build` says `built fn.wasm, but the runtime would refuse it: …`** (or `guestfn push` refuses): the module lacks the ABI; the message is the runtime's own load-time refusal. `guestfn inspect fn.wasm` lists what the module exports and imports.
- **`module oci … requires egress GET to host "x" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit`** (or `… which the compositionPolicy does not permit`; the same pair for the private /tmp and env forms; or `requires runtime vX or newer, this is vY`): the module's manifest declares a need the named policy layer does not permit - add a `permit` to that layer or use a module that needs less.
- **`module oci … requires credential "cmdb" (requires.credentials[0]), which the operator policy (--sandbox-policy-file) does not permit`** (or `… which the compositionPolicy does not permit`, or `… but the runtime has no --sandbox-policy-file, which is required to grant step credentials (spendCredential)`): the module reads that step credential from its request, and the named layer has no `spendCredential` permit for `Credential::"cmdb"` - add one, or use a module that needs less.
- **A module stops seeing a step credential it used to read** (a Go-era module, or one whose manifest does not declare it): the runtime forwards only the credentials a module was granted. Declare it in the manifest - `requires.credentials: [name]` for the whole credential, or a `requires.env` binding for one key - and permit `spendCredential` for it in the operator policy (and in a `compositionPolicy` that scopes the action). `function validate --resolve` lists what each step's module receives; the runtime logs the withheld names at debug level.
- **`module … requires.credentials[0]: the request carries no credential "cmdb"; declare it on the pipeline step`**: the module requires a credential the step does not pass - add it to the step's `credentials`.
- **`module oci … config does not match the module's schema: /greeting: got number, want string`**: the Input's `config` fails the module's `config.schema`.
- **`function validate` exits 1**: at least one step is refused - the line names the runtime's reason; exit 2 is the tool's own failure. Run it with the flags the runtime is started with.
- **`module.path is refused`**: the runtime was started without `--module-dir`.
- **`module.from: … names a OCI source, but the Input has no compositionPolicy`**: a `module.from` OCI/HTTP source requires a `compositionPolicy` whose `pullModule` permits its repository; add the policy, or name the source statically.
- **`module.from: cannot read status.module from the composite resource: module: no such field`**: the XR has not set the field the step reads. Set it, or add `module.allowEmpty: true` to let the step run nothing until it is set (the pod then logs `No module chosen by the composite resource` and counts `requests_total{outcome="skipped"}`).
- **`module.allowEmpty is set but module.from is not`**: `allowEmpty` only makes sense for a field the composite resource may leave unset; a static source always resolves. Remove it, or switch the step to `module.from`.
- **`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`** (or `limits.timeout … --module-timeout`): lower the limit or raise the flag.
- **`module … requires a private /tmp (requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities`** (and the env/egress forms): mount a Cedar `--sandbox-policy-file` with a matching permit or use a module that requires nothing.
- **`the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot create one under …`** at startup: point `TMPDIR` at a writable directory (an `emptyDir`; tmpfs with `sizeLimit` bounds what a module may write).
- **Guest gets `EPERM` under `/tmp`**: its path left the private `/tmp`; there is no other directory to reach.
- **`cannot verify module oci …: the operator policy requires a cosign signature, but the runtime has no --cosign-key to verify it`**: add `--cosign-key`, or narrow the `requireSignature` rule. The runtime warns loudly at startup when `--cosign-key` is set but the policy requires nothing.
- **`cannot verify module oci …: <ref> carries no cosign signature (no Sigstore bundle among its referrers)`**: nothing among the manifest's OCI 1.1 referrers (the referrers API, or the `sha256-<hex>` tag index where the registry has no API) carries a Sigstore bundle naming this digest. Sign it with cosign 3 (`cosign sign --key cosign.key <repo>@sha256:…`). A module signed only with cosign 2 or `--new-bundle-format=false` (a `sha256-<hex>.sig` tag) lands here too: the legacy format is not read - re-sign it.
- **`cannot verify module oci …: no valid cosign signature for <ref>: signature does not verify with the configured keys`**: a bundle is there but no `--cosign-key` key signed it - a different key, or a keyless signature (not verified yet, #116; countersign it with your key). Other reasons in the same list: `signed statement's predicate type is "https://slsa.dev/provenance/v1", not https://sigstore.dev/cosign/sign/v1` (an attestation, not a signature), `signed statement is for another digest` (a bundle copied from another manifest), and fetch, size or digest failures of a bundle.
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
- `docs/abi-v2.md` — the ABI v2 (component) host/guest contract
- `docs/one-pager-abi-v2.md` — ABI v2 on the component model and this Rust host (the port delivered its phases 1-4; the v2 spike is issue #65)
- `docs/one-pager-three-layer-authz.md`, `docs/one-pager-trust-model.md`, `docs/one-pager-sandbox.md` — the authorization and sandbox model
- `docs/one-pager-cache.md`, `docs/one-pager-resource-governance.md`, `docs/one-pager-governance-perf.md` — caches, bounds, fairness
- `docs/one-pager-module-source-schema.md`, `docs/one-pager-module-manifest.md`, `docs/one-pager-manifest-less-sources.md` — the Input and the manifest
- `docs/one-pager-admission-tooling.md` — validate and inspection
- `docs/one-pager-language-support.md` — the guest language matrix
- `docs/one-pager-use-case-examples.md` - the use-case examples: why these, how they are tested (scaffold goldens, shared plumbing, xprin suites), how they are published
- `crates/function/src/input.rs` — authoritative Input schema
- `.claude/skills/cut-release/SKILL.md`, `.claude/skills/remediate-cves/SKILL.md` — releasing
