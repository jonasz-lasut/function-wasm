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
| the CLI | `cmd/guestfn` (root module) | `guestfn init` scaffolds a guest project, `guestfn build` compiles it, `guestfn push` publishes it |

## Architecture Overview

### Request Processing Flow

```
Crossplane RunFunctionRequest
    ↓
┌──────────────────────────────────────────────────────────────────────────────┐
│ cmd/function/fn.go: RunFunction()                                            │
│  1. request.GetInput → v1beta1.Input                       fatal on error    │
│  2. module.FromComposite(input.module, observed XR)      *From → concrete    │
│  3. registryAuth(req, module) → authn.Authenticator      (step credential)   │
│  4. resolver.Resolve(module) → module.Ref{Digest, Fetch}   internal/module   │
│       no I/O: oci manifest digest from the ref, http.digest from the Input,  │
│       path hashed by size+mtime                                              │
│  5. modules.Get(digest, fetch)                             engine.Cache      │
│       memory (idle TTL) → compiled artifact on disk → fetch (oci: manifest   │
│       GET → layer digest; blob store on disk → source, verified against the  │
│       blob digest; tar layer extracted) + Compile + Serialize to disk        │
│  6. engine.Run(ctx, module, req, log)                      internal/engine   │
│       fresh Store: WASI argv=["function"], no env/fs/net, epoch deadline,    │
│       memory limiter; _initialize → wasmfn_alloc → copy req → wasmfn_run     │
│       → packed ptr/len → proto.Unmarshal(RunFunctionResponse)                │
│       host import wasmfn.log → the request logger                            │
│  7. return the guest's response verbatim (meta filled if the guest omitted it)│
│     trap / timeout / OOM / bad ABI / fetch / compile → response.Fatal        │
└──────────────────────────────────────────────────────────────────────────────┘
    ↓
Crossplane RunFunctionResponse (whatever the module produced)
```

Inside the module (a Go guest built with `wasmfn`): `wasmfn_run` looks up the buffer `wasmfn_alloc` handed out, `proto.Unmarshal`s the request, calls the registered `Runner.RunFunction`, turns a returned `error` or a panic into a fatal result, `proto.Marshal`s the response and returns `ptr<<32|len`.

### Key Components

```
cmd/function/main.go        kong CLI → engine.New, openCaches (afero stores under /tmp/function-wasm-cache), module.NewResolver, engine.NewCache → function.Serve(&Function{})
cmd/function/fn.go          Function.RunFunction: the seven steps above; registryAuth for OCI step credentials
internal/engine             wasmtime wrapper — the ONLY importer of github.com/bytecodealliance/wasmtime-go/vNN
  engine.go                   Engine (config, epoch ticker, linker with WASI + wasmfn.log), Compile + checkABI
  run.go                      Run: store per call, deadline/limiter, ABI calls, guestError/trapText
  hostlog.go                  wasmfn.log import → logging.Logger
  cache.go                    Cache: memory (idle TTL) over the compiled-artifact store, single-flight loads; Serialize/Deserialize/Version in engine.go
internal/cache              afero content-addressed Store (verify on read for blobs), Subdir, RemoveOthers; DefaultDir /tmp/function-wasm-cache
internal/module             ModuleSource → Ref{Digest, Description, Fetch}
  module.go                   Resolver, Options (Blobs *cache.Store), Validate (digest-pinned oci refs, required http digest), verified() (blob store + digest check), timed() (fetch metric)
  oci.go / http.go / path.go  one source each; oci keys on the manifest digest, fetches the manifest only inside Fetch and stores the layer by its digest
  from.go                     FromComposite: ociFrom/httpFrom/pathFrom → the field of the observed XR (spec./status. only), decoded strictly
  auth.go                     AuthFor: step credential (.dockerconfigjson | username/password) → authn.Authenticator
  signature.go                Verifier: cosign key-based signature check (<repo>:sha256-<digest>.sig, simple-signing payload; ECDSA/RSA/ed25519); no sigstore dependency
internal/testwasm           WAT fixtures implementing ABI v1 (testwasm.Fixed) and BuildGuest (go build of a Go guest)
input/v1beta1               Input{Module ModuleSource; Config *runtime.RawExtension} → package/input CRD
pkg/wasmfn/                 guest SDK: register.go (Register, handle), abi_wasip1.go (exports), logger*.go, config.go
cmd/guestfn                 CLI: main.go (init --lang go|tinygo|rust, build with toolchain detection, push), internal/scaffold
                              (templates/<lang>, goldens under testdata/<lang>, vendorproto.go run by go generate)
examples/hello-go           nested module; the go scaffold rendered for its own module path — kept identical by a test
examples/hello-tinygo       the tinygo scaffold rendered for itself: vendored proto → protoc-gen-go + protoc-gen-go-vtproto (internal/fnv1), own ABI glue, ~1.3 MB
examples/hello-rust         the rust scaffold rendered for itself: prost over the vendored proto, cdylib for wasm32-wasip1, ~150 KB
examples/render.sh          shared: start the runtime serving an example dir, crossplane render its example/, optionally --check
docs/abi.md                 the language-agnostic host/guest contract
```

## Codebase Structure

```
.
├── cmd/function/           the Crossplane function (package main): main.go, fn.go, fn_test.go, guest_test.go (e2e)
├── cmd/guestfn/            the CLI (package main) + internal/scaffold/{scaffold.go,vendorproto.go,templates/<lang>/,testdata/<lang>/}
├── internal/{engine,module,testwasm}
├── input/v1beta1/          Input types (+ generate.go under input/ for controller-gen)
├── package/                crossplane.yaml + generated input CRD
├── pkg/wasmfn/             guest SDK — separate go.mod, no dependency on the root module
├── examples/hello-go/         example guest — separate go.mod, `replace ../../pkg/wasmfn`; built only by tests, CI's render job and local rendering (Makefile), never published
├── examples/hello-tinygo/  same guest, TinyGo + vtprotobuf — separate go.mod, `make generate` (protoc) regenerates internal/fnv1
├── examples/hello-rust/    same guest, Rust + prost — Cargo crate, `make build` targets wasm32-wasip1
├── examples/render.sh      shared render/render-check driver
├── docs/abi.md
├── Dockerfile              CGo build (cross gcc when BUILDARCH != TARGETARCH), distroless/cc runtime image
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
    Module ModuleSource         `json:"module"`           // exactly one of OCI{Ref, Credentials}, HTTP{URL, Digest}, Path, OCIFrom, HTTPFrom, PathFrom
    Config *runtime.RawExtension `json:"config,omitempty"` // opaque; the guest reads it via wasmfn.GetConfig
}
```

The user-facing field reference lives in `README.md` ("Input reference"); keep it in sync with `input/v1beta1/input.go`.

### ABI v1

Guest exports `memory`, `wasmfn_alloc(u32)->u32`, `wasmfn_run(u32,u32)->u64`, optional `_initialize`; host import `wasmfn.log(u32,u32,u32)` with a JSON `{"msg","kv"}` payload; protobuf `RunFunctionRequest`/`RunFunctionResponse` on the wire. `docs/abi.md` is authoritative; `internal/engine.checkABI` enforces it at load. Payload evolution is protobuf's job; a mechanics change is a new set of export names.

### Error semantics

A guest's returned `error` becomes a fatal result on a fresh response (what a gRPC error means for crossplane), a panic is recovered into a fatal result with the stack on stderr, and anything that stops the instance (trap, exit, deadline, memory limit) or the load (fetch, digest, compile, exports) is a fatal result from the host naming the module. The host never returns a gRPC error for guest problems and never crashes on them.

### Caches

Two on-disk stores under `/tmp/function-wasm-cache` (fixed; `internal/cache`, afero): `modules/<digest>` — every fetched blob (an OCI layer as delivered, tar included; an HTTP module), verified on read, never held in memory; `compiled/<engine.Version()>/<digest>` — wasmtime artifacts (`Module.Serialize`), other version dirs removed at startup. In memory: compiled modules only, idle TTL 10 min (`engine.DefaultIdleTTL`), single-flight loads, or nothing at all with `--disable-memory-cache` (`engine.CacheOptions.NoMemory`; large Go modules). Keys are digests stated in the Input — the manifest digest of `oci.ref` (the manifest pins the layer; the blob store keeps the layer under its own digest, so a lost artifact costs one manifest GET and no download), `http.digest` (required) — or hashed for served files. Full design: `docs/one-pager-cache.md`.

### One-pagers

Design documents under `docs/one-pager-*.md` follow one pattern: the H1 is the feature name (`# WASM Sandbox`, `# Module and Compiled Artifacts Cache`), immediately followed by

```
* Owner: First Last (@github-handle)
* Reviewers: Function WASM Maintainers
* Status: Draft | Implemented, revision x.y
```

then the body. Bump the revision when the design changes; flip Draft → Implemented when the code lands.

### Signatures

`--cosign-key` loads PEM public keys into `module.Verifier`; the resolver verifies every OCI manifest digest once (`<repo>:sha256-<hex>.sig`, layer annotation `dev.cosignproject.cosign/signature`, payload `critical.image.docker-manifest-digest` must match) and refuses non-OCI sources. Keyless (Fulcio/Rekor) is deliberately unsupported: sigstore-go alone is ~370 modules and needs network access to Rekor/TUF at run time.

### Metrics

`internal/metrics` registers `function_wasm_module_{compile,fetch,run}_duration_seconds` and `function_wasm_module_cache_events_total` on the default Prometheus registry (function-sdk-go serves it on `:8080/metrics`). Never add a module/digest label — unbounded cardinality. `metrics.Sample` reads a series back for tests.

### Guest size

`pkg/wasmfn` imports only `function-sdk-go/proto/v1` (which brings grpc), protobuf and crossplane-runtime's `logging` interface (logr + klog) — a raw-proto guest is ~20 MB. `function-sdk-go/{request,response,resource}` add crossplane-runtime + Kubernetes apimachinery → ~75 MB, same as a native function. Do not add imports to `pkg/wasmfn` that pull those in.

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

`cmd/function/guest_test.go` (`TestRunFunctionGuests`) builds `examples/hello-go` (Go), `examples/hello-tinygo` (skipped without `tinygo`) and `examples/hello-rust` (skipped without `cargo` + the `wasm32-wasip1` target) and runs each through the host with the same expectations — the three guests must stay behaviourally identical (default and configured greeting, guest-side fatal on a bad config, guest logs). Toolchains: `rustup target add wasm32-wasip1`; TinyGo ≥ 0.41; `protoc` only for `make -C examples/hello-tinygo generate` (the plugins come from that module's `go.mod` tool directives) and for `cargo build` (prost-build).

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

Guest modules for tests come from `internal/testwasm`: `testwasm.Fixed(t, rsp, Options{...})` assembles a WAT module implementing ABI v1 that returns `rsp` (with `Body`, `Initialize`, `Extra`, `SkipRun`, `RunSignature` to misbehave in one way each), and `testwasm.BuildGuest(t, dir)` builds a real Go guest (skipped with `-short`). Registry-backed tests use `github.com/google/go-containerregistry/pkg/registry` behind `httptest`, wrapped in a basic-auth handler where credentials matter.

`cmd/guestfn/internal/scaffold` has a golden scaffold per language under `testdata/<lang>` (regenerated by `go generate ./...`) and `TestRenderMatchesExample`, which fails when an example and its templates drift apart. `cmd/guestfn` tests scaffold + build the tinygo and rust flavours when their toolchains are on PATH. CI's `render (go)`, `render (tinygo)` and `render (rust)` jobs run `make -C examples/<guest> render-check` — the guest compiled with its toolchain, served by the runtime built from the tree, rendered by `crossplane render`; `render (tinygo)` also regenerates `internal/fnv1` and fails on drift.

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
- Only `internal/engine` imports `github.com/bytecodealliance/wasmtime-go/vNN` — its majors change the import path.
- `wasmfn` must stay importable natively (unconstrained `doc.go`, portable `Register`/`NewLogger`/`GetConfig`); only the exports and the host import are `//go:build wasip1`.

## Common Development Tasks

### Adding an Input Field

1. Add the field to `input/v1beta1/input.go` with kubebuilder markers (`+optional`, enums, patterns)
2. `go generate ./...` — regenerates deepcopy and the CRD under `package/input/`
3. Wire it in `cmd/function/fn.go` (or `internal/module.Validate` / the resolver for a source option)
4. Document it in the README's "Input reference" table

### Adding a host import (ABI)

1. Define it in `internal/engine` (`linker.FuncWrap(HostModule, name, fn)`), allow it in `checkABI`
2. Add the guest side to `wasmfn` (`//go:wasmimport wasmfn <name>` in a `_wasip1.go` file, portable fallback elsewhere)
3. Document it in `docs/abi.md`; cover it with a `testwasm` fixture in `internal/engine/engine_test.go`

### Changing the scaffold

Edit the example **and** its template set under `cmd/guestfn/internal/scaffold/templates/<lang>` (templates use `[[ ]]` delimiters so Go braces survive; the examples are the templates rendered for themselves — `examples/hello-go` for `go`, `hello-tinygo`, `hello-rust`), then `go generate ./...` to refresh the goldens; `TestRenderMatchesExample` keeps each pair in sync (everything but `go.mod`; the examples' `Makefile` and `Cargo.lock` are extra). Rendering an example from its template with `guestfn init --offline` and copying the files over is the quickest way to update it.

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

- `github.com/bytecodealliance/wasmtime-go/v47` — the sandbox. CGo binding to a prebuilt static libwasmtime (linux amd64/arm64, macOS, Windows). Needs gcc to build and test; the image is `distroless/cc` (glibc + libgcc). Each wasmtime release is a new Go major with a new import path.
- `github.com/crossplane/function-sdk-go` — gRPC server, `request`/`response`, protobuf types; also what guests code against (through `wasmfn`).
- `github.com/google/go-containerregistry` — OCI pull (`remote`, `name`, `authn`) and, in `guestfn push`, artifact creation.
- `github.com/alecthomas/kong` — both CLIs.

## Important Design Decisions

- **wasmtime-go over wazero** (Jonasz, 2026-08-16): better WASI support and the reference runtime, accepted CGo cost; the image moved from distroless/static to distroless/cc rather than static-linking glibc. `internal/engine` is the seam if this ever changes.
- **Transparent proxy**: the host forwards the whole request and returns the whole response; requirements/extra-resource round trips work with no runtime knowledge. The host adds `meta` only when the guest omitted it.
- **Guest error → fatal result** instead of a gRPC error: crossplane treats both as a failed step, fatal results are visible in `crossplane render --include-function-results`, and the wire stays one message (no envelope proto).
- **Memory-export ABI, not stdin/stdout**: wasmtime-go's WASI config only offers files or inherited descriptors for stdio, so a stream ABI would need temp files per call.
- **Fresh instance per request** (~10 ms): hermetic, no reentrancy, no leaks; the expensive part (compile, ~2.4 s for a 75 MB Go guest) is cached by content digest in memory and as a wasmtime artifact on disk.
- **Digests are stated, not discovered** (Jonasz, 2026-08-16): OCI refs must be `@sha256:` pinned (`repo:tag@sha256:…` is fine, the tag is context) and `http.digest` is required — no tags alone, no request-time resolution, no tag TTL; the caches key on the stated digest and every fetch is verified against it. An OCI source carries no separate module digest: the manifest digest already pins the layer, and duplicating it would only be one more thing to get wrong. Fetched modules always go to disk and never stay in memory; compiled modules stay in memory ten minutes idle. No cache flags.
- **Guest scaffold is wasm-only**: only the runtime is a gRPC server; the guest's tests run natively because `Register`/`NewLogger` are portable.
- **`pkg/wasmfn` is a nested module** (importable, hence under `pkg/`), never depends on the root module, and re-implements the two `response` helpers it needs (`to`, `fatal`) rather than importing `function-sdk-go/response`, so raw-proto guests stay ~20 MB; `guestfn` and `wasmfn` are tagged in lockstep so a released CLI pins the matching SDK.
- **WASI argv is always `["function"]`**: an empty argv traps at `_initialize` because klog's `init` indexes `os.Args[0]` — every function-sdk-go guest imports klog transitively.
- **TinyGo guests use vtprotobuf, not protobuf-go's codec**: protobuf-go compiles under TinyGo but `proto.Unmarshal` panics with `unimplemented: reflect.New()`; vtprotobuf's generated `MarshalVT`/`UnmarshalVT` (with its pre-generated well-known types) are reflection-free. `pkg/wasmfn` itself is not usable from TinyGo for that reason, so the TinyGo and Rust examples carry their own ABI glue.

## Key Reference Documents

- `README.md` — user-facing behaviour, the Input reference, runtime flags, trust model
- `docs/abi.md` — the host/guest contract
- `docs/one-pager-cache.md` — the two on-disk caches and the memory tier
- `docs/one-pager-sandbox.md` — design sketch (not implemented) for granting modules filesystem, HTTP egress or environment access
- `input/v1beta1/input.go` — authoritative Input schema
- `.claude/skills/cut-release/SKILL.md` — cutting a minor/major release (`release-X.Y` branch, tag, package publish); one release branch is kept at a time
- `.claude/skills/remediate-cves/SKILL.md` — patch releases for Grype/code-scanning findings against the latest release

## Releasing

Releases are driven by two skills; use them rather than improvising the branch/tag/publish sequence:

- **`/cut-release`** — a new minor or major version from `main` HEAD: new `release-X.Y` branch, tag (`.github/workflows/tag.yml`), GitHub release, package publish (`publish-pkg.yml` → `ghcr.io/jonasz-lasut/function-wasm`, mirrored to `xpkg.upbound.io/jonasz-lasut/function-wasm`), signing/attestation (`supplychain.yml`). The bump size is the user's choice, never inferred. The `pkg/wasmfn` module is tagged with the same version (`pkg/wasmfn/vX.Y.Z`) so `guestfn` scaffolds pin it.
- **`/remediate-cves`** — a patch release on the current `release-X.Y` branch for CVEs found by `grype-scan.yml` (weekly against the latest release). A wasmtime-go bump is in scope there (it is the sandbox's own security fix) but changes the import path on majors — `internal/engine` only.

## Troubleshooting

- **Fatal `_initialize failed: trap` from a Go guest**: the guest panicked during package init; its stack is in the function pod's stderr. An empty WASI argv is one cause (host bug — the runtime always sets it).
- **`module imports X.Y, which the host does not provide`**: the module needs an import outside `wasi_snapshot_preview1` and `wasmfn.log`; it was built for another host or uses sockets/threads.
- **`does not export "wasmfn_run"`**: not built as a reactor with the exports — for Go, `-buildmode=c-shared` and `wasmfn.Register` in an `init`.
- **First request slow, then fast**: expected — compile is per digest; the artifact under `/tmp/function-wasm-cache/compiled` makes the next process fast too if that path is on a volume.
- **`module.oci.ref … tags are not supported`**: pin the reference to the manifest digest — `repo@sha256:…` or `repo:tag@sha256:…`, as `guestfn push` prints it.
- **`module.path is refused`**: the runtime was started without `--module-dir`.
- **Cannot create /tmp/function-wasm-cache at startup**: the pod's filesystem is read-only there — mount an emptyDir (or a volume) at that path through a `DeploymentRuntimeConfig`.
