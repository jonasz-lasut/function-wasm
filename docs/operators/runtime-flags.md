# Runtime flags

The binary has two subcommands: `serve` - the default, so `function
--insecure --module-dir=.` and a `DeploymentRuntimeConfig`'s `args` don't
need a subcommand - and [`validate`](../guests/validate.md), which takes the
ceiling flags below with the same defaults and environment variables, so
`validate` checks a Composition against exactly what a runtime started with
those flags would admit.

| flag | env | default | purpose |
|---|---|---|---|
| `--module-dir` | `MODULE_DIR` | unset | serve `path` sources from this directory |
| `--max-module-size` | | `128` MB | largest module accepted |
| `--module-timeout` | | `30s` | wall-clock budget of one run; the ceiling for `limits.timeout` |
| `--module-memory-limit` | | `512` MB | linear memory a run may use; the ceiling for `limits.memory` |
| `--module-stack-limit` | `MODULE_STACK_LIMIT` | `512` KB | call stack a run may use (Wasmtime's own default). Past it the run fails with `trap: call stack exhausted`. Engine-wide - no Input field narrows it |
| `--enable-memory-cache` | `ENABLE_MEMORY_CACHE` | `true` | keep compiled modules in memory between requests. With `--enable-memory-cache=false` (or `--no-enable-memory-cache`) each request maps the module's compiled artifact from disk (6-8 ms for a large Go module) and releases it afterward |
| `--max-cached-modules` | `MAX_CACHED_MODULES` | `0` (unbounded) | most compiled modules resident at once. Beyond it the least recently used is dropped (freed once its last run ends). Artifacts are mapped from disk, so a resident Go module costs ~90 MB of file-backed memory |
| `--max-concurrent-compiles` | `MAX_CONCURRENT_COMPILES` | `1` | modules compiled at once. One compile already uses every core (~25 CPU-seconds and ~1 GB for a large Go module); further first requests wait their turn instead of multiplying that |
| `--max-cache-size` | `MAX_CACHE_SIZE` | `0` (unbounded) | MB the two on-disk caches may hold together; past it the runtime removes the least recently used entries (fetched modules and artifacts alike, ~230 MB per Go module version), at startup and every ten minutes. Size the volume, or set this below its size |
| `--cosign-key` | `COSIGN_KEY` | unset | PEM file of cosign public keys. On its own, all-or-nothing: only OCI modules with a matching key-based cosign 3 signature (`cosign sign --key`: a Sigstore bundle attached as an OCI 1.1 referrer, found through the referrers API or the `sha256-<hex>` tag fallback) run and `http`/`path` sources are refused. cosign 2's legacy `.sig` signatures aren't read. Keyless signatures are `--cosign-trusted-root`'s - see [signing](signatures.md). With a `--sandbox-policy-file`, it supplies the keys while the policy's `requireSignature` rules decide which repositories need a signature (a repository that the rules don't name runs unsigned) |
| `--cosign-trusted-root` | `COSIGN_TRUSTED_ROOT` | unset | Sigstore `trusted_root.json` (Fulcio authorities, Rekor and certificate-transparency keys, timestamp authorities, each with its validity window; `cosign initialize` fetches the public-good instance's into `~/.sigstore/root/`) that verifies keyless cosign signatures offline: the signing time comes from the bundle's RFC 3161 timestamp and its Rekor entry's signed entry timestamp, never from the clock. A keyless signature admits a module only where the operator policy's `trustSigner` rules trust its signer for the repository. Set it beside a `--sandbox-policy-file` with such rules (the runtime warns at startup otherwise). Combines with `--cosign-key`: a bundle with a key hint goes to the keys, one with a certificate to the root - see [signing](signatures.md). Excludes `--cosign-trust-instance` |
| `--cosign-trust-instance` | `COSIGN_TRUST_INSTANCE` | unset | `public-good`, `staging` or `github`: fetch that Sigstore instance's trusted root over TUF instead of reading `--cosign-trusted-root`'s file, and refresh it while serving. Bootstrapped from the TUF root embedded in the runtime; the verified metadata and the root are cached under `/tmp/function-wasm-cache/tuf/<instance>`, which a pod that can't connect to the repository starts from. Keyless signatures are refused (naming the instance and the last failure) until the first fetch or the cache yields a root. Readiness isn't held back. See [signing](signatures.md) |
| `--cosign-tuf-refresh` | `COSIGN_TUF_REFRESH` | `1h` | how often `--cosign-trust-instance` refreshes the trusted root, at least `1m` (the syntax of `--module-timeout`). A changed root replaces the one in use and forgets the verified-digest memo; a failed refresh keeps the last good root |
| `--max-concurrent-runs` | `MAX_CONCURRENT_RUNS` | `0` (unbounded) | module runs executing at once; a further request waits for a slot under its own deadline and, if that passes first, is a fatal result (`waiting for a run slot: deadline exceeded`) without having run. Unbounded, concurrency is the caller's - Crossplane's reconcile workers |
| `--max-total-run-memory` | `MAX_TOTAL_RUN_MEMORY` | `0` (unbounded) | total linear-memory budget in MB across all running modules. A component reserves nothing up front. The runtime reserves each growth, the initial memory included, from the pool as the guest claims it, and a run waits under its deadline while the pool is full. The pool accounts for the memory runs use rather than their worst-case ceilings. The pool denies a growth it can't serve before the run's deadline: the guest sees `memory.grow` fail, counted in `function_wasm_module_memory_denials_total` |
| `--warm-modules` | `WARM_MODULES` | unset | modules loaded before the health service reports Serving. Each is resolved and verified (`--cosign-key` applies), then compiled or mapped through the same caches a request uses. An entry is an OCI reference pinned to its manifest digest (`repo[:tag]@sha256:…`, pulled with the runtime's Docker config) or, with `--module-dir`, `path:<file>`. Repeatable or comma-separated. An entry that fails to load is logged with the reason and doesn't stop the pod from serving; that module is loaded on its first request as usual |
| `--egress-rate-limit-per-minute` | `EGRESS_RATE_LIMIT_PER_MINUTE` | `0` (off) | Sustained egress requests per minute per module digest (a process-wide token bucket). The one tunable egress budget. The rest are fixed: `timeout` `10s`, `maxRequests` 16, `maxResponseBytes` 4 MiB and `maxRedirects` 5. Enablement and the host allowlist and CIDR rules live in `--sandbox-policy-file` |
| `--egress-rate-limit-burst` | `EGRESS_RATE_LIMIT_BURST` | `0` (derived) | Burst tokens for `--egress-rate-limit-per-minute`; `0` derives `max(1, requestsPerMinute)` |
| `--sandbox-policy-file` | `SANDBOX_POLICY_FILE` | unset | [Cedar](https://www.cedarpolicy.com) document with the operator's grant policy - the operator layer of the three-layer capability decision and **the only authority that enables a sandbox capability**: which callers (by `principal.namespace`, `principal.xrKind`) a module's manifest may be granted a private `/tmp` (`usePrivateTmp`), environment bound to step credentials (`setEnv`, `spendCredential`), the step credentials it reads from its request (`spendCredential`) or egress (`grantEgress`, also the host allowlist) for. It may also carry the SSRF CIDR block/allow rules (`forbid`/`permit` on `Action::"dialAddress"` with `context.ip.isInRange(ip(…))`/`isLoopback()`), which compile at load into the egress block list (with the built-in default block list) - Cedar never runs on the dial path. Evaluated **default-deny** (a `forbid` wins): a capability without a matching permit is refused. When unset, every sandbox capability is refused and a runtime offers only the default sandbox. A mounted ConfigMap satisfies it. It's compiled once and immutable for the process (restart to reload). See [operator grant policy](grant-policy.md) |
| `--health-address` | `HEALTH_ADDRESS` | `:8081` | plain-HTTP `/livez` (the process is up) and `/readyz` (200 once the caches are open and `--warm-modules` are loaded, 503 while warming) - what a Kubernetes probe can reach, since the function port speaks mTLS; empty disables them |
| `--metrics-address` | `METRICS_ADDRESS` | `:8080` | plain-HTTP Prometheus `/metrics` endpoint (see [Metrics](metrics.md)) - the port function-sdk-go serves for the Go runtime; empty disables it |
| `--ttl` | | `60s` | TTL of responses the runtime itself produces (fatal results); a module sets its own |

The usual function-sdk-go flags (`--insecure`, `--debug`, `--tls-certs-dir`,
`--address`, `--max-recv-message-size`) apply too. `--debug` turns on the
runtime's own debug lines (guest log records sent at debug level, the
credentials withheld from a module, trap details) in a human-readable
format; every other crate, `wasmtime` and `cranelift` included, stays at `info`,
so a compile under `--debug` doesn't print Cranelift's per-pass lines. A
`RUST_LOG` that's set replaces the flag's filter in full
(`RUST_LOG=info,cranelift_codegen=debug` for those lines). The caches live under
`/tmp/function-wasm-cache` (not configurable); back it with a volume through a
`DeploymentRuntimeConfig` to keep them across pod restarts, and mount an
`emptyDir` there if the pod's root filesystem is read-only. A volume shared
between pods is safe. Entries are content-addressed and written atomically,
and artifacts of another Wasmtime version are only removed once nothing has
written them for a day, so a rolling upgrade doesn't thrash.

Opening the sandbox is the same `DeploymentRuntimeConfig`: mount the Cedar
`--sandbox-policy-file`, and a tmpfs behind `TMPDIR` bounds the private `/tmp`:

```yaml
spec:
  deploymentTemplate:
    spec:
      template:
        spec:
          containers:
          - name: package-runtime
            args:
            - --sandbox-policy-file=/etc/function-wasm/policy.cedar
            env:
            - name: TMPDIR
              value: /scratch
            volumeMounts:
            - {name: policy, mountPath: /etc/function-wasm, readOnly: true}
            - {name: scratch, mountPath: /scratch}
          volumes:
          - name: policy
            configMap: {name: function-wasm-policy}
          - name: scratch
            emptyDir: {medium: Memory, sizeLimit: 64Mi}
```

A fuller example with every operator-authorable option is in
[`examples/deployment-runtime-config-cedar.yaml`](https://github.com/jonasz-lasut/function-wasm/blob/main/examples/deployment-runtime-config-cedar.yaml).
