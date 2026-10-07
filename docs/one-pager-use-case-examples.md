# Use-Case Examples

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.2
* Tracking: https://github.com/jonasz-lasut/function-wasm/issues/112

## Context

Every example guest used to implement the same hello function. That proved
the ABI holds across languages, but showed a platform team nothing about why
they would put a WebAssembly module into a Composition. This document records
which real use cases the examples solve instead, why those, how the examples
are tested, and how they are published.

## What function-wasm adds

Existing functions already cover each use case below on its own:
function-cel-filter does conditional resources, function-tag-manager does
tags, function-status-transformer and function-auto-ready do status and
readiness, and Crossplane core is moving towards per-step, digest-pinned
function references chosen by the Composition author
([crossplane#7840](https://github.com/crossplane/crossplane/pull/7840)). What
remains distinct to function-wasm:

1. **The composite resource can choose the logic.** `module.from` reads the
   module source from an XR field, fenced by the `compositionPolicy`'s
   `pullModule`, and `module.allowEmpty` makes a step an empty hook until the
   XR fills it in. function-pythonic is the only other function with per-XR
   code, and it runs that code with `exec()`.
2. **Untrusted code runs sandboxed by default.** A module gets no network,
   files or environment; each capability is requested in its manifest and must
   be permitted by both Cedar layers. function-python, function-shell and
   function-kcl run user code with the pod's network and environment.
3. **Network access is requested, granted and audited.** A module asks the
   host, which resolves, checks the address against its block list, applies
   budgets and writes an audit line per request.
4. **One runtime, many modules.** Nothing is installed per module; modules are
   pinned by digest and can be signature-checked.

An example earns its place when its use case needs at least one of these.

## Selection

- **Real demand**, cited from crossplane/crossplane and crossplane-contrib
  issues (reactions at the time of writing) or a common infrastructure-as-code
  pattern.
- **A different function-wasm feature per example**, so the five together
  cover what the runtime offers.
- **Renders from files alone**: no cloud account and no internet. Egress
  examples fetch from a local fixture server.
- **Crossplane v2 shapes**: namespaced XRDs (`apiextensions.crossplane.io/v2`),
  no claims, namespaced provider resources (`*.aws.m.upbound.io`).
- **The languages platform engineers reach for get the strongest stories**:
  TypeScript and Python above C#.

## The examples

| Example | Language | Use case | What it shows | Demand |
|---|---|---|---|---|
| [`cloudflare-origin`](../examples/cloudflare-origin) | Rust (async, `wasi:http` 0.3) | Only Cloudflare may reach an origin: a security group plus one ingress rule per range Cloudflare publishes, read at reconcile time | Egress through the host under both policy layers, the block list, conditional resources | [crossplane#2712](https://github.com/crossplane/crossplane/issues/2712) (conditional resources, 84 reactions); Terraform's `data "http"` over Cloudflare's lists |
| [`policy-gate`](../examples/policy-gate) | TypeScript | The organisation's policy as the last step: mandatory tags, allowed regions and instance classes per environment | `requirements` through the wire-level proxy, fatal versus warning results | [crossplane#1225](https://github.com/crossplane/crossplane/issues/1225), [discussion #2617](https://github.com/crossplane/crossplane/discussions/2617), [provider-upjet-aws#169](https://github.com/crossplane-contrib/provider-upjet-aws/issues/169) |
| [`pdb-addon`](../examples/pdb-addon) | Go (function-sdk-go) | A tenant add-on hook: a `WebApp` Composition's last step runs the module the XR names; this add-on derives a PodDisruptionBudget from the desired Deployment | `module.from`, `allowEmpty`, the `pullModule` fence, the default sandbox | [function-go-templating#24](https://github.com/crossplane-contrib/function-go-templating/issues/24), [crossplane#6139](https://github.com/crossplane/crossplane/issues/6139) (staggered rollout), [crossplane#2524](https://github.com/crossplane/crossplane/issues/2524) |
| [`team-tags`](../examples/team-tags) | Python | Team metadata from a CMDB, looked up with that team's own token from one shared Secret keyed by the XR's namespace | Step credentials, egress with an Authorization header, the composition layer fencing where tokens may go | [crossplane#2099](https://github.com/crossplane/crossplane/issues/2099), [crossplane#7343](https://github.com/crossplane/crossplane/issues/7343), [provider-upjet-aws#169](https://github.com/crossplane-contrib/provider-upjet-aws/issues/169) |
| [`dashboard-bundle`](../examples/dashboard-bundle) | C# | Dashboards as code: a sha256-pinned zip of Grafana dashboards unpacked into one sidecar-labelled ConfigMap per dashboard | The private `/tmp`, egress, integrity pinning, refusing archive entries that escape | Grafana's dashboard sidecar, delivered as a versioned bundle |

Design choices worth keeping:

- **A failed external read is a fatal result, never an empty answer.**
  Crossplane applies nothing from a fatal pipeline, so `cloudflare-origin`'s
  rules from the last reconcile stay in place while Cloudflare's list cannot be
  read, instead of being deleted.
- **Tenancy comes from the namespace, never from the spec.** `team-tags` keys
  the shared Secret by the XR's namespace, which cluster RBAC sets; keying by a
  spec field would let an XR in team A's namespace spend team B's token.
- **A module sees only the credentials its manifest names.** `team-tags`
  declares `requires.credentials: [cmdb]`, and the runtime forwards that
  credential only where both policy layers permit `spendCredential`; the pull
  credential and every other step credential are edited out of the request
  before the run
  ([#122](https://github.com/jonasz-lasut/function-wasm/issues/122)). The
  tokens' boundary is then egress, which the same two layers fence.

### Considered and dropped

- **A health rollup** (crossplane#5643): an XR's `Ready` already aggregates its
  composed resources' readiness, and Crossplane drops a function's `Healthy`
  condition (with `Ready`, `Synced`, `UpToDate` and `Responsive`).
- **A subnet layout** from a VPC CIDR: upbound/function-cidr already covers it,
  and it shows no feature the table lacks.
- **Operations** (`Operation`, `CronOperation`, `WatchOperation`): the package
  declares only the `composition` capability, and `module.from` has no
  composite resource to read in an Operation.

### Languages not rewritten

TinyGo, Zig and C stay unpublished hello examples while
[#114](https://github.com/jonasz-lasut/function-wasm/issues/114) decides their
path to ABI v2 or their retirement. The ABI v1 Rust example was removed: the
rust scaffold and `cloudflare-origin` cover Rust as a component. The
AssemblyScript example was retired (2026-10-07): no component-model path
exists, and #114's spike showed that hand-written canonical-ABI glue carries
`run` and `log` but makes `wasi:http` a hand-maintained cost.

## Testing

- **Scaffolds stay minimal hello projects.** The guest suite
  (`crates/function/tests/guests.rs`) builds every language's scaffold from its
  golden, exactly what `guestfn init` writes, and runs the same ABI-level
  expectations over each (greetings, egress granted and refused, a guest-side
  fatal, logs). A scaffold has no lockfile, so the suite resolves dependencies
  the way a user's first build does; that caught the Go 1.27 break in #117.
- **Examples share only the plumbing with their scaffold**: the vendored ABI
  glue, the generated codecs, the proto and the WIT, held byte-identical by
  `examples_share_the_scaffold_plumbing`. The function, its manifest and its
  example manifests are the example's own.
- **Each example has its own unit tests in its language**, covering every
  fatal path.
- **Each example has a render test**, `example/xprin.yaml`, run by
  [xprin](https://github.com/crossplane-contrib/xprin). It renders each case
  and asserts what is composed declaratively, and validates the output against
  the XRD and vendored provider CRDs where every rendered kind has a schema.
  Suites assert composed resources and XR status, never function results (xprin
  collapses unnamed results onto one key); a fatal path fails the render, so it
  lives in the unit tests. `examples/render.sh` runs the runtime's side:
  `function validate` over every `example/xr*.yaml`, the operator policy in
  `example/policy.cedar`, and the fixture server for `example/fixtures/`.
- **The OCI path has an end-to-end scenario at the same render tier**,
  `test/e2e/oci/run.sh`: `pdb-addon` pushed to a registry behind basic auth
  and signed with cosign, a `WebApp` naming it by digest through
  `module.from`, and the runtime pulling it with the step credential the XR
  names, behind `pullModule` and `requireSignature`. The script sets the
  stage; xprin and `function validate --output json` assert.
  `test/e2e/README.md` sets out the tiers and the conventions.

## Publishing

The examples that work as published are released as CNCF Wasm OCI artifacts
with their module manifests, by `publish-pkg.yml`'s `publish-examples` job:

- **Names:** `ghcr.io/jonasz-lasut/wasmfn/examples/<name>`, beside the WIT
  world at `wasmfn/function`. A namespace per kind of artifact is the
  convention across OCI-distributed WebAssembly plugins (Kubewarden's
  `policies/`, wasmCloud's `components/`).
- **Versions:** each example's own semver from its `wasmfn.yaml`, tagged
  `v<version>`, independent of the runtime's. A version already published is
  never pushed again (the same registry probe as `publish-wit`), so a release
  publishes only the examples whose version moved, and a published tag stays
  immutable. `minRuntime` in a manifest states the runtime it needs.
- **When:** at a release, so a published example only ever targets a released
  runtime.
- **Signing:** keyless cosign plus SLSA provenance from the release workflow,
  with no key in the repository. The runtime verifies key-based signatures
  only (keyless waits on sigstore-rs,
  [#116](https://github.com/jonasz-lasut/function-wasm/issues/116)); until then
  an operator who requires signatures verifies the keyless one, copies the
  module and countersigns it with its own key.
- **References:** a release asset, `function-wasm-examples-<release>.yaml`,
  lists every published example's reference pinned by digest
  (`repo:v<version>@sha256:…`: the tag for people, the digest for the runtime).
  Nothing is committed back.
- **Which:** `cloudflare-origin`, `policy-gate` and `pdb-addon`. `team-tags` and
  `dashboard-bundle` request egress to placeholder hosts (`cmdb.example.org`,
  `artifacts.example.com`) that their users replace before publishing their
  own.

## Open

- The 2.0.0 world freeze
  ([#77](https://github.com/jonasz-lasut/function-wasm/issues/77)): the
  component examples target `wasmfn:function@2.0.0-draft` until then.
- A cluster-tier E2E, Crossplane installing the package in a kind cluster:
  mTLS, the operator policy from a mounted ConfigMap, readiness and warm-up,
  in-cluster egress. The tool is open: kyverno/chainsaw, or the Crossplane
  CLI's own test command once it ships upstream.
