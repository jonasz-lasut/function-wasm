# Install

This page is for operators adding function-wasm to a cluster that already runs Crossplane. It installs the function package and checks that Crossplane accepted it.

Apply a `Function` with the package reference:

```yaml
apiVersion: pkg.crossplane.io/v1
kind: Function
metadata:
  name: function-wasm
spec:
  package: ghcr.io/jonasz-lasut/function-wasm:v0.6.0
```

The same package is published to `xpkg.upbound.io/jonasz-lasut/function-wasm`, for clusters that pull from `xpkg.upbound.io`.

One installed function serves any number of modules: a Composition step puts its module reference in the step's Input, so adding a module needs no further install.

Crossplane supports the three most recent minor releases, and function-wasm supports the same three. Pushing a release branch runs the end-to-end suite on all three, and a release is cut only when that run is green ([the release matrix](../project/testing.md#the-release-matrix) lists the versions).

## Check the install

Crossplane pulls the package and starts the function's pod. The function is ready when `INSTALLED` and `HEALTHY` are both `True`:

```shell
kubectl get function function-wasm
```

If it stays unhealthy, read the pod's logs and the Function's conditions (`kubectl describe function function-wasm`). [Troubleshooting](troubleshooting.md) lists the runtime's own refusals.

## Next steps

A fresh install runs every module in the default sandbox, which gives a module no network, filesystem or environment beyond the request ([trust model](trust-model.md)).

- Set flags, such as limits and caches, through a `DeploymentRuntimeConfig`: see [runtime flags](runtime-flags.md).
- Keep the caches across restarts on a volume that only this Function's pods mount: see [cache storage](runtime-flags.md#cache-storage).
- Add readiness and liveness probes and pre-load modules: see [readiness and warm-up](readiness.md).
- Require signed modules: see [signatures](signatures.md).
- Allow a module more than the default sandbox, such as egress or a private `/tmp`: see the [grant policy](grant-policy.md).
