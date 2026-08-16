# Nix Development Environment

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft, revision 0.1

A proposal to declare this repository's toolchain in a Nix flake — first as
an opt-in development shell that CI also runs, later (if it earns it) as the
place hermetic checks live — modelled on how crossplane/crossplane adopted
Nix in 2026, and adjusted for what is different here: a CGo runtime, three
guest toolchains, and a render loop that needs Docker. Nothing in this
document is implemented; it is the design to decide on.

## Background

The repository already needs more toolchains than most Go projects, and every
guest language the project takes on adds one
(`docs/one-pager-language-support.md`):

| need | today (local) | today (CI, `.github/workflows/ci.yml`) |
|---|---|---|
| Go ≥ 1.26 (`go.mod`) + a C compiler (wasmtime-go is CGo) | contributor's Go, system gcc/clang | `actions/setup-go` with `go-version-file: go.mod`; the runner's gcc |
| TinyGo 0.41.1 | contributor's install | `acifani/setup-tinygo` pinned to `0.41.1` |
| Rust stable + `wasm32-wasip1` + rustfmt + clippy | rustup | `dtolnay/rust-toolchain@stable` — **floating** |
| protoc + `protoc-gen-go` + `protoc-gen-go-vtproto` | contributor's protoc; plugins from `go.mod` tool directives | `arduino/setup-protoc@v3` — the action is SHA-pinned, the protoc **version is not** |
| golangci-lint v2 | contributor's install | `golangci/golangci-lint-action` with `GOLANGCI_VERSION` |
| Crossplane CLI (`crossplane render`) | contributor's install | `curl … crossplane/master/install.sh` — **latest release, unpinned** |
| Docker (the render engine runs in a container; the image build) | Docker Desktop / colima | the runner's Docker |

Three of those float, and the README's "Building" section is a list of
installers a new contributor follows by hand. On the owner's machine
`tinygo`, `protoc`, `golangci-lint` and `crossplane` already resolve through
a [devbox](https://www.jetify.com/devbox) global profile — a Nix store
without a `flake.nix` in the repository to say what it should contain. The
question is not "should the project touch Nix" but "should the repository
declare what one contributor's Nix profile already does, so every
contributor and CI get the same thing".

## What crossplane/crossplane does

The upstream project replaced Earthly with a Nix flake in January 2026
(`5690389 Add Nix flake for building Crossplane`, 2026-01-23; thirty commits
to `flake.nix` since) after a design in
`design/one-pager-build-with-nix.md` (Nic Cope, status Approved). Read from
the tree on 2026-08-16:

- **One flake, four kinds of outputs.** `packages` — cross-compiled Go
  binaries for seven platforms, OCI images and the Helm chart, built with
  `buildGoModule` (`CGO_ENABLED=0`; `nix/go-builders.nix` shares one
  vendor derivation per Go module via `proxyVendor` and
  `nix/vendor-hashes.nix`, refreshed by `nix run .#tidy`); `checks` — unit
  tests, code generation, Go/Helm/shell/Nix lint, run hermetically by
  `nix flake check` in the Nix sandbox (no network, no ambient filesystem);
  `apps` — `nix run .#test|lint|generate|tidy|e2e|hack|push-images|…`, fast
  and impure, for the inner loop; `devShells.default` — Go (from an
  `nixpkgs-unstable` overlay: `pkgs.unstable.go_1_25`), golangci-lint,
  kubectl, helm, kind, docker-client, buf, protoc plugins, controller-tools,
  nixfmt. `nixpkgs` is pinned to a stable channel (`nixos-26.05`) with the
  unstable overlay for the few tools that must be newer.
- **`nix.sh`.** A wrapper that runs Nix inside the `nixos/nix` Docker image
  (`--privileged` for Docker-in-Docker so `kind` E2E works), keeps the Nix
  store, Go caches and Docker data in a named volume, and points at the
  project's Cachix cache (`crossplane.cachix.org`), so Docker is the only
  prerequisite; native Nix is documented as optional. Their design notes
  that the macOS installer's `/nix` volume was a real barrier for some
  contributors — the wrapper is the mitigation.
- **CI.** Every job installs Nix with `cachix/install-nix-action@v31` and
  `cachix/cachix-action@v17`, then runs `nix build .#checks.x86_64-linux.<x>
  --print-build-logs` (generate, lint, test), `nix run .#e2e`, `nix build`
  for release artifacts and `nix run .#push-images`; a `nicknovitski/nix-develop`
  step gives CodeQL the dev shell. The contributor guide's rule: "Every PR
  must run (and pass) `./nix.sh flake check`."
- **Their measured payoff.** With a hot binary cache the publish-artifacts
  job fell from ~20 min (Earthly) to ~5 min; a cold cache is a wash.

Two things do not transfer to this repository directly: their Go build is
`CGO_ENABLED=0` (ours links libwasmtime), and their hermetic checks cover a
single-language tree (ours has to build TinyGo and Rust guests, whose
dependency graphs Nix can only see when they are vendored or hashed).

## Goals

- One declaration of the toolchain that local development and CI both use,
  pinned by `flake.lock` the way `go.sum` and `Cargo.lock` pin dependencies.
- Zero cost for a contributor who does not use Nix: `make`, `go test`,
  `cargo test` and the README's per-tool instructions keep working unchanged.
- A place for the toolchains new guest languages bring (Zig, wasi-sdk,
  AssemblyScript/Node) that does not turn the README into an installer list.
- CI toolchain versions that stop floating.

Nothing here gates `v0.1.0`: the shell is opt-in and every phase is
additive to the Makefile and CI paths that exist today.

## Non-goals (for this revision)

- Building the runtime image or the Crossplane package with Nix. The image
  is a digest-pinned Chainguard `glibc-dynamic` base that Renovate bumps and
  Grype scans (AGENTS.md, "wasmtime-go over wazero"); a `dockerTools` image
  would swap that base for nixpkgs' glibc and change the CVE story the
  project chose. Docker buildx stays.
- Replacing the Makefiles or the per-toolchain GitHub Actions on day one.
- A `nix.sh`-style Docker wrapper — worth copying only if native Nix turns
  out to be a barrier for contributors here.

## Proposal

### Phase 1 — `flake.nix` dev shell, `flake.lock`, `.envrc`, one CI job (S)

`flake.nix` at the root with `inputs.nixpkgs` (a stable channel plus an
unstable overlay, as upstream does) and a Rust input (`rust-overlay` or
`fenix`, since plain nixpkgs `rustc` cannot add the `wasm32-wasip1` target),
and `devShells.default = pkgs.mkShell { packages = [ … ]; }` listing:

| tool | nixpkgs attribute (checked 2026-08-16 with `nix eval`, unstable) | note |
|---|---|---|
| Go | `go` — 1.26.5 | **one patch behind `go.mod` (1.26.6)**: either an overlay that bumps the derivation, or leave `GOTOOLCHAIN=auto` so Go fetches the exact patch on first use inside the shell (acceptable in a dev shell, not in a hermetic check) |
| C toolchain | `gcc` (15.3) — or `stdenv.cc` | wasmtime-go ships prebuilt static libwasmtime per platform; the shell needs only a native compiler; the multi-arch image keeps its cross gcc in the Dockerfile |
| TinyGo | `tinygo` — 0.41.1 | exact match with CI |
| Rust | `rust-bin.stable.latest.default.override { targets = [ "wasm32-wasip1" ]; extensions = [ "rustfmt" "clippy" ]; }` (rust-overlay) or the fenix equivalent | verified to evaluate |
| protoc | `protobuf` — 35.1 | plugins stay `go tool` directives from `examples/hello-tinygo/go.mod` |
| golangci-lint | `golangci-lint` — 2.12.2 | exact match with `GOLANGCI_VERSION` |
| Crossplane CLI | **`crossplane-cli`** — 2.4.1 | **not `crossplane`**: that attribute is nginx's config parser and installs a different binary with the same name |
| Docker client | `docker-client` | the daemon stays the host's; `crossplane render` and `make render*` need it |
| wasm tooling | `wasmtime` 47.0.3 (same major as `wasmtime-go/v47`), `wasm-tools` | for probing modules by hand |
| future guests | `zig` 0.16.0, `assemblyscript` 0.28.13 / `nodejs_22`; `wasi-sdk` is **not packaged** (a `fetchurl` of the upstream release tarball, or a README install, when a C scaffold lands) | added when `docs/one-pager-language-support.md` lands them |

`flake.lock` is committed and refreshed deliberately (`nix flake update` in
its own PR; Renovate's `nix` manager can open those). `.envrc` with
`use flake` for direnv users. `README.md` "Building" gains one line ahead of
the per-tool instructions: `nix develop` (or `direnv allow`) gives every
tool below. CI gains **one** job — `DeterminateSystems/nix-installer-action`
(or `cachix/install-nix-action`, which upstream uses) plus
`DeterminateSystems/magic-nix-cache-action`, then `nix develop -c make …`
over the existing targets — alongside the existing jobs, so a broken flake
never blocks a PR by itself. The job doubles as the first macOS CI run if it
is given a `macos-14` matrix entry: both installers report Linux and macOS
as stable, and nothing exercises the macOS build of wasmtime-go today.

### Phase 2 — hermetic checks for what can be hermetic (M)

Once the shell has been in use for a while: `checks` for the root module
(`go vet`, `golangci-lint`, `go test -short ./...` — the CGo build works in
the sandbox because gcc and the vendored libwasmtime are inputs) using
`buildGoModule` with a shared vendor derivation, as upstream's
`nix/go-builders.nix` does. The guest builds are the hard part and are
*not* forced hermetic in this phase: `cargo` needs a `cargoLock`/crane
setup per crate, TinyGo builds need the module cache, and `crossplane
render` needs Docker — those stay `apps` (`nix run .#render-go` etc.),
impure by design. `nix flake check` therefore covers less than the full CI
matrix and says so.

### Phase 3 — decide whether the flake becomes the build (L, not proposed now)

Upstream went all the way (binaries, images, chart, release from `nix
build`). For this repository that would mean either building the runtime
image with `dockerTools` (dropping the Chainguard base) or keeping Docker
for images and Nix for everything else. Revisit after phases 1–2 with data;
the non-goal above stands until then.

## Risks

- **Two ways to build.** Phase 1 deliberately keeps both. Drift is bounded
  by CI running the same `make` targets through both paths; the flake job
  failing alone is a signal, not a blocker.
- **Go patch staleness.** nixpkgs trailed `go.mod` by one patch on the day
  of writing; `GOTOOLCHAIN=auto` in the shell hides it (Go downloads the
  toolchain once) at the cost of one network fetch, an overlay removes it at
  the cost of maintaining a hash. Recommendation: `GOTOOLCHAIN=auto` in
  phase 1, an overlay only if phase 2 makes the root module hermetic.
- **Learning curve.** The shell is a list of packages; upstream's experience
  (and their note that LLM assistance covered most of the flake work) is
  that this stays readable. Anything beyond the shell — vendor hashes,
  `checks` — is where the language starts to matter; that is why it is a
  later phase.
- **macOS installation.** The `/nix` volume needs root; upstream added
  `nix.sh` for contributors who could not install it. Here the shell is
  opt-in, so a contributor who cannot install Nix loses nothing.
- **Docker.** Nothing about `crossplane render` changes; the shell provides
  the client only.

## Alternatives considered

| option | verdict | why |
|---|---|---|
| **`flake.nix` dev shell, additive** | proposed | pins everything nixpkgs has (nearly all of it), works with the devbox profile already in use, the CI story upstream proved (`install-nix-action` + Cachix / `nix-installer` + magic cache) |
| `devbox.json` in the repo | not instead | it is Nix underneath with a friendlier CLI; a `flake.nix` is the more standard artifact for an OSS repository and its GitHub Actions path is better trodden; devbox users can consume the flake |
| mise / asdf (`.tool-versions`) | not instead | pins versions but not builds — each plugin shells out to the tool's own installer; weaker on exactly the axis (wasi-sdk, protoc plugins, Rust targets) that motivates this |
| devcontainer | complement | editor/Codespaces integration; could run `nix develop` inside; not a substitute for a shell on bare metal |
| status quo | the baseline | three floating versions in CI, an installer list in the README, every new guest language adds to both |

## Open questions

1. `flake.nix` or `devbox.json`? Both are Nix; the recommendation is the
   flake for the reasons above, but the owner's daily tool is devbox.
2. Which Nix installer in CI — `cachix/install-nix-action` (what upstream
   uses) or `DeterminateSystems/nix-installer-action`? Either; the choice
   decides which cache action pairs with it (Cachix needs an account and a
   cache; magic-nix-cache uses the GitHub Actions cache).
3. Should the phase-1 shell already carry the guest toolchains the
   language one-pager proposes (`zig`, `assemblyscript`), or only what the
   repository builds today?
4. Is a macOS CI job wanted at the same time (the flake makes it cheap), or
   is that a separate decision?
