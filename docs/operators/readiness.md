# Readiness and warm-up

This page is for operators who wire Kubernetes probes for a function-wasm pod
and decide whether to preload modules. It assumes Crossplane runs the function
from a package with a `DeploymentRuntimeConfig`.

## Probes

The runtime reports readiness in two places: the gRPC health service
(`grpc.health.v1.Health`) on the function port, and plain HTTP `/readyz` on
`--health-address` (`:8081`), which also serves `/livez`. Readiness means the
caches are open, the engine is up and the modules named by `--warm-modules`
(none by default) are loaded.

Use the HTTP endpoints for Kubernetes probes. Crossplane runs functions with
mTLS on the function port and kubelet's gRPC probe dials without credentials,
so a `grpc:` probe never succeeds outside `--insecure`.

```yaml
apiVersion: pkg.crossplane.io/v1beta1
kind: DeploymentRuntimeConfig
metadata:
  name: function-wasm
spec:
  deploymentTemplate:
    spec:
      template:
        spec:
          containers:
          - name: package-runtime
            readinessProbe:
              httpGet:
                path: /readyz
                port: 8081
            livenessProbe:
              httpGet:
                path: /livez
                port: 8081
```

## Warm-up

A ready pod hasn't necessarily loaded any module. The first request for a
module on a pod deserializes its compiled artifact from the disk cache, which a restart keeps
only on a volume, or compiles the module when the cache is cold.
`--warm-modules` moves that work ahead of readiness.

With the flag set, the runtime listens at once but reports `NOT_SERVING` (and `/readyz` answers 503)
while it loads the listed modules, one after another. With a warm volume each load takes
milliseconds. With a cold cache each costs one compile. The runtime reports
`SERVING` when every entry has loaded or failed. `/livez` answers throughout, so
a liveness probe is unaffected.

Warm-up runs the path a request does, so a warmed module is a memory hit for
its first request. With `--enable-memory-cache=false` the artifact stays on
disk instead.

## Failed entries

A failed entry is logged as `Cannot warm module` with the entry and the reason,
and never holds readiness back. The module loads on its first request, and the
pod remains ready. [Troubleshooting](troubleshooting.md) covers a readiness
probe that fails for a while after start.

## Trusted root

With `--cosign-trust-instance`, the first fetch of the Sigstore trusted root
runs just before the modules and is bounded to one minute. It never holds
readiness back either; [signatures](signatures.md) covers what a pod does
without a root.
