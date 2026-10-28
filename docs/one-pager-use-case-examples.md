# Use-case examples

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.7 (rewritten in the present tense over the Rust workspace; the cluster-tier E2E question answered)
* Tracking: https://github.com/jonasz-lasut/function-wasm/issues/112

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

## The examples

| Example | Language | Use case | What it shows | Demand |
|---|---|---|---|---|
| [`cloudflare-origin`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/cloudflare-origin) | Rust (async, `wasi:http` 0.3) | Only Cloudflare may connect to an origin: a security group plus one ingress rule per range Cloudflare publishes, read at reconcile time | Egress through the host under both policy layers, the block list, conditional resources | [`crossplane#2712`](https://github.com/crossplane/crossplane/issues/2712) (conditional resources); Terraform's `data "http"` over Cloudflare's lists |
| [`policy-gate`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/policy-gate) | TypeScript | The organisation's policy as the last step: mandatory tags, allowed regions and instance classes per environment | `requirements` through the wire-level proxy, fatal versus warning results | [`crossplane#1225`](https://github.com/crossplane/crossplane/issues/1225), [discussion #2617](https://github.com/crossplane/crossplane/discussions/2617), [`provider-upjet-aws#169`](https://github.com/crossplane-contrib/provider-upjet-aws/issues/169) |
| [`pdb-addon`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/pdb-addon) | Go (function-sdk-go) | A tenant add-on hook: a `WebApp` Composition's last step runs the module the XR names; this add-on derives a PodDisruptionBudget from the desired Deployment | `module.from`, `allowEmpty`, the `pullModule` fence, the default sandbox | [`function-go-templating#24`](https://github.com/crossplane-contrib/function-go-templating/issues/24), [`crossplane#6139`](https://github.com/crossplane/crossplane/issues/6139) (staggered rollout), [`crossplane#2524`](https://github.com/crossplane/crossplane/issues/2524) |
| [`team-tags`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/team-tags) | Python | Team metadata from a CMDB, looked up with that team's own token from one shared Secret keyed by the XR's namespace | Step credentials, egress with an Authorization header, the composition layer fencing where tokens may go | [`crossplane#2099`](https://github.com/crossplane/crossplane/issues/2099), [`crossplane#7343`](https://github.com/crossplane/crossplane/issues/7343), [`provider-upjet-aws#169`](https://github.com/crossplane-contrib/provider-upjet-aws/issues/169) |
| [`dashboard-bundle`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/dashboard-bundle) | C# | Dashboards as code: a sha256-pinned zip of Grafana dashboards unpacked into one sidecar-labelled ConfigMap per dashboard | The private `/tmp`, egress, integrity pinning, refusing archive entries that escape | Grafana's dashboard sidecar, delivered as a versioned bundle |

Design choices to keep:

- **A failed external read is a fatal result, never an empty answer.**
  Crossplane applies nothing from a fatal pipeline, so `cloudflare-origin`'s
  rules from the last reconcile stay in place while the module can't read
  Cloudflare's list (an empty answer would have deleted them).
- **Tenancy comes from the namespace, never from the `spec`.** `team-tags` keys
  the shared Secret by the XR's namespace, which cluster RBAC sets. Keying by a
  `spec` field would let an XR in team A's namespace spend team B's token.
- **A module sees only the credentials its manifest names.** `team-tags`
  declares `requires.credentials: [cmdb]`, and the runtime forwards that
  credential only where both policy layers permit `spendCredential`; the pull
  credential and every other step credential, which the runtime edits out of
  the request before the run
  ([#122](https://github.com/jonasz-lasut/function-wasm/issues/122)). The
  tokens' boundary is then egress, which the same two layers fence.

### Unpublished hello examples

`hello-zig`, `hello-c`, `hello-odin` and `hello-moonbit` are the same greeting
guest in four more languages, each an ABI v2 component. Zig and C share their
scaffolds' plumbing; Odin and MoonBit have no scaffold, and `guestfn build`
detects the MoonBit one by its `moon.mod`
([#155](https://github.com/jonasz-lasut/function-wasm/issues/155)). Rust,
Python, TypeScript and Go are covered by the use-case examples in the table.
C# has only `dashboard-bundle`. The language matrix is in
[other languages](guests/other-languages.md).

## Testing

- **Scaffolds are minimal hello projects.** The guest suite
  (`crates/function/tests/guests.rs`) builds every language's scaffold from its
  golden, exactly what `guestfn init` writes, and runs the same ABI-level
  expectations over each: greetings, egress granted and refused, a guest-side
  fatal, logs. A scaffold has no lockfile. The suite resolves dependencies
  the way a user's first build does, which caught the Go 1.27 break in
  [#117](https://github.com/jonasz-lasut/function-wasm/issues/117).
- **Examples share only the plumbing with their scaffold**: the vendored ABI
  glue, the generated codecs, the proto and the WIT, held byte-identical by
  `examples_share_the_scaffold_plumbing`. The function, its manifest and its
  example manifests are the example's own.
- **Each example has unit tests in its language**, covering each fatal path.
- **Each example has a `build-<lang>` job** in the `/e2e` workflow (`e2e.yml`):
  its toolchain, the build, test and lint targets, and a drift check of its
  generated codec and bindings.
- **Nothing in CI asserts what an example composes.** The Kubernetes-native
  end-to-end suite (`crates/e2e`, [testing](project/testing.md), #185) drives a
  committed test module of its own through Crossplane in a kind cluster. It
  covers the runtime behaviour the examples rely on (signatures, `module.from`,
  egress, credentials, env, the private `/tmp`, requirements, fatals, limits),
  and an example's unit tests cover its own logic.
- **`make render` is each example's development loop.** `examples/render.sh`
  runs `function validate` over every `example/xr*.yaml`, then serves the
  directory with `example/policy.cedar` as the operator policy and
  `example/fixtures/` on a local HTTP server for `crossplane render`
  ([render locally](guests/render-locally.md)).

## Publishing

The `publish-examples` job of `publish-pkg.yml` releases the examples that work
as published as CNCF Wasm OCI artifacts, with their module manifests:

- **Names:** `ghcr.io/jonasz-lasut/wasmfn/examples/<name>`, beside the WIT
  world at `wasmfn/function`. A namespace per kind of artifact is the
  convention across OCI-distributed WebAssembly plugins (Kubewarden's
  `policies/`, wasmCloud's `components/`).
- **Versions:** each example's own semver from its `wasmfn.yaml`, tagged
  `v<version>`, independent of the runtime's. A version already published is
  never pushed again (the same registry probe as `publish-wit`), so a release
  publishes only the examples whose version moved, and a published tag remains
  immutable. `minRuntime` in a manifest states the runtime it needs.
- **When:** at a release, so a published example only ever targets a released
  runtime.
- **Signing:** keyless cosign plus SLSA provenance from the release workflow,
  with no key in the repository. The runtime verifies the keyless signature
  against a Sigstore trusted root (`--cosign-trusted-root`) and admits it
  where the operator's `trustSigner` rule names this repository's release
  workflow on a version tag
  ([#116](https://github.com/jonasz-lasut/function-wasm/issues/116); the
  signatures page has the rule); an operator may still countersign with its
  own key instead.
- **References:** a release asset, `function-wasm-examples-<release>.yaml`,
  lists every published example's reference pinned by digest
  (`repo:v<version>@sha256:…`: the tag for people, the digest for the runtime).
  The release commits nothing back.
- **Which:** `cloudflare-origin`, `policy-gate` and `pdb-addon`. `team-tags` and
  `dashboard-bundle` request egress to placeholder hosts (`cmdb.example.org`,
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
retired on 2026-10-07 ([#114](https://github.com/jonasz-lasut/function-wasm/issues/114)).
AssemblyScript has no component-model path, and #114's spike showed that
hand-written canonical-ABI glue covers `run` and `log` but makes `wasi:http` a
hand-maintained cost. TinyGo builds the world on its own `wasip2` target, but
its bindings generator is unmaintained and the Go flavour can't share it. The
`go` scaffold and `pdb-addon` instead build a wasip1 reactor that `guestfn
build` embeds the world into and wraps.

## Open questions

The world's publication ([#106](https://github.com/jonasz-lasut/function-wasm/issues/106)):
the examples target `wasmfn:function@2.0.0`, frozen on 2026-10-07
([#77](https://github.com/jonasz-lasut/function-wasm/issues/77)). The
`publish-wit` job of `publish-pkg.yml` holds the world back while the `rust`
scaffold's `rust-toolchain.toml` pins a beta channel (`beta-2026-10-05` today),
and publishes it at the first release after the scaffold builds on stable Rust
([#129](https://github.com/jonasz-lasut/function-wasm/issues/129)).
