# Use-case examples

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.11 (the artifacts published under the earlier names retired with the first publish under the new ones; the Zig and C scaffolds retired, #264; the examples named `function-<runtime>-<lang>` and the `function-oci-go` example, #263; the examples publish on their own, by hand, apart from the release, #245)
* Tracking: https://github.com/jonasz-lasut/guestfn/issues/112

## Context

A platform team deciding whether to put a WebAssembly module into a
Composition needs to see a real use case, and a hello function in every
language doesn't show one. This record says which use cases the examples solve,
why those, how the project tests the examples and how it publishes them.

## What function-wasm adds

Existing functions already cover each use case below on its own:
`function-cel-filter` does conditional resources, `function-tag-manager` does
tags, `function-status-transformer` and `function-auto-ready` do status and
readiness, and Crossplane core is moving towards per-step, digest-pinned
function references chosen by the Composition author
([`crossplane#7840`](https://github.com/crossplane/crossplane/pull/7840)). What
remains distinct to function-wasm:

1. **The composite resource can choose the logic.** `module.from` reads the
   module source from an XR field, fenced by the `compositionPolicy`'s
   `pullModule`, and `module.allowEmpty` makes a step an empty hook until the
   XR fills it in. function-pythonic is the only other function with per-XR
   code, and it runs that code with `exec()`.
2. **Untrusted code runs sandboxed by default.** A module starts with neither
   network nor files nor environment. The manifest requests each capability,
   and both Cedar layers must permit it. function-python,
   function-shell and function-kcl run user code with the pod's network and
   environment.
3. **Network access is requested, granted and audited.** A module sends its
   request to the host, which resolves, checks the address against its block
   list, applies budgets and writes an audit line per request.
4. **One runtime, many modules.** No install step per module: modules are
   pinned by digest and can be signature-checked.

An example belongs here when its use case needs at least one of these.

## Selection

- **Real demand**, cited from crossplane/crossplane and crossplane-contrib
  issues (reactions at the time of writing) or a common infrastructure-as-code
  pattern.
- **A different function-wasm feature per example**, so the five together
  cover what the runtime offers.
- **Renders from files alone**: no cloud account and no internet. Egress
  examples fetch from a local fixture server.
- **Crossplane v2 shapes**: namespaced XRDs (`apiextensions.crossplane.io/v2`)
  and namespaced provider resources (`*.aws.m.upbound.io`), with no claims.
- **The languages platform engineers use get the strongest stories**,
  TypeScript and Python over C#.

## Names

An example's directory is `function-<runtime>-<lang>`: `function-wasm-go`,
`function-wasm-rust`, `function-oci-go`. The reader who comes for a language
finds it by name, and the use case stays in the example's README and its
manifest's `description` (Jonasz, 2026-11-08,
[#263](https://github.com/jonasz-lasut/function-wasm/issues/263)). The
manifest's `name` follows the directory, and the published repository is
`ghcr.io/jonasz-lasut/guestfn/examples/<runtime>-<lang>`, the directory's
name less its `function-` prefix (Jonasz, 2026-11-08): `wasm-rust`,
`oci-go`. The first publish under the new names was on 2026-11-09. The
artifacts published under the earlier names, at
`ghcr.io/jonasz-lasut/wasmfn/examples/<name>`, retired with it (Jonasz,
2026-11-09): the e2e profiles pin `wasm-go` and `oci-go`, and the registry
drops the old packages.

## The examples

| Example | Language | Use case | What it shows | Demand |
|---|---|---|---|---|
| [`function-wasm-rust`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-rust) | Rust (async, `wasi:http` 0.3) | Only Cloudflare may connect to an origin: a security group plus one ingress rule per range Cloudflare publishes, read at reconcile time | Egress through the host under both policy layers, the block list, conditional resources | [`crossplane#2712`](https://github.com/crossplane/crossplane/issues/2712) (conditional resources); Terraform's `data "http"` over Cloudflare's lists |
| [`function-wasm-ts`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-ts) | TypeScript | The organisation's policy as the last step: mandatory tags, allowed regions and instance classes per environment | `requirements` through the wire-level proxy, fatal versus warning results | [`crossplane#1225`](https://github.com/crossplane/crossplane/issues/1225), [discussion #2617](https://github.com/crossplane/crossplane/discussions/2617), [`provider-upjet-aws#169`](https://github.com/crossplane-contrib/provider-upjet-aws/issues/169) |
| [`function-wasm-go`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-go) | Go (function-sdk-go) | A tenant add-on hook: a `WebApp` Composition's last step runs the module the XR names; this add-on derives a PodDisruptionBudget from the desired Deployment | `module.from`, `allowEmpty`, the `pullModule` fence, the default sandbox | [`function-go-templating#24`](https://github.com/crossplane-contrib/function-go-templating/issues/24), [`crossplane#6139`](https://github.com/crossplane/crossplane/issues/6139) (staggered rollout), [`crossplane#2524`](https://github.com/crossplane/crossplane/issues/2524) |
| [`function-wasm-python`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-python) | Python | Team metadata from a CMDB, looked up with that team's own token from one shared Secret keyed by the XR's namespace | Step credentials, egress with an Authorization header, the composition layer fencing where tokens may go | [`crossplane#2099`](https://github.com/crossplane/crossplane/issues/2099), [`crossplane#7343`](https://github.com/crossplane/crossplane/issues/7343), [`provider-upjet-aws#169`](https://github.com/crossplane-contrib/provider-upjet-aws/issues/169) |
| [`function-wasm-csharp`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-csharp) | C# | Dashboards as code: a sha256-pinned zip of Grafana dashboards unpacked into one sidecar-labelled ConfigMap per dashboard | The private `/tmp`, egress, integrity pinning, refusing archive entries that escape | Grafana's dashboard sidecar, delivered as a versioned bundle |
| [`function-wasm-cpp`](https://github.com/jonasz-lasut/guestfn/tree/main/examples/function-wasm-cpp) | C++ (mbedTLS) | A TLS Secret's self-signed certificate kept fresh: read through the response's `requirements`, reissued with a fresh P-256 key once fewer than `renewBeforeDays` remain | The read fence and the output fence scoped together, `requires.resources` and `composes` in the manifest, a C adapter over the generated structs | Self-signed certificates for internal services, rotated without cert-manager |

Design choices to keep:

- **A failed external read is a fatal result, never an empty answer.**
  Crossplane applies nothing from a fatal pipeline, so `function-wasm-rust`'s
  rules from the last reconcile stay in place while the module can't read
  Cloudflare's list (an empty answer would have deleted them).
- **Tenancy comes from the namespace, never from the `spec`.** `function-wasm-python` keys
  the shared Secret by the XR's namespace, which cluster RBAC sets. Keying by a
  `spec` field would let an XR in the namespace of team A spend the token of team B.
- **A module sees only the credentials its manifest names.** `function-wasm-python`
  declares `requires.credentials: [cmdb]`, and the runtime forwards that
  credential only where both policy layers permit `spendCredential`; the pull
  credential and every other step credential, which the runtime edits out of
  the request before the run
  ([#122](https://github.com/jonasz-lasut/guestfn/issues/122)). The
  tokens' boundary is then egress, which the same two layers fence.

### Unpublished hello examples

`function-wasm-zig`, `function-wasm-c`, `function-wasm-odin` and `function-wasm-moonbit` are the same greeting
guest in four more languages, each an ABI v2 component and none with a
scaffold. The Zig and C templates retired on 2026-11-08
([#264](https://github.com/jonasz-lasut/guestfn/issues/264)). Each of the four
examples has its plumbing in its own tree. The vendored proto and WIT are
checked in, and its `gen-proto` and `gen-bindings` targets write the codec and
the bindings. `guestfn build`
detects the first three by their `build.zig` and the MoonBit one by its
`moon.mod` ([#155](https://github.com/jonasz-lasut/guestfn/issues/155)). Rust,
Python, TypeScript and Go are covered by the use-case examples in the table.
C# has only `function-wasm-csharp`, C++ only `function-wasm-cpp`. The language matrix is in
[other languages](wasm/other-languages.md).

### The OCI example

`function-oci-go` is the hello function as an `oci@0.1` module
([#263](https://github.com/jonasz-lasut/function-wasm/issues/263), phase 6
of the [OCI runtime record](one-pager-oci-runtime.md)). An ordinary
function-sdk-go function sits behind the SDK's server. Its
`Dockerfile` cross-compiles it into an empty image, and `guestfn push
--image` appends the manifest. It doesn't share plumbing with a scaffold,
because the contract needs none. The `build-oci-go` job vets and
tests it and builds the image. Once it's published, the `oci` e2e profile gets the case the
`wasm` lifecycle profile has with the published Go example: the module
pulled from `ghcr.io` with its real signature.

## Testing

- **Scaffolds are minimal hello projects.** The guest suite
  (`crates/function/tests/guests.rs`) builds every language's scaffold from its
  golden, exactly what `guestfn init` writes, and runs the same ABI-level
  expectations over each: greetings, egress granted and refused, a guest-side
  fatal, logs. A scaffold has no lockfile. The suite resolves dependencies
  the way a user's first build does, which caught the Go 1.27 break in
  [#117](https://github.com/jonasz-lasut/guestfn/issues/117).
- **Examples share only the plumbing with their scaffold** - the vendored ABI glue, the generated codecs, the proto and the WIT - which `examples_share_the_scaffold_plumbing` holds byte-identical for the four pairs, Go, Rust,
  TypeScript and Python. The function, its manifest and its example manifests
  are the example's own. An example with no scaffold has the plumbing in its
  own tree, and the drift check of its build job compares the generated code with the pinned generators.
- **Each example has unit tests in its language**, covering each fatal path.
- **Each example has a `build-<lang>` job** in the `/e2e` workflow (`e2e.yml`) covering its toolchain, the build, test and lint targets, and a drift check of its
  generated codec and bindings.
- **Nothing in CI asserts what an example composes.** The Kubernetes-native
  end-to-end suite (`crates/e2e`, [testing](project/testing.md), #185) drives a
  committed test module of its own through Crossplane in a kind cluster. It
  covers the runtime behaviour the examples rely on (signatures, `module.from`,
  egress, credentials, env, the private `/tmp`, requirements, fatals, limits),
  and an example's unit tests cover its own logic.
- **`make render` is the development loop of each example.** `examples/render.sh`
  runs `function validate` over every `example/xr*.yaml`, then serves the
  directory with `example/policy.cedar` as the operator policy and
  `example/fixtures/` on a local HTTP server for `crossplane render`
  ([render locally](wasm/render-locally.md)).

## Publishing

`publish-examples.yml`, run by hand, publishes the examples that work as
published as CNCF Wasm OCI artifacts, with their module manifests:

- **Names:** `ghcr.io/jonasz-lasut/guestfn/examples/<runtime>-<lang>`, beside the WIT
  world at `guestfn/function`. A namespace per kind of artifact is the
  convention across OCI-distributed WebAssembly plugins (Kubewarden's
  `policies/`, wasmCloud's `components/`).
- **Versions:** each example's semver comes from its `guestfn.yaml` and is tagged `v<version>`, independent of the runtime's. A version already published is
  never pushed again (the same registry probe as `publish-wit`). Each run publishes only the examples whose version moved, and a published tag remains
  immutable.
- **When:** a maintainer runs the workflow from `main` after the version of an example moved. The examples have a lifecycle of their own. A
  release neither builds nor carries them, and a failed example build stays
  out of the package's publish run.
- **Signing:** keyless cosign plus SLSA provenance from the publish workflow,
  with no key in the repository. The runtime verifies the keyless signature
  against a Sigstore trusted root (`--cosign-trusted-root`) and admits it
  where the operator's `trustSigner` rule names this repository's release
  workflow on a version tag
  ([#116](https://github.com/jonasz-lasut/guestfn/issues/116); the
  signatures page has the rule); an operator may still countersign with its
  own key instead.
- **References:** the registry's: `repo:v<version>` for people, the digest
  `guestfn inspect` prints for a Composition's `module.oci.ref`. The workflow
  commits nothing back and attaches nothing to a release.
- **An OCI module:** `function-oci-go`'s image is built by `docker buildx`
  for `linux/amd64` and `linux/arm64` and pushed into the same repository
  by digest, under no tag. `guestfn push --image` then publishes the
  module, the image with its manifest layer, as `v<version>`, the
  repository's one tag per version (Jonasz, 2026-11-09), and the signature
  covers that digest. `oci-go` 0.1.0 predates this and also has
  `v0.1.0-image`.
- **Which:** `function-wasm-rust`, `function-wasm-ts`, `function-wasm-go` and
  `function-oci-go`. `function-wasm-python` and
  `function-wasm-csharp` request egress to placeholder hosts (`cmdb.example.org`,
  `artifacts.example.com`) that their users replace before publishing their
  own.

## Alternatives

A health rollup (`crossplane#5643`) lost because an XR's `Ready` already
aggregates its composed resources' readiness, and Crossplane drops a function's
`Healthy` condition (with `Ready`, `Synced`, `UpToDate` and `Responsive`).

A subnet layout from a VPC CIDR lost because `upbound/function-cidr` already
covers it and it doesn't show a feature the table lacks.

Operations (`Operation`, `CronOperation`, `WatchOperation`) lost because the
package declares only the `composition` capability, and `module.from` has no
composite resource to read in an Operation.

An AssemblyScript example and a TinyGo scaffold with `hello-tinygo` were
retired on 2026-10-07 ([#114](https://github.com/jonasz-lasut/guestfn/issues/114)).
AssemblyScript has no component-model path, and #114's spike showed that
hand-written canonical-ABI glue covers `run` and `log` but makes `wasi:http` a
hand-maintained cost. TinyGo builds the world on its own `wasip2` target, but
its bindings generator is unmaintained and the Go flavour can't share it. The
`go` scaffold and `function-wasm-go` instead build a wasip1 reactor that `guestfn
build` embeds the world into and wraps.

The Zig and C scaffolds retired on 2026-11-08
([#264](https://github.com/jonasz-lasut/guestfn/issues/264)). A `guestfn init`
template exists for a language with an official `function-sdk-<lang>` under the
`crossplane` GitHub organisation (Go, Rust, TypeScript and Python), the
languages a function author expects a project to support. Zig and C have none, and the
project maintained two template sets, their goldens and two guest-suite cases
for a scaffold nobody asked for. `function-wasm-zig` and `function-wasm-c` stay
as examples, checked by their build jobs, like Odin, MoonBit, C# and C++, and
`guestfn build` still builds a `build.zig` project.

## Open questions

The world's publication ([#106](https://github.com/jonasz-lasut/guestfn/issues/106)):
the examples target `guestfn:function@1.0.0`, frozen on 2026-10-07
([#77](https://github.com/jonasz-lasut/guestfn/issues/77)). The
`Publish WIT` workflow (`publish-wit.yml`) publishes it, run by hand from a
`wit-v<version>` tag.
