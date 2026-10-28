# Use cases

This page is for platform engineers and composition authors deciding whether
function-wasm fits a pipeline step. It lists what the runtime suits, where its
limits are and the examples that show it. It assumes you know Crossplane
compositions. [The introduction](introduction.md) covers what function-wasm is.

function-wasm suits a step whose *logic* should change without building,
publishing and installing a Function package. It also suits a step whose
logic someone other than the Composition's author maintains.

- **A customization hook at the end of a pipeline.** The platform team
  publishes the Composition and reserves its last step for the module each
  composite resource names in `module.from`; with `allowEmpty: true` the step
  does nothing until the field is set. That team changes labels, sizing or
  sidecars in its own language, without a change request against the
  Composition. The platform team keeps the guardrails: the digest pin and
  the `compositionPolicy` fence on repositories in the
  [Input](operators/input-reference.md), and the operator's
  [signature check](operators/signatures.md).
- **Policy gates.** A module that runs last sees the complete observed and
  desired state. It can check the composed resources against organisational
  rules such as naming, mandatory tags or allowed regions. A fatal result
  stops the reconcile, and a warning leaves a trace without stopping it.
- **Fast iteration and rollback.** Write the function, `guestfn build`, render
  locally, `guestfn push` and reference the digest in the Composition. Rolling
  forward or back is a digest change in that Composition; nothing is
  reinstalled.
- **Many small functions, one runtime.** Each team maintains its own modules,
  in any of the [supported languages](guests/other-languages.md). The cluster
  runs one function-wasm, which compiles each module once per digest and caches
  it.
- **Restricted networks.** Modules can come from an internal registry, an
  internal HTTP server (`http`) or a volume mounted into the runtime (`path`).
  The runtime keeps fetched modules and compiled code on disk, so with a
  volume at `/tmp/function-wasm-cache` a module it has already fetched keeps
  running across restarts while the registry is down, unless a signature is
  required: the runtime verifies that over the registry once per process. A
  module it has never fetched can't be loaded.

## Limits

By default a module doesn't get network, filesystem or environment access; cluster
state comes in through the request and the module returns desired state. A
module can request more in its manifest, and the runtime grants it only when
the Composition's policy and the operator's policy both permit it. See
[HTTP egress](operators/http-egress.md) and the
[trust model](operators/trust-model.md).

function-wasm doesn't support Crossplane Operations: its package declares
only the `composition` capability.

## Worked examples

Each example under
[`examples/`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples)
is a guest in one language that solves one of these cases. Every `/e2e` run
builds, tests and lints them, but nothing in CI asserts what an example
composes. The [use-case examples design record](one-pager-use-case-examples.md)
explains why these were chosen and how they're published.

A release publishes `cloudflare-origin`, `policy-gate` and `pdb-addon` to
`ghcr.io/jonasz-lasut/wasmfn/examples/<name>`, signed and attested, when the
version in the example's `wasmfn.yaml` isn't in the registry yet. The
digest-pinned references are in the release's
`function-wasm-examples-<release>.yaml`.

- [`examples/cloudflare-origin`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/cloudflare-origin) (Rust): only
  Cloudflare may connect to an origin. The module composes the origin's AWS
  security group and one ingress rule per range Cloudflare publishes, read
  at reconcile time through the host's egress - a capability its manifest
  requests and both policy layers must grant.
- [`examples/policy-gate`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/policy-gate) (TypeScript): the
  organisation's policy as the last pipeline step. The module asks Crossplane
  for the environment's EnvironmentConfig through `requirements`, adds its
  mandatory tags to every composed resource without overwriting the team's,
  and refuses a region or instance class the environment doesn't allow -
  one digest-pinned module the platform team rolls out on its own.
- [`examples/pdb-addon`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/pdb-addon) (Go): a customization hook at
  the end of a pipeline. A `WebApp` Composition reserves its last step for
  the add-on each `WebApp` names in `spec.addOn` (`module.from`, skipped while
  unset), fenced to one repository by the `compositionPolicy`; this add-on
  gives every Deployment the platform composed a PodDisruptionBudget, in the
  default sandbox, with no capability at all.
- [`examples/team-tags`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/team-tags) (Python): a team's owner and
  cost center come from the CMDB, never from the composite resource. After
  function-go-templating composes an S3 bucket, the module reads the record
  of the team the composite resource's namespace belongs to, with that
  team's token from a step credential holding every team's, and stamps
  `team`, `owner` and `cost-center` tags on every composed managed
  resource. The module receives that credential only where its manifest
  requires it and both policy layers permit it.
- [`examples/dashboard-bundle`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/dashboard-bundle) (C#): dashboards
  as code, delivered as a bundle. A composite resource pins a team's zip of
  Grafana dashboards by sha256; the module fetches it through the host's
  egress, refuses any other content, unpacks it into its private `/tmp` and
  composes one ConfigMap per dashboard for Grafana's sidecar - the two
  capabilities its manifest requests.
