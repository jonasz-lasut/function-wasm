# Use cases

function-wasm is the right tool when the *logic* of a pipeline step should be
replaceable without rebuilding, publishing and installing a Function package.
It's also the right tool when the person who maintains that logic isn't the
person who maintains the Composition.

- **A customization hook at the end of a pipeline.** The platform team publishes
  the Composition and reserves its last step for a module the consuming team
  provides - chosen per Composition, or per composite resource with
  `module.from: spec.hooks.module` (with `allowEmpty: true` the step does
  nothing until a team fills the field in). The team adjusts labels,
  annotations, sizing or values, adds a sidecar resource, or reshapes the
  desired state to its conventions, in the language it prefers, without a
  change request against the Composition. The platform team keeps the
  guardrails: digest pinning,
  a `compositionPolicy` (Cedar) fencing which registries the team may pick
  from, `--cosign-key` so only modules signed with the organisation's key
  run, and the resource caps of the sandbox (`limits` per step, the
  runtime's flags as ceilings).
- **Audit and compliance trails.** A module that runs last sees the complete
  observed and desired state. It can log a structured record of every
  reconcile - who requested what, which resources are about to change and how
  - through the runtime's logger (`wasmfn.NewLogger`, straight into the pod
  log and whatever forwards it), emit results that become events on the XR, or
  compose an audit resource next to the workload. Nothing else in the
  pipeline changes.
- **Policy gates.** Check the desired composed resources against
  organisational rules - naming, mandatory tags, allowed regions, cost
  guardrails - and return a fatal result with a precise message to stop the
  reconcile, or a warning that leaves a trace and doesn't stop it. Per-tenant
  policy is a per-tenant module, and the pipeline remains one.
- **Per-tenant, per-environment or per-region behaviour.** One Composition,
  and a module picked from the XR (`module.from`, a field under `spec.` or
  `status.` holding the source) or from a per-environment Composition - the
  same step running different logic for dev and prod, or for `eu` and `us`.
- **Fast iteration and safe rollback.** Write the function, `guestfn build`,
  render locally against the runtime, `guestfn push`, reference the digest.
  Rolling forward or back is a digest change in the Composition; nothing is
  reinstalled and every other Composition on the same runtime is untouched.
- **Many small functions, one runtime.** Each team maintains its modules,
  possibly in a language of its choice (Go, Rust, Python, see the
  [size table](guests/other-languages.md)); the cluster runs one function-wasm, which
  compiles each module once per digest and caches it.
- **Restricted networks.** Modules can come from an internal registry, an
  internal HTTP server (`http`) or a volume mounted into the runtime
  (`path`), and the on-disk caches keep fetched modules and compiled code
  across restarts, so a registry outage doesn't stop reconciling.

What it's not for: anything that needs to reach out of the sandbox at run
time beyond what its manifest declares and the policy layers permit. By
default a module doesn't get network, filesystem or environment access;
cluster state comes in through the request (observed state, required
resources), and the module returns desired state. The sandbox opens
selectively, per capability
([docs/one-pager-three-layer-authz.md](one-pager-three-layer-authz.md),
[docs/one-pager-sandbox.md](one-pager-sandbox.md)): a module declares in
its manifest (`requires`) what it can't run without. The capabilities are a
private `/tmp` for the request, environment variables bound to step
credentials (host directories aren't mountable, by design), the step
credentials it reads from its request, and **HTTP egress through the host**
to call the APIs it lists, with the host resolving, filtering, budgeting and
auditing every request - see [HTTP egress](operators/http-egress.md). The
runtime grants each capability only when the Input's `compositionPolicy` and the
operator's Cedar `--sandbox-policy-file` both permit it; a step credential a
module wasn't granted is edited out of the request it receives.

function-wasm doesn't support Crossplane Operations in 1.0 (decided
2026-10-07). The package declares only the `composition` capability
(`package/crossplane.yaml`), so Crossplane refuses the function in an
Operation pipeline rather than running it with a request it wasn't
written for. Support would first need a definition of what `module.from`
and `allowEmpty` mean for a request with no composite resource, and of how
operator grants scoped to an XR principal (`namespace`, `xrKind`) apply
when there is none. The sandbox works the same whatever kind of pipeline
calls it, so the limit is one of scope.

## Worked examples

Each guest language under [`examples/`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples) solves one of these, built,
rendered and asserted on every `/e2e` run
([docs/one-pager-use-case-examples.md](one-pager-use-case-examples.md) has
why these, how they're tested and published). Each release publishes the ones
that work as published (`cloudflare-origin`, `policy-gate`, `pdb-addon`) at
`ghcr.io/jonasz-lasut/wasmfn/examples/<name>`, signed and attested, with their
digest-pinned references in the release's `function-wasm-examples-<release>.yaml`:

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
  the add-on each WebApp names in `spec.addOn` (`module.from`, skipped while
  unset), fenced to one repository by the `compositionPolicy`; this add-on
  gives every Deployment the platform composed a PodDisruptionBudget, in the
  default sandbox, with no capability at all.
- [`examples/team-tags`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/team-tags) (Python): a team's owner and
  cost center come from the CMDB, never from the composite resource. After
  function-go-templating composes an S3 bucket, the module reads the record
  of the team the composite resource's namespace belongs to, with that
  team's token from a step credential holding every team's, and stamps
  `team`, `owner` and `cost-center` tags on every composed managed
  resource. The module receives that credential only because its manifest
  requires it and both policy layers permit it, and the tokens can go only
  where egress is granted: the one request its manifest declares, which the
  `compositionPolicy` and the operator's policy both fence.
- [`examples/dashboard-bundle`](https://github.com/jonasz-lasut/function-wasm/tree/main/examples/dashboard-bundle) (C#): dashboards
  as code, delivered as a bundle. A composite resource pins a team's zip of
  Grafana dashboards by sha256; the module fetches it through the host's
  egress, refuses any other content, unpacks it into its private `/tmp` and
  composes one ConfigMap per dashboard for Grafana's sidecar - the two
  capabilities its manifest requests.
