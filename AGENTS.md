# Function-WASM Agent Guide

This document provides orientation for AI agents and developers working with the function-wasm codebase.

## What is Function-WASM?

### Purpose

function-wasm is a Crossplane composition function that runs a user-supplied WebAssembly module in a [wasmtime](https://wasmtime.dev) sandbox. A module implements the same contract as a native Go function — `RunFunction(*fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error)` — so users write an ordinary function-sdk-go function, compile it to `wasip1`, publish it (usually as an OCI artifact) and reference it from a Composition step's Input. One installed function serves any number of modules.

The repository ships three things:

| deliverable | where | what |
|---|---|---|
| the runtime (host) | `cmd/function` + `internal/` (root module) | the gRPC function: resolves the module named by the Input, compiles and caches it, runs it per request |
| the guest SDK | `pkg/wasmfn/` (nested module `github.com/jonasz-lasut/function-wasm/pkg/wasmfn`) | linked into a user's `.wasm`: the ABI exports, request/response codec, a `logging.Logger` over the host, `GetConfig` |
| the CLI | `cmd/guestfn` (root module) | `guestfn init` scaffolds a guest project (with its `wasmfn.yaml` manifest), `guestfn build` compiles it (and checks its ABI with the runtime's engine, and its manifest), `guestfn inspect` shows what the runtime sees in a module or an artifact, `guestfn push` publishes module and manifest (refusing a module the runtime would refuse), `guestfn manifest validate|show`, `guestfn scaffold composition` writes a Composition step from a manifest — CGo, like the runtime |

## Architecture Overview

### Request Processing Flow

```
Crossplane RunFunctionRequest
    ↓
┌──────────────────────────────────────────────────────────────────────────────┐
│ cmd/function/fn.go: RunFunction()                                            │
│  1. request.GetInput → v1beta1.Input                       fatal on error    │
│       admission.Admit(input, ceilings) — internal/admission, shared with     │
│       function validate: sandbox.Validate (shape); Ceiling.Grant → private  │
│       /tmp, env within the --enable-sandbox-* flags, or a fatal naming the   │
│       grant and the flag; sandbox.egress → egress.Grant (rules ∩            │
│       --sandbox-egress-policy) or a fatal naming the rule / the flag;        │
│       limits → engine.RunOptions, or a fatal naming the limit and the        │
│       ceiling flag; module.Validate + ValidatePolicy (shape)                 │
│  2. module.FromComposite(input.module, input.policy, observed XR)            │
│       type + from → the XR field decoded into the type's object → concrete;  │
│       policy (repositoryAllowList, credentialsAllowList) admits or refuses  │
│  3. registryAuth(req, module) → authn.Authenticator      (step credential)   │
│  4. resolver.Resolve(module) → module.Ref{Digest, Fetch}   internal/module   │
│       no I/O: oci manifest digest from the ref, http.digest from the Input,  │
│       path hashed by size+mtime; then ref.Verify — the cosign check, before  │
│       any cache is consulted, once per digest per process                    │
│  5. modules.Get(digest, fetch)                             engine.Cache      │
│       memory (idle TTL) → compiled artifact on disk → fetch (oci: manifest   │
│       GET → layer digest; blob store on disk → source, verified against the  │
│       blob digest; a tar layer yields /fn.wasm exactly) + Compile (checkABI  │
│       after wasmtime decodes it — the one ABI check) + Serialize to disk     │
│       then checkManifest: the artifact's manifest layer (Ref.Manifest, via   │
│       the manifests/<digest> store; none for path/http) held against the     │
│       admitted grants and the Input's config — narrowing only, a miss is a   │
│       fatal "module <desc> requires …" / "config does not match …"          │
│  6. engine.Run(ctx, module, req, log, limits)              internal/engine   │
│       run slot first when --max-concurrent-runs bounds them (waits under    │
│       ctx; deadline while waiting → fatal "waiting for a run slot: …");     │
│       fresh Store: WASI argv=["function"], no net; fs and env only as        │
│       granted (read-only pre-opens, a private /tmp made before and removed   │
│       after the store, SetEnv), epoch deadline,                              │
│       memory limiter (the Config's ceilings narrowed by RunOptions);         │
│       _initialize → wasmfn_alloc → copy req → wasmfn_run                     │
│       → packed ptr/len → proto.Unmarshal(RunFunctionResponse)                │
│       host import wasmfn.log → the request logger                            │
│       host import wasmfn.http → RunOptions.HTTP (egress.Client: grant,      │
│       block list, budgets, audit line) or a refusal; answer written via the │
│       guest's wasmfn_alloc                                                   │
│  7. return the guest's response verbatim (meta filled if the guest omitted it)│
│     trap / timeout / OOM / bad ABI / fetch / compile → response.Fatal        │
└──────────────────────────────────────────────────────────────────────────────┘
    ↓
Crossplane RunFunctionResponse (whatever the module produced)
```

Inside the module (a Go guest built with `wasmfn`): `wasmfn_run` looks up the buffer `wasmfn_alloc` handed out, `proto.Unmarshal`s the request, calls the registered `Runner.RunFunction`, turns a returned `error` or a panic into a fatal result, `proto.Marshal`s the response and returns `ptr<<32|len`.

### Key Components

```
cmd/function/main.go        kong CLI: CLI{Debug, Serve (default:"withargs" — every existing invocation and a DeploymentRuntimeConfig's args keep working), Validate}; CeilingFlags embedded in both (module-dir, max-module-size, module-timeout, module-memory-limit, cosign-key, enable-sandbox-*, sandbox-egress-policy) with ceilings() (sandbox.NewCeiling checked at startup, egress.LoadPolicy/New only with --enable-sandbox-egress → admission.Ceilings) and resolver(); ServeCmd.Run → engine.New, openCaches (afero stores under /tmp/function-wasm-cache), engine.NewCache → health NOT_SERVING → function.Serve(&Function{}) with warm-up (below) flipping it to SERVING; main maps validate's exitError to the exit code
cmd/function/validate.go    ValidateCmd: files (multi-doc YAML/JSON, - stdin) → Composition pipeline[].input of kind Input (wasm.fn.crossplane.io/v1beta1) or bare Input docs → per step: strict decode (unknown fields → warnings), admission.Admit, --xr → module.FromComposite / else module.ValidateFrom, --resolve → resolver + Verify + Fetch + engine.Inspect; text (one line per step, warnings indented) or --output json (one object per step per line); exit 0 admitted / 1 refused / 2 tool failure
cmd/function/fn.go          Function.RunFunction: the seven steps above; ceilings() → admission.Ceilings; load (cache Get + fetch log, shared with warm-up); checkManifest/manifestFor (memory → manifests store → Ref.Manifest; parsed once per digest); registryAuth for OCI step credentials; egress grant → per-run HTTP client on RunOptions
cmd/function/warm.go        --warm-modules: warmSource (path:<file> | OCI ref) → resolve → Verify → load → Release, at most --max-concurrent-compiles at once, failures logged per entry (recover included), never fatal
internal/engine             wasmtime wrapper — the only production importer of github.com/bytecodealliance/wasmtime-go/vNN (internal/testwasm uses its Wat2Wasm)
  engine.go                   Engine (config incl. MaxConcurrentRuns → run slots, on-demand epoch ticker + runs_in_flight gauge, linker with WASI + wasmfn.log + wasmfn.http; native unwind info off), Compile + checkABI (exports, both imports' types), Module leases; RunOptions (limits + PrivateTmp/Env + HTTP); Config.WithDefaults
  shape.go                    Inspect(wasm) → Shape{Exports, Imports, Memories, ABIError} by compiling with wasmtime (the only reader; the compiled code is released) — what guestfn build/push/inspect and validate --resolve show; Module.Shape() for a compiled module; Shape.HostImports
  run.go                      Run(ctx, m, req, log, RunOptions): run slot under ctx (not timed/counted if never obtained), private /tmp then store per call, deadline/limiter (Config capped by RunOptions), configureSandbox, ABI calls, guestError/trapText
  sandbox.go                  privateTmp/removePrivateTmp (os.MkdirTemp under os.TempDir(), removed after the store), configureSandbox (/tmp pre-opened read-write — the only pre-open a guest ever gets, host directories are not mountable — SetEnv sorted)
  hostlog.go                  wasmfn.log import → logging.Logger
  hosthttp.go                 wasmfn.http import → RunOptions.HTTP (engine.HTTPRequester, nil = refuse); JSON in, JSON out through the guest's wasmfn_alloc (re-entrant); the run's deadline bounds the request
  cache.go                    Cache: memory (idle TTL, LRU bound, leases) over the compiled-artifact store (mapped), single-flight loads with compile slots; Serialize/Deserialize/DeserializeFile/Version in engine.go
internal/manifest           the module manifest (docs/one-pager-module-manifest.md): Manifest{ABI, Name, Version, Source, Description, Requires{Egress{HTTP []SandboxHTTPRule}, Filesystem} — env is deliberately not a requirement (values the Composition sets, one day the request), Config{Schema}, MinRuntime}; Load (wasmfn.yaml, strict), Parse (the artifact layer: unknown top-level fields ignored, unknown requires refused, ≤ MaxSize), Validate (rules via sandbox.ValidateRules, semver, schema compiled once — jsonschema/v6, draft 2020-12, $ref to URLs refused), Check(Grants, config, runtimeVersion) with the fixed refusal strings, ValidateConfig, Sandbox() (the Composition block that satisfies requires), Summary, JSON, RuntimeVersion; LayerMediaType application/vnd.wasmfn.manifest.v1+json, FileName wasmfn.yaml
internal/admission          Admit(in, Ceilings{Engine engine.Config, Sandbox *sandbox.Ceiling, Egress *egress.Egress}) → Admitted{Options engine.RunOptions, Grant, HTTP *egress.Grant}: RunFunction's step 1 — sandbox shape, grants within the ceilings, egress within the policy, limits, module + policy shape — one function RunFunction and function validate share, refusal strings unchanged
internal/cache              afero content-addressed Store (verify on read for blobs), Subdir, RemoveOthers; DefaultDir /tmp/function-wasm-cache
internal/module             ModuleSource → Ref{Digest, Description, Fetch}
  module.go                   Resolver, Options (Blobs *cache.Store), Validate (type + exactly one of its object or from, no foreign object; digest-pinned oci refs, required http digest), Resolve switches on type, verified() (blob store + digest check), timed() (fetch metric)
  oci.go / http.go / path.go  one source each; oci keys on the manifest digest, fetches the manifest only inside Fetch and stores the layer by its digest; Ref.Manifest reads the artifact's manifest layer (ManifestLayer; verified, ≤ manifest.MaxSize; path/http: none); WasmLayer (the layer rule, shared with guestfn inspect; the manifest layer never counts as the only layer), IsTarLayer, ExtractWasm (a tar layer must hold ScratchModulePath = /fn.wasm exactly — nothing is guessed)
  from.go                     FromComposite(src, policy, composite): type + from → the field of the observed XR (spec./status. only), decoded strictly into the type's object, then admit()
  policy.go                   ValidatePolicy (shape; credentialsAllowList requires repositoryAllowList), ValidateFrom (what a from source needs without the XR: shape + the repositoryAllowList fence; function validate without --xr) and admit(): an XR-chosen OCI/HTTP source requires repositoryAllowList; its normalized location (registry/repository, scheme://host/path — dot segments refused by Validate) must match a prefix, credentials only from credentialsAllowList and only within those repositories; static sources bypass
  auth.go                     AuthFor: step credential (.dockerconfigjson | username/password) → authn.Authenticator
  signature.go                Verifier: cosign key-based signature check (<repo>:sha256-<digest>.sig, simple-signing payload; ECDSA/RSA/ed25519); no sigstore dependency
internal/sandbox            Validate (shape of the Input's sandbox: http rules host XOR hostPattern — bare host names and one-wildcard patterns via egress.ValidHost/ValidHostPattern — methods from the enum, normalized pathPrefix; env keys identifiers, values NUL-free), RequestsEgress;
                            Options/NewCeiling (the operator's flags, checked once: a private /tmp probed under os.TempDir()), Ceiling.Grant(sandbox) → Grant{PrivateTmp, Env} or an error naming the grant and the flag; a nil Ceiling allows nothing. No mounts: host directories are deliberately not mountable into a module
internal/egress             HTTP egress through the host (sandbox.egress): Policy (the --sandbox-egress-policy file, defaults, DefaultBlockedCIDRs) → New → Egress (the ceiling) → Grant(rules) (intersection, refuses rules outside the ceiling) → Client(log) per run: admit (scheme, host/pattern, method, normalized path prefix), dialer that resolves and judges every address then dials the checked one, redirects re-checked, budgets, metric + audit line; Request/Response are the ABI's JSON payloads
internal/testwasm           WAT fixtures implementing ABI v1 (testwasm.Fixed; the allocator is $wasmfn_alloc so a Body can call it) and BuildGuest (go build of a Go guest); wasi.go: ReadFile/WriteRead/Environ fixtures that use raw WASI (path_open, fd_read/write, environ_get) on pre-open fd 3 — the private /tmp — and hand the bytes back as a normal Result message, exiting with the errno on failure
input/v1beta1               Input{Module ModuleSource{Type, OCI, HTTP, Path, From}; Policy; Limits; Sandbox; Config} → package/input CRD (CEL rules on module, policy, sandbox http rules — tooling only: Crossplane never installs it, the runtime enforces every rule itself)
pkg/wasmfn/                 guest SDK: register.go (Register, handle), abi_wasip1.go (exports), logger*.go, config.go, http*.go (HTTPClient: *http.Client over the wasmfn.http import; ErrNoHostHTTP natively; HTTPError for a host refusal)
cmd/guestfn                 CLI (CGo — it links internal/engine for the runtime's own verdicts): main.go (init --lang go|tinygo|rust; build with toolchain detection + engine.Inspect verdict + wasmfn.yaml validation and the example config against its schema; push refusing a non-ABI module, adding the manifest layer from wasmfn.yaml/--manifest with --module-version/--revision, the wasm config with layerDigests through a partial.CompressedImageCore, OCI standard annotations, printing the module: and sandbox: blocks), inspect.go (file → engine.Inspect; reference → manifest, layers, annotations, module.WasmLayer, the manifest layer's summary, --pull), manifest.go (manifest validate <file>, manifest show <ref>; shared OCI helpers), scaffold.go (scaffold composition --from fn.wasm|<ref> [--manifest] [--full]), internal/scaffold
                              (templates/<lang> incl. wasmfn.yaml.tmpl, goldens under testdata/<lang>, vendorproto.go run by go generate)
examples/hello-go           nested module; the go scaffold rendered for its own module path — kept identical by a test
examples/hello-tinygo       the tinygo scaffold rendered for itself: vendored proto → protoc-gen-go + protoc-gen-go-vtproto (internal/fnv1), own ABI glue + wasmfn.http helper (http.go), ~1.8 MB
examples/hello-rust         the rust scaffold rendered for itself: prost over the vendored proto, cdylib for wasm32-wasip1, own ABI glue + wasmfn.http helper (src/http.rs), ~250 KB
examples/render.sh          shared: start the runtime serving an example dir, crossplane render its example/, optionally --check
docs/abi.md                 the language-agnostic host/guest contract
```

## Codebase Structure

```
.
├── cmd/function/           the Crossplane function (package main): main.go (serve + validate), fn.go, validate.go, fn_test.go, validate_test.go (+ testdata/validate/), guest_test.go (e2e)
├── cmd/guestfn/            the CLI (package main): main.go, inspect.go, manifest.go, scaffold.go + internal/scaffold/{scaffold.go,vendorproto.go,templates/<lang>/,testdata/<lang>/}
├── internal/{admission,egress,engine,manifest,module,sandbox,testwasm}
├── input/v1beta1/          Input types (+ generate.go under input/ for controller-gen)
├── package/                crossplane.yaml + generated input CRD
├── pkg/wasmfn/             guest SDK — separate go.mod, no dependency on the root module
├── examples/hello-go/         example guest — separate go.mod, `replace ../../pkg/wasmfn`; built only by tests, CI's render job and local rendering (Makefile), never published
├── examples/hello-tinygo/  same guest, TinyGo + vtprotobuf — separate go.mod, `make generate` (protoc) regenerates internal/fnv1
├── examples/hello-rust/    same guest, Rust + prost — Cargo crate, `make build` targets wasm32-wasip1
├── examples/render.sh      shared render/render-check driver
├── docs/abi.md
├── Dockerfile              CGo build (cross gcc when BUILDARCH != TARGETARCH), Chainguard glibc-dynamic runtime image (digest-pinned, Renovate bumps it)
└── .github/workflows/      ci.yml (Go modules + the TinyGo/Rust render jobs), publish-pkg.yml, supplychain.yml, grype-scan.yml, tag.yml
```

Four Go modules plus a Cargo crate, no `go.work`: the root module never imports `pkg/wasmfn` or the examples (the root tests build them with their own toolchains in their directories), `wasmfn` never imports the root module (it would drag wasmtime-go/CGo into every guest and create a cycle), `examples/hello-go` uses a `replace` for the SDK, and `examples/hello-tinygo`/`hello-rust` depend on nothing in this repository — they carry the vendored `run_function.proto` and their own ABI glue.

## Key Concepts

### Input

The function receives an `Input` (`wasm.fn.crossplane.io/v1beta1`) — a KRM-like object whose CRD is generated only to describe its schema:

```go
type Input struct {
    metav1.TypeMeta   `json:",inline"`
    metav1.ObjectMeta `json:"metadata,omitempty"`
    Module  ModuleSource         `json:"module"`            // Type OCI|HTTP|Path (required) + exactly one of OCI{Ref, Credentials}, HTTP{URL, Digest}, Path, or From (the XR field holding that object)
    Policy  *Policy              `json:"policy,omitempty"`  // RepositoryAllowList, CredentialsAllowList — fences From sources only; never read from the XR
    Limits  *Limits              `json:"limits,omitempty"`  // Timeout (metav1.Duration), Memory (resource.Quantity) — each ≤ the runtime's ceiling flag
    Sandbox *Sandbox             `json:"sandbox,omitempty"` // Filesystem{PrivateTmp}, Env, Egress{HTTP} — all granted within the --enable-sandbox-* flags; host directories are not mountable
    Config  *runtime.RawExtension `json:"config,omitempty"` // opaque; the guest reads it via wasmfn.GetConfig
}
```

`module.type` is always the Composition's choice; with `from` the composite resource chooses the instance (an `{ref, credentials}` object for OCI, `{url, digest}` for HTTP, a string for Path), and `policy` — read from the Input only — says which repositories (prefixes) it may name and which step credentials it may spend (only within those repositories; a credentials list without a repository list is refused). `limits` narrow one run below `--module-timeout` / `--module-memory-limit`; more than a ceiling is a fatal result naming both. `sandbox` is validated for shape (`internal/sandbox.Validate`) and checked against the operator's ceiling (`sandbox.Ceiling.Grant`): `filesystem.privateTmp` gives the run an empty writable `/tmp` (`--enable-sandbox-private-tmp`) — the only directory a module can be given, host directories are deliberately not mountable — `env` sets exactly those variables (`--enable-sandbox-env`); a grant outside the ceiling is a fatal result naming the grant and the flag, and `egress` is HTTP through the host (`--enable-sandbox-egress` + `--sandbox-egress-policy`, `internal/egress`: a rule outside the operator's ceiling or a grant without the flag is a fatal result before the module runs; the sandbox one-pager). The design of the whole shape: `docs/one-pager-module-source-schema.md`.

The user-facing field reference lives in `README.md` ("Input reference"); keep it in sync with `input/v1beta1/input.go`.

### ABI v1

Guest exports `memory`, `wasmfn_alloc(u32)->u32`, `wasmfn_run(u32,u32)->u64`, optional `_initialize`; host imports `wasmfn.log(u32,u32,u32)` with a JSON `{"msg","kv"}` payload and `wasmfn.http(u32,u32)->u64` with a JSON `{method,url,headers,body}` request answered by a JSON `{status,headers,body}` / `{status:0,error}` written into a buffer the host obtains by calling the guest's `wasmfn_alloc` **re-entrantly** and returned as `ptr<<32|len` (both optional: always provided, type-checked at load, imported by the guests that use them); protobuf `RunFunctionRequest`/`RunFunctionResponse` on the wire. `docs/abi.md` is authoritative; `internal/engine.checkABI` enforces it at load, over wasmtime's decoded types — the one ABI check, whose verdict `engine.Inspect` reports to `guestfn build`/`push`/`inspect` and `function validate --resolve`. Payload evolution is protobuf's (and, for imports, JSON's) job; a mechanics change is a new set of export names.

### Admission and validation

Crossplane never installs a function's Input CRD, so every rule of the Input is enforced by the runtime on every request — `internal/admission.Admit` (sandbox shape and grants, egress, limits, module and policy shape) then `module.FromComposite` — and nowhere else in a cluster. `function validate composition.yaml…` (`cmd/function/validate.go`) runs the same `Admit` (and, with `--xr`, `FromComposite`; without it `module.ValidateFrom`) over Compositions offline against the same `CeilingFlags`, printing the runtime's own refusal strings; `--resolve` adds resolve → `Verify` → fetch → `engine.Inspect`. Keep the two in lockstep: a new Input rule goes into `Admit` (or the module package) so both the request path and `validate` apply it, and a new refusal string gets a fixture under `cmd/function/testdata/validate/`.

### Error semantics

A guest's returned `error` becomes a fatal result on a fresh response (what a gRPC error means for crossplane), a panic is recovered into a fatal result with the stack on stderr, and anything that stops the instance (trap, exit, deadline, memory limit) or the load (fetch, digest, compile, exports) is a fatal result from the host naming the module. The host never returns a gRPC error for guest problems and never crashes on them.

### Caches

Three on-disk stores under `/tmp/function-wasm-cache` (fixed; `internal/cache`, afero): `modules/<digest>` — every fetched blob (an OCI layer as delivered, tar included; an HTTP module; a manifest layer), verified on read, never held in memory; `compiled/<engine.Version()>/<digest>` — wasmtime artifacts (`Module.Serialize`; `Version()` = wasmtime-go module version + GOOS/GOARCH), other version dirs removed at startup once a day old (`cache.StaleVersionAge`); `manifests/<digest>` — the module manifest of each digest the runtime served (empty entry: none), so a warm volume asks the registry nothing about what a module requires. In memory: compiled modules only, reference-counted (`Module.Release`; freed on the last release), idle TTL 10 min (`engine.DefaultIdleTTL`), LRU-bounded by `--max-cached-modules` (`CacheOptions.MaxEntries`), single-flight loads under the cache's own context (`DefaultLoadTimeout`) with `--max-concurrent-compiles` slots (default 1), or nothing at all with `--enable-memory-cache=false` (`CacheOptions.NoMemory` — the internal option is the opt-out so the zero value keeps the tier); artifacts on a real filesystem are mapped with `DeserializeFile` (`cache.Store.Path`), never copied. Keys are digests stated in the Input — the manifest digest of `oci.ref` (the manifest pins the layer; the blob store keeps the layer under its own digest, so a lost artifact costs one manifest GET and no download), `http.digest` (required) — or hashed for served files. Full design: `docs/one-pager-cache.md`.

### Readiness, warm-up and the run bound

Readiness is answered twice — the gRPC health service on the function port and plain-HTTP `/readyz` (`/livez` always 200) on `--health-address` `:8081`, since the function port speaks mTLS and kubelet's gRPC probe cannot — and starts as not ready; `Function.warm` (`cmd/function/warm.go`) loads every `--warm-modules` entry (`repo[:tag]@sha256:…` or `path:<file>`) through the request's own resolve → `Verify` → `load` path, at most `--max-concurrent-compiles` at once, logs `Cannot warm module` per failure (never fatal, panics recovered) and then the status flips to ready — while the server already listens, so a probe reads not-ready rather than a refused connection and an early request is served cold or joins the load in flight. `--max-concurrent-runs` (`engine.Config.MaxConcurrentRuns`, 0 = unbounded) is a slot channel in the engine: `Run` takes a slot after the module is loaded and before the run is timed, waiting under the request context; a deadline while waiting is `waiting for a run slot: context deadline exceeded` — a fatal result at the function, not counted as a run. `function_wasm_module_runs_in_flight` is the gauge. Design: `docs/one-pager-cache.md` (warm-up), `docs/one-pager-resource-governance.md` (the bound).

### One-pagers

Design documents under `docs/one-pager-*.md` follow one pattern: the H1 is the feature name (`# WASM Sandbox`, `# Module and Compiled Artifacts Cache`), immediately followed by

```
* Owner: First Last (@github-handle)
* Reviewers: Function WASM Maintainers
* Status: Draft | Implemented, revision x.y
```

then the body. Bump the revision when the design changes; flip Draft → Implemented when the code lands.

### Signatures

`--cosign-key` loads PEM public keys into `module.Verifier`; `Ref.Verify` checks every OCI manifest digest once per process (`<repo>:sha256-<hex>.sig`, layer annotation `dev.cosignproject.cosign/signature`, payload `critical.image.docker-manifest-digest` must match), `RunFunction` calls it **before** the caches (an artifact on disk may predate the key), and non-OCI sources are refused. Keyless (Fulcio/Rekor) is deliberately unsupported: sigstore-go alone is ~370 modules and needs network access to Rekor/TUF at run time.

### Metrics

`internal/metrics` registers `function_wasm_module_{compile,fetch,run}_duration_seconds`, `function_wasm_module_runs_in_flight`, `function_wasm_module_cache_events_total`, `function_wasm_module_cache_bytes` and `function_wasm_module_http_requests_total{outcome}` (ok, refused, budget, error — no host label) on the default Prometheus registry (function-sdk-go serves it on `:8080/metrics`). Never add a module/digest/host label — unbounded cardinality. `metrics.Sample` reads a series back for tests (counter or gauge value, histogram sample count).

### Guest size

`pkg/wasmfn` imports only `function-sdk-go/proto/v1` (which brings grpc), protobuf, crossplane-runtime's `logging` interface (logr + klog) and stdlib `net/http` (already linked through grpc; a guest that calls `wasmfn.HTTPClient()` pays about 3 MB for the client paths, one that does not pays nothing) — a raw-proto guest is ~20 MB. `function-sdk-go/{request,response,resource}` add crossplane-runtime + Kubernetes apimachinery → ~75 MB, same as a native function. Do not add imports to `pkg/wasmfn` that pull those in.

## Development Guide

### Building

```bash
# Generate code (deepcopy + Input CRD, and the guestfn scaffold golden)
go generate ./...

# Build and vet (needs a C compiler: wasmtime-go is CGo)
go build ./... && go vet ./...

# The guest SDK and the example must also build for wasm
(cd pkg/wasmfn && GOOS=wasip1 GOARCH=wasm go vet ./...)
(cd examples/hello-go && GOOS=wasip1 GOARCH=wasm go build -buildmode=c-shared -o /dev/null .)

# Build the runtime image (Go version taken from go.mod in CI)
docker buildx build --platform linux/amd64,linux/arm64 --target image .

# Build a Crossplane package
crossplane xpkg build -f package --embed-runtime-image=runtime
```

### Testing

```bash
go test -race ./...                       # root: engine (WAT fixtures), module (in-memory registry), host, CLI, scaffold
go test -short ./...                      # skips the tests that build the example guests to wasm
(cd pkg/wasmfn && go test -race ./...)
(cd examples/hello-go && go test -race ./...)
(cd examples/hello-tinygo && go test -race ./...)
(cd examples/hello-rust && cargo test)
```

`cmd/function/guest_test.go` (`TestRunFunctionGuests`) builds `examples/hello-go` (Go), `examples/hello-tinygo` (skipped without `tinygo`) and `examples/hello-rust` (skipped without `cargo` + the `wasm32-wasip1` target) and runs each through the host with the same expectations — the three guests must stay behaviourally identical (default and configured greeting, a greeting fetched through the host's egress and refused without a grant, guest-side fatal on a bad config, guest logs). Toolchains: `rustup target add wasm32-wasip1`; TinyGo ≥ 0.41; `protoc` only for `make -C examples/hello-tinygo generate` (the plugins come from that module's `go.mod` tool directives) and for `cargo build` (prost-build).

### Test Patterns

Tests are table-driven with `map[string]struct` keyed by case name and single inline `args`/`want` structs:

```go
cases := map[string]struct {
    reason string
    args   args
    want   want
}{
    "TestCaseName": {
        reason: "Description of what this tests",
        args:   args{...},
        want:   want{...},
    },
}
for name, tc := range cases {
    t.Run(name, func(t *testing.T) { ... })
}
```

Compare with `go-cmp` — `cmp.Diff(want, got)`, using `cmp.AllowUnexported` / `cmp.Transformer` where needed — rather than field-by-field `if`/`t.Errorf`. Do **not** add third-party test packages (`testify`, `assert`, `require`, `gomock`); stdlib `testing` plus `go-cmp` only.

### Test Assertion Conventions

`RunFunction` tests compare the **full expected response** with `cmp.Diff` and `protocmp.Transform()`, and errors with `cmpopts.EquateErrors()`. Construct the whole `*fnv1.RunFunctionResponse` — including `Results` for fatal cases (`fatal(msg)`) — and diff it.

Guest modules for tests come from `internal/testwasm`: `testwasm.Fixed(t, rsp, Options{...})` assembles a WAT module implementing ABI v1 that returns `rsp` (with `Body`, `Initialize`, `Extra`, `SkipRun`, `RunSignature` to misbehave in one way each), and `testwasm.BuildGuest(t, dir)` builds a real Go guest (skipped with `-short`). The sandbox is tested through raw WASI rather than a language runtime: `testwasm.ReadFile(name)`, `WriteRead(name, content)` and `Environ()` are `Options` for `Fixed` whose guest opens `name` on pre-open descriptor 3 (`path_open`/`fd_read`/`fd_write`) or reads `environ_get`, and returns the bytes as the message of a normal `Result` (framed by hand, so keep payloads under 124 bytes); a WASI errno becomes `proc_exit(errno)`, i.e. `module exited with status 63` (EPERM: a path escape or a write into a read-only mount), `44` (ENOENT), `8` (EBADF: no pre-open at all). Registry-backed tests use `github.com/google/go-containerregistry/pkg/registry` behind `httptest`, wrapped in a basic-auth handler where credentials matter.

`cmd/function/validate_test.go` drives `function validate` through kong over the fixtures in `testdata/validate/` (one Composition per refusal family, an egress policy file, an XR, a bare Input, a non-YAML file) and diffs stdout, stderr and the exit code; `TestValidateResolve` writes fixture modules to a temp `--module-dir`. `cmd/guestfn/internal/scaffold` has a golden scaffold per language under `testdata/<lang>` (regenerated by `go generate ./...`) and `TestRenderMatchesExample`, which fails when an example and its templates drift apart. `cmd/guestfn` tests scaffold + build the tinygo and rust flavours when their toolchains are on PATH; `TestPush`/`TestInspect` use two hand-assembled modules (an ABI v1 one and one without `wasmfn_run`) against an in-memory registry, so they need no toolchain. CI's `render (go)`, `render (tinygo)` and `render (rust)` jobs run `make -C examples/<guest> render-check` — the guest compiled with its toolchain, served by the runtime built from the tree, rendered by `crossplane render`; `render (tinygo)` also regenerates `internal/fnv1` and fails on drift.

### Linting

```bash
golangci-lint run ./...
(cd pkg/wasmfn && golangci-lint run ./...)
(cd examples/hello-go && golangci-lint run ./...)
(cd examples/hello-tinygo && golangci-lint run ./...)
(cd examples/hello-rust && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo clippy --target wasm32-wasip1 -- -D warnings)
```

Configuration is `.golangci.yml` (golangci-lint v2, the version CI pins is in `.github/workflows/ci.yml`). `goimports` groups local imports under `github.com/jonasz-lasut/function-wasm`. Intentional `unsafe`/`gosec` findings (wasm i32↔pointer reinterpretation, subprocesses the CLI runs on purpose) carry a `//nolint:gosec // why` comment.

### Coding Conventions

- Go ≥ 1.26 (`go.mod` is the source of truth; CI and the Dockerfile read it). For a pointer to a literal use `new(value)`; never write pointer helper funcs.
- Prefer self-documenting code; comments explain *why*, as the existing ones do.
- English only; inclusive terminology (allowlist/blocklist, primary/replica, main branch).
- Conventional commits: `<type>(<scope>): <subject>`, imperative, ≤ 50 chars, one logical change per commit.
- Only `internal/engine` (and `internal/testwasm`, for `Wat2Wasm`) imports `github.com/bytecodealliance/wasmtime-go/vNN` — its majors change the import path.
- `wasmfn` must stay importable natively (unconstrained `doc.go`, portable `Register`/`NewLogger`/`GetConfig`/`HTTPClient`); only the exports and the two host imports are `//go:build wasip1`.

## Common Development Tasks

### Adding an Input Field

1. Add the field to `input/v1beta1/input.go` with kubebuilder markers (`+optional`, enums, patterns, `XValidation` CEL rules for cross-field invariants — the `module` object's type/object/from rules and `policy`'s list rule are the pattern). Cross-package types such as `metav1.Duration` need `+kubebuilder:validation:Type=string` before a `Pattern` applies. **The markers are documentation for tooling: Crossplane does not install a function's Input CRD (an Input is a fragment of a Composition), so every rule must also be enforced at runtime — `module.Validate`, `module.ValidatePolicy`, `sandbox.Validate`, `runOptions` — with a test**
2. `go generate ./...` — regenerates deepcopy and the CRD under `package/input/`
3. Wire it: a source option → `internal/module.Validate` / the resolver; something the XR may choose → `FromComposite`/`policy.go` (never read `policy`, `limits` or `sandbox` from the composite); a run budget → `runOptions` in `internal/admission` + `engine.RunOptions`; a sandbox grant → `internal/sandbox` (`Validate` for the shape, `Ceiling`/`Options` for its `--enable-sandbox-*` flag, `Grant` for the check) → `admission.Admit` → `engine.RunOptions` + `configureSandbox` for the mechanics. Everything `Admit` checks is what `function validate` checks — add a fixture under `cmd/function/testdata/validate/` for the new refusal; a new ceiling flag goes into `CeilingFlags` (`cmd/function/main.go`) so `serve` and `validate` share it
4. Document it in the README's "Input reference" table, and update the module-source-schema one-pager if the shape changes

### Adding a host import (ABI)

`wasmfn.http` (`internal/engine/hosthttp.go`, `pkg/wasmfn/http*.go`, `internal/engine/hosthttp_test.go`) is the worked example:

1. Define it in `internal/engine` (`linker.FuncWrap(HostModule, name, fn)`; `fn` may return a trailing `*wasmtime.Trap`), allow it in `checkABI` (`checkImport` with its exact type); reach per-run state through `caller.Data().(*call)` and pass what it needs in `RunOptions` (grouped per capability). To hand bytes back, call the guest's `wasmfn_alloc` through `caller.GetExport(ExportAlloc)` — copy the request out first and re-read `UnsafeData` afterwards, the guest may grow its memory — and return `ptr<<32|len`; keep failures inside the payload (never a trap for a refusal)
2. Add the guest side to `wasmfn` (`//go:wasmimport wasmfn <name>` in a `_wasip1.go` file, a portable fallback in a `_other.go` file behind a swappable package var so the codec is testable natively); a buffer the host allocated through `wasmfn_alloc` is found in `buffers` and released after use
3. Document it in `docs/abi.md` (JSON payload, memory protocol, what a refusal looks like); cover it with a `testwasm` fixture in `internal/engine` (a WAT body may call `$wasmfn_alloc` and return the host's answer as a `Result` message, see `httpImport`) and, if it needs a real guest, a Go fixture under `internal/testwasm/testdata`

### Changing the scaffold

Edit the example **and** its template set under `cmd/guestfn/internal/scaffold/templates/<lang>` (templates use `[[ ]]` delimiters so Go braces survive; the examples are the templates rendered for themselves — `examples/hello-go` for `go`, `hello-tinygo`, `hello-rust`; `wasmfn.yaml.tmpl` is the manifest every flavour ships), then `go generate ./...` to refresh the goldens; `TestRenderMatchesExample` keeps each pair in sync (everything but `go.mod`; the examples' `Makefile` and `Cargo.lock` are extra). Rendering an example from its template with `guestfn init --offline` and copying the files over is the quickest way to update it.

`go generate ./...` in the root also runs `cmd/guestfn/internal/scaffold/vendorproto.go`: it copies `run_function.proto` out of the function-sdk-go module the root `go.mod` requires into the tinygo/rust templates **and** examples (header names the version) and copies `examples/hello-tinygo/internal/fnv1/*.pb.go` into the tinygo template. After a function-sdk-go bump the order is: root `go generate ./...` → `go generate ./...` in `examples/hello-tinygo` (needs protoc; regenerates the codecs) → root `go generate ./...` again; CI's `check-diff` and `render (tinygo)` fail on drift.

### Rendering Locally

```bash
make -C examples/hello-go render          # build fn.wasm, start the runtime serving examples/hello-go, crossplane render example/
make -C examples/hello-go render-check    # same, asserting the output — what CI's `render` job runs
make -C examples/hello-tinygo render   # the TinyGo guest (tinygo on PATH)
make -C examples/hello-rust render     # the Rust guest (cargo + wasm32-wasip1 + protoc)
```

By hand: `go run ./cmd/function --insecure --debug --module-dir=examples/hello-go`, then in `examples/hello-go`: `guestfn build` (or the `GOOS=wasip1 GOARCH=wasm go build -buildmode=c-shared` it wraps) and `crossplane render example/xr.yaml example/composition.yaml example/functions.yaml --include-function-results`. `functions.yaml` uses the Development runtime, so the function must be running locally; the render engine itself runs in Docker.

## Key Dependencies

- `github.com/bytecodealliance/wasmtime-go/v47` — the sandbox. CGo binding to a prebuilt static libwasmtime (linux amd64/arm64, macOS, Windows). Needs gcc to build and test; the runtime image is `cgr.dev/chainguard/glibc-dynamic` (glibc + libgcc_s + libstdc++, nonroot, `/tmp`), digest-pinned in the Dockerfile — its Wolfi glibc must never be older than the one `golang:<go.mod version>` (Debian trixie, 2.41) links against. Each wasmtime release is a new Go major with a new import path.
- `github.com/crossplane/function-sdk-go` — gRPC server, `request`/`response`, protobuf types; also what guests code against (through `wasmfn`).
- `github.com/google/go-containerregistry` — OCI pull (`remote`, `name`, `authn`) and, in `guestfn push`, artifact creation.
- `github.com/alecthomas/kong` — both CLIs.

## Important Design Decisions

- **wasmtime-go over wazero** (Jonasz, 2026-08-16): better WASI support and the reference runtime, accepted CGo cost; the image moved from distroless/static to a glibc base rather than static-linking glibc — Chainguard `glibc-dynamic` over `distroless/cc-debian13` because it scanned clean where distroless carried won't-fix libc6 CVEs (Jonasz, 2026-08-16: fewer CVEs wins). `internal/engine` is the seam if this ever changes.
- **Not Extism** (Jonasz, 2026-08-16, after an evaluation with probes): Extism was considered for its Go host SDK, its PDKs in a dozen languages and its built-in outbound HTTP. Its Go SDK is a wazero host (pure Go, but idle since 2025-05, wazero pinned behind, per-runtime memory limits, no artifact serialization); its built-in `http_request` checks only a hostname glob against `allowed_hosts` on a hard-coded, non-replaceable `http.DefaultClient` — no scheme/method/path, no judgment of resolved addresses (a name resolving to loopback was dialled), redirects to non-allowed hosts followed, `HTTP_PROXY` honoured, refusal = the guest call fails with no in-band error, no budgets, audit or metric — so `internal/egress` would have stayed and the built-in would have been a denied dead-end; the kernel data path (8 bytes per host call) adds ~15–30 ms per MiB of request; and, measured on the three example guests, ABI v1 ran unmodified under wazero (the ABI is host-agnostic) but 4–17× slower per request (77 MB Go guest 74–97 ms vs 10–12 ms, compile 10.6 s serial vs 2.2 s, cache load 334 ms vs 13 ms). Decision: keep wasmtime-go and ABI v1; language breadth comes from more `guestfn` templates over the same ~40-line glue (`docs/one-pager-language-support.md`); an Extism-compatible import shim on wasmtime (running the 3.4 KB BSD-3 kernel + `extism:host/env` in our linker, ~M) is the answer only if a real user brings PDK-built plugins; if CGo ever becomes a hard operational blocker the fallback is wazero under the unchanged ABI, never the Extism SDK.
- **wasmtime is the only reader of a module** (Jonasz, 2026-08-17): a pure-Go reader of the binary format (`internal/wasmbin`: sections, imports, exports, custom sections; a pre-compile ABI reject costing milliseconds; a pure-Go `guestfn`) was built and rejected — compatibility with the runtime and no second wasm decoder to maintain win. `engine.Inspect` compiles a module with wasmtime and reports its `Shape` and `checkABI`'s verdict; `guestfn build`/`push`/`inspect` and `function validate --resolve` use it, so every verdict is the runtime's, and `guestfn` is CGo like the runtime. Accepted: a compile per build/push/inspect (seconds for a large Go guest), no pre-compile reject (wasmtime-go has no parse-only API), and no custom-section access through wasmtime — the module-manifest one-pager must carry its section another way. Details and the trade-off: `docs/one-pager-admission-tooling.md`.
- **`FROM scratch` images hold the module at `/fn.wasm`** (Jonasz, 2026-08-17): the tar path stays for `COPY fn.wasm /` images (`module.ScratchModulePath`), but the resolver never picks "the first `.wasm` file" — the entry must be that exact root path, and it is not configurable; raw `application/wasm` layers (`guestfn push`, `oras push`) are the recommended shape.
- **The module manifest is a layer of the OCI artifact, not a custom section** (Jonasz, 2026-08-17): the draft embedded a `wasmfn.manifest` custom section, which needs a WebAssembly section walker beside wasmtime — ruled out with the pure-Go reader. As a second layer (`application/vnd.wasmfn.manifest.v1+json`) the manifest is covered by the pinned manifest digest and a cosign signature, is written by `guestfn push` from `wasmfn.yaml` (or `oras push`), and needs no parser; `path` and `http` sources carry none, `guestfn manifest set` does not exist. It is a requirement, never a grant: `checkManifest` runs after admission and load, before `Run`, over grants the ceiling already admitted, so it can only refuse earlier. Design: `docs/one-pager-module-manifest.md`.
- **No local-loop machinery** (Jonasz, 2026-08-17): `function run`, request capture, per-run guest stderr files, panic headers on results, a per-run summary line, log budgets, `--debug` builds and `guestfn dev` (`docs/one-pager-local-loop.md`, now Withdrawn) were built in part and removed the same day; the loop is `guestfn build` + `crossplane render` against `go run ./cmd/function --insecure --module-dir=.` (`examples/render.sh`, `make render`) and `function validate`. Guest stderr stays the pod's.
- **`function validate` lives in the runtime binary** (Jonasz, 2026-08-17): the checks are the operator's — the same kong flags, env, policy file and version as the pod (`docker run <package image> validate …` works: the package image is the runtime image); `serve` is the default-with-args command so nothing existing changes; warnings never change the exit code; `guestfn push` refuses non-ABI modules with no override.
- **Transparent proxy**: the host forwards the whole request and returns the whole response; requirements/extra-resource round trips work with no runtime knowledge. The host adds `meta` only when the guest omitted it. One carve-out: the step credential named by `module.oci.credentials` pulled the module and is removed from the forwarded request — the guest never sees the host's registry secret.
- **Discriminated `module`, Composition-owned `policy`/`limits`/`sandbox`** (Jonasz, 2026-08-16): `module.type` + one typed object or `from` replaced six sibling fields so a source kind is stated once and rules that depend on where a source came from have a home; `policy`, `limits` and `sandbox` are top-level siblings read from the Input only, so an XR author can pick code but never widen permissions, grants or budget. The rule "an XR object may name credentials only from `credentialsAllowList` and only for a `repositoryAllowList` repository" is one rule in `policy.go`, which also refuses a credentials list without a repository list (the CRD says the same for tooling).
- **Sandbox types before behaviour**: the `sandbox` subtree shipped as validated types with a "not implemented yet" refusal so Compositions and tooling could settle on the schema; each capability then lands with its own flag — every one is switched on with `--enable-sandbox-<feature>` (bool, default off, `ENABLE_SANDBOX_<FEATURE>`) and declared with `--sandbox-<feature>-…`. The private `/tmp` (`--enable-sandbox-private-tmp`), environment (`--enable-sandbox-env`) and egress (`--enable-sandbox-egress`/`--sandbox-egress-policy`) are all in.
- **Sandbox filesystem and env are WASI, not ABI** (Jonasz, 2026-08-16): the private `/tmp` is a per-run `os.MkdirTemp` under `os.TempDir()` — `$TMPDIR` is the operator's quota knob (tmpfs `emptyDir` with `sizeLimit`), no extra flag — pre-opened at `/tmp` and removed after the store on every path out of `Run`, and env is `SetEnv` of exactly the grant. Nothing new for guests to import, so Go, TinyGo and Rust guests are equal; the ceiling is checked once at startup (`sandbox.NewCeiling`) so an unwritable `$TMPDIR` stops the runtime, not every request.
- **No host mounts** (Jonasz, 2026-08-16): named read-only mounts of operator-declared directories were built and removed the same day — a module's inputs come through the request (`config`, context, required resources), and mapping any part of the pod's filesystem into a module is a boundary the runtime does not offer; the private `/tmp` is the only directory a guest ever gets. `--enable-sandbox-mounts`/`--sandbox-mount` and `sandbox.filesystem.mounts` do not exist.
- **HTTP egress goes through the host, never a socket** (Jonasz, 2026-08-16, sandbox one-pager): the guest asks (`wasmfn.http`), the host resolves the name, judges every resolved address against a default block list (loopback, link-local, RFC 1918, CGNAT, ULA, NAT64, unspecified, multicast, reserved; `allowedCIDRs` punch holes, `blockedCIDRs` add, an explicit block wins), dials the checked address (no re-resolution, no `HTTP_PROXY`), terminates TLS, applies the Composition's rules (host or pattern ∩ the operator's ceiling, method, normalized path prefix — dot segments refused) on the first request and every redirect hop, enforces per-run budgets (requests, response bytes, redirects, timeout, and the run's own deadline), and writes one audit line (method, host, path without query, status or reason, bytes; never headers or body) plus a metric with an outcome label only. A refusal is a JSON error the guest reads, never a trap. The response travels back through the guest's own `wasmfn_alloc`, called re-entrantly — Go's `//go:wasmexport` supports it (verified with the httpguest fixture), and Rust/TinyGo allocators are plain functions. Budgets live in the policy file only (not per Input yet); redirects are followed host-side so non-Go guests get the final answer.
- **Guest error → fatal result** instead of a gRPC error: crossplane treats both as a failed step, fatal results are visible in `crossplane render --include-function-results`, and the wire stays one message (no envelope proto).
- **Memory-export ABI, not stdin/stdout**: wasmtime-go's WASI config only offers files or inherited descriptors for stdio, so a stream ABI would need temp files per call.
- **Fresh instance per request** (~10 ms): hermetic, no reentrancy, no leaks; the expensive part (compile, ~2.4 s for a 75 MB Go guest) is cached by content digest in memory and as a wasmtime artifact on disk.
- **Disk caches are bounded by LRU sweep, not per-entry policy**: `--max-cache-size` (off by default) removes least recently used entries across both stores (`cache.Sweep`; a read touches the entry) at startup and every ten minutes; entries are immutable and reproducible, so removal is always safe.
- **Digests are stated, not discovered** (Jonasz, 2026-08-16): OCI refs must be `@sha256:` pinned (`repo:tag@sha256:…` is fine, the tag is context) and `http.digest` is required — no tags alone, no request-time resolution, no tag TTL; the caches key on the stated digest and every fetch is verified against it. An OCI source carries no separate module digest: the manifest digest already pins the layer, and duplicating it would only be one more thing to get wrong. Fetched modules always go to disk and never stay in memory; compiled modules stay in memory ten minutes idle. No cache flags.
- **Guest scaffold is wasm-only**: only the runtime is a gRPC server; the guest's tests run natively because `Register`/`NewLogger` are portable.
- **`pkg/wasmfn` is a nested module** (importable, hence under `pkg/`), never depends on the root module, and re-implements the two `response` helpers it needs (`to`, `fatal`) rather than importing `function-sdk-go/response`, so raw-proto guests stay ~20 MB; `guestfn` and `wasmfn` are tagged in lockstep so a released CLI pins the matching SDK.
- **WASI argv is always `["function"]`**: an empty argv traps at `_initialize` because klog's `init` indexes `os.Args[0]` — every function-sdk-go guest imports klog transitively.
- **TinyGo guests use vtprotobuf, not protobuf-go's codec**: protobuf-go compiles under TinyGo but `proto.Unmarshal` panics with `unimplemented: reflect.New()`; vtprotobuf's generated `MarshalVT`/`UnmarshalVT` (with its pre-generated well-known types) are reflection-free. `pkg/wasmfn` itself is not usable from TinyGo for that reason, so the TinyGo and Rust examples carry their own ABI glue and their own `wasmfn.http` helpers (`encoding/json` works under TinyGo; Rust uses serde_json + base64).
- **Warm-up runs while the server listens, health NOT_SERVING until it is done**: a closed port for minutes of compiling would fail a liveness probe on the function port and tell a probe nothing; Not Serving on an open port is what gRPC health is for, and an early request is simply cold. Failures never hold readiness back — an entry is a hint, the request path is the truth — and entries are stated like a Composition states a module (digest-pinned, `path:` under `--module-dir`), never resolved.
- **The run bound is the engine's, taken after the load and outside the run metric**: `engine.Config.MaxConcurrentRuns` keeps the engine reusable and the bound next to the other ceilings; waiting after `Get` means a queued request holds a module lease but never a compile slot; taking the slot before `start` keeps `run_duration_seconds` about runs, and a request cut short while waiting is a fatal result that is not counted — it never ran. Off by default: the caller's concurrency is a cost the operator can compute, and a semaphore adds head-of-line blocking.

## Key Reference Documents

- `README.md` — user-facing behaviour, the Input reference, runtime flags, trust model
- `docs/abi.md` — the host/guest contract
- `docs/one-pager-cache.md` — the two on-disk caches and the memory tier
- `docs/one-pager-module-source-schema.md` — the Input in one place — discriminated `module`, top-level `policy` (allowlists for XR-chosen modules), `limits` (per-Composition budgets), `sandbox` (grants)
- `docs/one-pager-trust-model.md` — parties, what pins the code, credentials and `policy`, what the guest sees, threats (implemented)
- `docs/one-pager-resource-governance.md` — every bound on a run, `limits`, the tiers, compiles and disk; sizing; readiness; failure containment (implemented)
- `docs/one-pager-sandbox.md` — the sandbox grants: a private `/tmp` (host mounts deliberately not offered), environment and HTTP egress through the host, each behind its `--enable-sandbox-*` flag (implemented)
- `docs/one-pager-admission-tooling.md` — `function validate` (the runtime's own admission over a Composition, offline; `internal/admission`), `engine.Inspect` behind `guestfn inspect`/`build`/`push` and `validate --resolve`, `layerDigests` in pushed artifacts, the wasmtime-not-a-reader and `/fn.wasm` decisions (implemented)
- `docs/one-pager-module-manifest.md` — the module manifest as an artifact layer (required grants, `config` schema, ABI, min runtime), written by `guestfn push` from `wasmfn.yaml`, checked against the grant between load and run — narrowing only; `internal/manifest`, the manifests store (implemented)
- `docs/one-pager-local-loop.md` — `function run`, `guestfn dev`, request capture, summary line, per-run stderr, readable traps (withdrawn: the loop is render + `go run ./cmd/function --insecure`)
- `docs/one-pager-request-secrets.md` — secrets and files delivered from the request into env and the private `/tmp`, never from the host; the `env` shape decision (draft)
- `docs/one-pager-governance-perf.md` — fuel-based `limits.instructions`, egress response cache and rate limits, per-Input concurrency, registry mirror + OCI-layout source + `function precompile`, raw-bytes gRPC codec — phases over the resource-governance one-pager (draft)
- `docs/one-pager-language-support.md` — what a guest toolchain must produce, what "supported" means, the candidate matrix (Zig and C proven, AssemblyScript/Swift to spike, JS/Python/C# blocked by the core-module ABI), the optional JSON payload mode, order and non-goals (draft)
- `docs/one-pager-nix-devenv.md` — a Nix flake dev shell modelled on crossplane/crossplane's adoption, phased and additive; images stay on Docker (draft)
- `input/v1beta1/input.go` — authoritative Input schema
- `.claude/skills/cut-release/SKILL.md` — cutting a minor/major release (`release-X.Y` branch, tag, package publish); one release branch is kept at a time
- `.claude/skills/remediate-cves/SKILL.md` — patch releases for Grype/code-scanning findings against the latest release

## Releasing

Releases are driven by two skills; use them rather than improvising the branch/tag/publish sequence:

- **`/cut-release`** — a new minor or major version from `main` HEAD: new `release-X.Y` branch, tag (`.github/workflows/tag.yml`), GitHub release, package publish (`publish-pkg.yml` → `ghcr.io/jonasz-lasut/function-wasm`, mirrored to `xpkg.upbound.io/jonasz-lasut/function-wasm`), signing/attestation (`supplychain.yml`). The bump size is the user's choice, never inferred. The `pkg/wasmfn` module is tagged with the same version (`pkg/wasmfn/vX.Y.Z`) so `guestfn` scaffolds pin it.
- **`/remediate-cves`** — a patch release on the current `release-X.Y` branch for CVEs found by `grype-scan.yml` (weekly against the latest release). A wasmtime-go bump is in scope there (it is the sandbox's own security fix) but changes the import path on majors — `internal/engine` only.

## Troubleshooting

- **Fatal `_initialize failed: trap` from a Go guest**: the guest panicked during package init; its stack is in the function pod's stderr. An empty WASI argv is one cause (host bug — the runtime always sets it).
- **`module imports X.Y, which the host does not provide`**: the module needs an import outside `wasi_snapshot_preview1`, `wasmfn.log` and `wasmfn.http`; it was built for another host or uses sockets/threads.
- **`does not export "wasmfn_run"`**: not built as a reactor with the exports — for Go, `-buildmode=c-shared` and `wasmfn.Register` in an `init`.
- **First request slow, then fast**: expected — compile is per digest; the artifact under `/tmp/function-wasm-cache/compiled` makes the next process fast too if that path is on a volume.
- **`module.oci.ref … tags are not supported`**: pin the reference to the manifest digest — `repo@sha256:…` or `repo:tag@sha256:…`, as `guestfn push` prints it.
- **`module layer is a tar archive without /fn.wasm`**: a `FROM scratch` image must `COPY` the module to `/fn.wasm` exactly; the resolver does not look for other `.wasm` files. Prefer `guestfn push` / `oras push` (a raw `application/wasm` layer).
- **`guestfn build` says `built fn.wasm, but the runtime would refuse it: …`** (or `guestfn push` refuses): the module lacks the ABI — for Go, `-buildmode=c-shared` and `wasmfn.Register` in an `init`; the message is the runtime's own load-time refusal. `guestfn inspect fn.wasm` lists what the module exports and imports.
- **`module oci … requires sandbox.egress.http host … , which the Composition does not grant`** (or `requires sandbox.filesystem.privateTmp` / `requires runtime vX or newer`): the module's manifest declares a need its Composition does not grant — add the grant (`guestfn push` prints the `sandbox:` block; `guestfn scaffold composition --from <ref>` writes a step) or use a module that needs less; the operator's flags still cap what a grant may be.
- **`module oci … config does not match the module's schema: /greeting: got number, want string`**: the Input's `config` fails the module's `config.schema`; fix the config, or the schema in the module's `wasmfn.yaml`.
- **`module oci … has an invalid manifest: …`**: the artifact's `application/vnd.wasmfn.manifest.v1+json` layer is not a manifest this runtime accepts (not JSON, `abi` other than 1, an unknown `requires` field from a newer `guestfn`); rebuild/push with a matching `guestfn`.
- **`function validate` exits 1**: at least one step is refused — the line names the runtime's reason; exit 2 is the tool's own failure (a file it cannot read or parse, `--sandbox-egress-policy` without `--enable-sandbox-egress`). Run it with the flags the runtime is started with; a Composition admitted with none of the `--enable-sandbox-*` flags cannot use the sandbox.
- **`module.path is refused`**: the runtime was started without `--module-dir`.
- **`module.type is required` / `module.type OCI needs exactly one of module.oci and module.from` / `module.path is set but module.type is OCI`**: the Input predates the discriminated shape — add `type: OCI|HTTP|Path` and keep only the matching object (or `from`); `ociFrom`/`httpFrom`/`pathFrom` became `type` + `from`.
- **`… names credentials "x", but a module chosen by the composite resource cannot use the step's credentials`**: an XR-chosen OCI object names a step credential; allow it with `policy.credentialsAllowList` plus a `policy.repositoryAllowList` covering its registry, or drop `credentials` from the XR object and mount a Docker config into the runtime.
- **`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`** (or `limits.timeout … --module-timeout`): the Composition asks for more than the operator allows; lower the limit or raise the flag.
- **`sandbox.filesystem.privateTmp is refused: the runtime was started without --enable-sandbox-private-tmp`** (or `sandbox.env is refused … --enable-sandbox-env`, `sandbox.egress is refused … --enable-sandbox-egress`): the Composition asks for a capability the operator did not enable — start the runtime with the flag (a `DeploymentRuntimeConfig` sets args or the `ENABLE_SANDBOX_*` env) or drop the grant.
- **`--enable-sandbox-private-tmp: cannot create a private /tmp under …`** at startup: the ceiling is checked once before serving; point `TMPDIR` at a writable directory rather than expecting requests to fail one by one.
- **Guest gets `EPERM` under `/tmp`**: its path left the private `/tmp` (`/tmp/../x`); wasmtime resolves paths inside the pre-open, and there is no other directory to reach.
- **`cannot create the private /tmp`** (fatal result) or the startup probe failing: the runtime's `$TMPDIR` is not writable (a read-only root filesystem) — point `TMPDIR` at an `emptyDir` (tmpfs with `sizeLimit` to bound what a module may write).
- **`sandbox.egress is refused: the runtime was started without --enable-sandbox-egress`**: the Composition grants egress but the operator did not enable it — set the flag (and, usually, `--sandbox-egress-policy`).
- **`sandbox.egress.http[0].host "x" is outside the runtime's egress policy (allowed: …)`**: the grant asks for more than `--sandbox-egress-policy` admits; narrow the rule or widen the policy's `hosts`/`hostPatterns` (a pattern must sit under a policy pattern; an exact policy host never covers a pattern).
- **A guest's request fails with `wasmfn: sandbox.egress: … resolves to 10.0.0.5, which the egress policy blocks (10.0.0.0/8)`**: the host refuses private, loopback, link-local and cluster ranges by default; an operator who means modules to reach an in-cluster service adds it to the policy's `allowedCIDRs`.
- **A guest's request fails with `wasmfn: sandbox.egress: HTTP egress is not granted to this module`**: the module calls `wasmfn.HTTPClient()` but its Composition has no `sandbox.egress.http` rule (or the runtime has no `--enable-sandbox-egress`); the import always exists, the grant decides.
- **`module imports wasmfn.http with the wrong type, ABI v1 requires (i32, i32) -> (i64)`**: a hand-written import declaration (Rust/TinyGo glue) has the wrong signature.
- **Cannot create /tmp/function-wasm-cache at startup**: the pod's filesystem is read-only there — mount an emptyDir (or a volume) at that path through a `DeploymentRuntimeConfig`.
- **`module … failed: waiting for a run slot: context deadline exceeded`**: `--max-concurrent-runs` is set and the request's deadline passed while other runs held every slot; nothing ran. Raise the bound, shorten the modules that hold slots (`limits.timeout`), or read `function_wasm_module_runs_in_flight` to see the queue.
- **`Cannot warm module` at startup**: a `--warm-modules` entry did not load — the log line carries the entry and the reason (a tag instead of `@sha256:`, a `path:` without `--module-dir`, a missing file, a registry error, a `--cosign-key` refusal). The pod serves anyway; that module is loaded on its first request.
- **Readiness probe fails for a while after start**: the pod is warming `--warm-modules` (`Warming modules` … `Warmed modules` in the log, one compile per cold entry); a gRPC liveness probe on the same port must outlast it, a TCP one is unaffected.
