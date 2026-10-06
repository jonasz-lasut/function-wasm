# pdb-addon

A tenant add-on for a platform's Composition: a
[Crossplane](https://crossplane.io) composition function in Go
([function-sdk-go](https://github.com/crossplane/function-sdk-go)),
compiled to a WebAssembly module (ABI v1) and run by
[function-wasm](https://github.com/jonasz-lasut/function-wasm).

The platform team ships a `WebApp` Composition. Its first step composes the
workload every WebApp gets, a Deployment and a Service; its last step is
reserved for a module the WebApp itself names in `spec.addOn`, and does
nothing until a team fills that field in. This module is one such add-on:
for every Deployment the earlier steps composed, it adds a
PodDisruptionBudget over the Deployment's own selector, so node drains and
other voluntary disruptions take its pods down a few at a time.

```yaml
apiVersion: platform.example.org/v1alpha1
kind: WebApp
metadata:
  name: shop
  namespace: default
spec:
  image: ghcr.io/example-org/shop:1.4.0
  replicas: 3            # default 2
  port: 8080             # the default
  addOn:                 # optional: leave it out and the hook runs nothing
    ref: ghcr.io/example-org/webapp-addons/pdb@sha256:…
```

## Why a module

A platform's Composition is one team's code, and every other team gets
exactly what it composes. A team that needs a little more - a disruption
budget, an extra label, a sidecar resource - files a change request
against it, or the platform team forks the Composition per team. The
reserved step is a third way: the platform keeps its Composition, and each
WebApp names the add-on it wants, so a new add-on digest rolls out one
WebApp at a time instead of through every WebApp of the Composition.

The platform team keeps the guardrails:

- **What a WebApp may name.** The step reads `spec.addOn` through
  `module.from`, and the Composition's `compositionPolicy` admits only
  references under one repository, default-deny: a WebApp that names a
  module anywhere else gets a fatal result naming the reference, and the
  module is never pulled. References must be pinned to a digest; a tag is
  refused.
- **What the add-on can do.** Nothing beyond the response. Its manifest
  (`wasmfn.yaml`) requires no capability, so it runs in function-wasm's
  default sandbox: no network, no filesystem, no environment, bounded time
  and memory. A tenant's code gets the request and nothing else, and what
  it returns is the desired state the next step sees. That is the point:
  the add-on can be written by anyone the policy admits, because it can
  reach nothing.
- **What the add-on keeps.** Everything earlier steps composed passes
  through untouched; the add-on only adds budgets, never replaces a
  resource an earlier step composed under the name it would use.

The budget follows the Deployment it guards: it takes the Deployment's
whole label selector, is named after the Deployment's composition resource
name (`deployment` gets `deployment-pdb`), and lets `maxUnavailable` pods
go at once, 1 unless the step's config says otherwise. A Deployment of
fewer than 2 replicas gets no budget and a warning instead: a lone pod
cannot stay available through a drain, so a budget over it either lets the
pod go anyway or blocks the drain.

## Run it

```shell
go test ./...         # the function's unit tests, natively
make build            # fn.wasm, with guestfn from this repository
make render           # serve it with function-wasm and crossplane render example/xr.yaml
make render-check     # the render test, example/xprin.yaml (needs xprin)
```

`example/` holds:

- `definition.yaml`: the `WebApp` XRD (Crossplane v2, namespaced).
- `composition.yaml`: the platform's step
  ([function-go-templating](https://github.com/crossplane-contrib/function-go-templating))
  and the reserved hook. For a local render the hook's `module.type` is
  `Path` and the runtime serves this directory (`--module-dir`), so a
  WebApp names the module file instead of a reference: `spec.addOn:
  fn.wasm`.
- `xr.yaml`: a WebApp of 3 replicas that names the add-on;
  `xr-no-addon.yaml`: one that names none, so the hook is skipped;
  `xr-single-replica.yaml`: one of a single replica, which gets no budget.
- `functions.yaml`: function-go-templating, and function-wasm on the
  Development runtime.
- `xprin.yaml`: the render test, one case per XR.

## In a cluster

Publish the module with its manifest, then name the digest `guestfn push`
prints in a WebApp:

```shell
guestfn push ghcr.io/example-org/webapp-addons/pdb:v0.1.0
```

The hook in the platform's Composition is the same step with `type: OCI`:

```yaml
  - step: add-on
    functionRef:
      name: function-wasm
    input:
      apiVersion: wasm.fn.crossplane.io/v1beta1
      kind: Input
      module:
        type: OCI
        from: spec.addOn
        allowEmpty: true
      compositionPolicy: |
        permit (principal, action == Action::"pullModule",
                resource in Repository::"ghcr.io/example-org/webapp-addons");
```

`module.type` is the Composition's choice, and `spec.addOn` holds the
source in that type's shape: `{ref: <repository>@sha256:<digest>}` for
`OCI`, a file name for `Path`. That is why the XRD leaves `spec.addOn`
untyped; function-wasm decodes it strictly and refuses any other shape.
The fence is per repository and boundary-correct:
`ghcr.io/example-org/webapp-addons/pdb` is admitted,
`ghcr.io/example-org/webapp-addons-other/pdb` is not. A `Path` source has
no host to fence: a WebApp can name only a file the operator put under the
runtime's `--module-dir` (a path that leaves it is refused), so the
`compositionPolicy` does not apply to it. To run only add-ons signed with
the organisation's key, start the runtime with `--cosign-key` (with a
`--sandbox-policy-file`, a `requireSignature` rule for the repository).

The step's config tunes the budget, checked against the manifest's schema:

```yaml
      config:
        maxUnavailable: 2    # an integer of at least 1; default 1
```

## Layout

- `fn.go`: `RunFunction`, an ordinary function-sdk-go function;
  `fn_test.go` covers the budgets, the skips, every fatal result and that
  earlier desired state is kept.
- `main.go` registers it with the vendored `internal/wasmfn` glue, which
  is the go scaffold's plumbing (`guestfn init`), kept identical to it.
- `wasmfn.yaml`: the manifest `guestfn push` publishes: no requirements,
  and the config schema.
