# Readiness and warm-up

The runtime reports readiness — caches open, engine up, the modules named
by `--warm-modules` (none by default) loaded — in two places: the gRPC
health service (`grpc.health.v1.Health`) on the function port, and plain
HTTP `/readyz` (plus `/livez`) on `--health-address` (`:8081`). Use the
HTTP one for Kubernetes probes: Crossplane runs functions with mTLS on the
function port, and kubelet's gRPC probe dials without credentials, so a
`grpc:` probe on `9443` never succeeds outside `--insecure`.

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

Ready is not warm: the first request for a module on a pod pays a
deserialize (with a warm volume) or a compile (~2 s for a large Go module).
`--warm-modules` moves that ahead of readiness: the runtime listens at
once but reports Not Serving while it loads the listed modules — a warm
volume makes that milliseconds, a cold cache one compile per module,
`--max-concurrent-compiles` at a time — and Serving when every entry is
loaded or has failed. Failures are logged (`Cannot warm module` with the
entry and the reason) and never hold readiness back: a wrong entry or an
unreachable registry costs that module its first request, not the pod its
traffic. `/livez` answers throughout, so a liveness probe is unaffected;
only `/readyz` (and the gRPC status) waits for warm-up. Warm-up runs the same
path a request does, so a warmed module is a memory hit for its first
request; with `--enable-memory-cache=false` it leaves the artifact on disk,
which is the point.
