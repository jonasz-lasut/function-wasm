# Runtime flags

This page is for operators who run the function-wasm runtime from a Crossplane package and set its flags through a `DeploymentRuntimeConfig`. It lists each flag with its environment variable and default, one line each. The linked pages have the detail.

The binary has two subcommands, `serve` and [`validate`](../guests/validate.md). `serve` is the default, so `function --insecure --module-dir=.` and a `DeploymentRuntimeConfig`'s `args` don't name it. `validate` takes the flags marked "yes" in the `validate` column below, with the same defaults and environment variables, so it checks a Composition against what a runtime started with those flags would admit.

## Limits and sources

These flags set the ceilings that a step's `limits` can't exceed ([Input reference](input-reference.md)) and where `path` sources come from.

| flag | env | default | `validate` | purpose |
|---|---|---|---|---|
| `--module-dir` | `MODULE_DIR` | unset | yes | directory served for `path` sources; unset refuses them |
| `--max-module-size` | | `128 MB` | yes | largest module accepted |
| `--module-timeout` | | `30s` | yes | wall-clock budget of one run, and the ceiling for `limits.timeout` |
| `--module-memory-limit` | | `512 MB` | yes | memory one run may use: its linear memories and tables (a host pointer per element) together, the initial memory included; also the ceiling for `limits.memory` |
| `--module-stack-limit` | `MODULE_STACK_LIMIT` | `512 KB` | no | call stack one run may use. Past it the run fails with `trap: call stack exhausted`. No Input field narrows it |
| `--ttl` | | `60s` | no | TTL of responses the runtime itself produces (fatal results); a module sets its own |

## Concurrency

By default nothing is bounded: concurrency is the caller's, set by Crossplane's reconcile workers.

| flag | env | default | `validate` | purpose |
|---|---|---|---|---|
| `--max-concurrent-runs` | `MAX_CONCURRENT_RUNS` | `0` (unbounded) | no | module runs executing at once; a request that can't get a slot before its deadline is a fatal result (`waiting for a run slot: deadline exceeded`) and never ran |
| `--max-total-run-memory` | `MAX_TOTAL_RUN_MEMORY` | `0` (unbounded) | no | total memory budget in MB across running modules, linear memories and tables together; see below |
| `--max-concurrent-compiles` | `MAX_CONCURRENT_COMPILES` | `1` | no | modules compiled at once; a compile is heavy on CPU and memory, so further first requests wait their turn |
| `--module-compile-timeout` | `MODULE_COMPILE_TIMEOUT` | `0s` (unbounded) | no | wall-clock bound on a cold load, the fetch and compile of a module that isn't cached yet. Past it the request fails with `cannot load module …: compile exceeded --module-compile-timeout <duration>`; the compile runs to its end, keeps its compile slot until then and stores its artifact for the next request. The syntax of `--module-timeout` |

A component reserves nothing up front. The runtime charges each memory or table growth, the initial memory included, to the `--max-total-run-memory` pool as the guest claims it, and a run waits under its deadline while the pool is full. A growth the pool can't serve before the deadline fails inside the guest as a failed `memory.grow`, counted in `function_wasm_module_memory_denials_total`. See [Sizing](sizing.md) for what these bounds cost.

## Caches

Compiled modules are cached in memory, and fetched modules and compiled artifacts on disk. See [Cache storage](#cache-storage) for the directory.

| flag | env | default | `validate` | purpose |
|---|---|---|---|---|
| `--enable-memory-cache` | `ENABLE_MEMORY_CACHE` | `true` | no | keep compiled modules in memory between requests; the flag takes a value, and with `--enable-memory-cache=false` each request maps the compiled artifact from disk and releases it afterward |
| `--max-cached-modules` | `MAX_CACHED_MODULES` | `0` (unbounded) | no | most compiled modules resident at once; past it the least recently used is dropped, freed once its last run ends |
| `--max-cache-size` | `MAX_CACHE_SIZE` | `0` (unbounded) | no | MB the two on-disk caches may hold together; past it the runtime removes the least recently used entries, at startup and every ten minutes. Size the volume, or set this below its size |
| `--warm-modules` | `WARM_MODULES` | unset | no | modules loaded before readiness; see [Readiness and warm-up](readiness.md) |

## Signing

These flags set which module signatures the runtime accepts. [Signatures](signatures.md) explains each mode and how they combine.

| flag | env | default | `validate` | purpose |
|---|---|---|---|---|
| `--cosign-key` | `COSIGN_KEY` | unset | yes | PEM file of cosign public keys for key-based cosign 3 signatures |
| `--cosign-trusted-root` | `COSIGN_TRUSTED_ROOT` | unset | yes | Sigstore `trusted_root.json` that verifies keyless signatures offline; excludes `--cosign-trust-instance` |
| `--cosign-trust-instance` | `COSIGN_TRUST_INSTANCE` | unset | yes | `public-good`, `staging` or `github`: fetch that instance's trusted root over TUF and refresh it while serving |
| `--cosign-tuf-refresh` | `COSIGN_TUF_REFRESH` | `1h` | no | how often `--cosign-trust-instance` refreshes the root, at least `1m` |

## Policy and egress

See [Operator grant policy](grant-policy.md) for writing and mounting the policy and [HTTP egress](http-egress.md) for the egress budgets.

| flag | env | default | `validate` | purpose |
|---|---|---|---|---|
| `--sandbox-policy-file` | `SANDBOX_POLICY_FILE` | unset | yes | [Cedar](https://www.cedarpolicy.com) document holding the operator's grant policy; unset, every sandbox capability is refused. Compiled once at startup: restart to reload |
| `--egress-rate-limit-per-minute` | `EGRESS_RATE_LIMIT_PER_MINUTE` | `0` (off) | yes | sustained egress requests per minute per module digest, as a process-wide token bucket |
| `--egress-rate-limit-burst` | `EGRESS_RATE_LIMIT_BURST` | `0` (derived) | yes | burst tokens for the rate limit; `0` derives `max(1, requestsPerMinute)` |

## Health and metrics

Both endpoints are plain HTTP, because the function port speaks mTLS. [Readiness and warm-up](readiness.md) covers wiring probes.

| flag | env | default | `validate` | purpose |
|---|---|---|---|---|
| `--health-address` | `HEALTH_ADDRESS` | `:8081` | no | `/livez` and `/readyz` (503 while warming); empty disables them |
| `--metrics-address` | `METRICS_ADDRESS` | `:8080` | no | Prometheus `/metrics` ([Metrics](metrics.md)); empty disables it |

The SDK's flags `--insecure`, `--debug`, `--tls-certs-dir`, `--address` and `--max-recv-message-size` apply too.

## Logging

`--debug` turns on the runtime's own debug lines in a human-readable format: guest log records sent at debug level, the credentials withheld from a module and trap details. Everything else logs at `info`, so a compile under `--debug` doesn't print the compiler's per-pass lines. A set `RUST_LOG` replaces the flag's filter in full.

## Cache storage

The caches live under `/tmp/function-wasm-cache`, which isn't configurable. Mount an `emptyDir` there if the pod's root filesystem is read-only, or a volume to keep the fetched modules and manifests across pod restarts:

```yaml
spec:
  deploymentTemplate:
    spec:
      template:
        spec:
          containers:
          - name: package-runtime
            volumeMounts:
            - name: cache
              mountPath: /tmp/function-wasm-cache
          volumes:
          - name: cache
            persistentVolumeClaim:
              claimName: function-wasm-cache
```

A volume keeps the fetched modules and the manifests, which the runtime verifies against their digest on every read. It doesn't keep the compiled code. Each process signs the artifacts it writes under a key drawn at startup and refuses every other ([trust model](trust-model.md#the-cache-directory)). A restarted pod compiles its modules again. Pods must not share a volume, because each would refuse and overwrite the other's artifacts. Give each pod its own volume, with the runtime as its only writer. The runtime removes the compiled directories of other Wasmtime versions after a day without writes.

## Opening the sandbox

Mount the Cedar `--sandbox-policy-file` through the same `DeploymentRuntimeConfig`, and put a tmpfs behind `TMPDIR` to bound the private `/tmp`:

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

A fuller example with every operator-authorable option is [`examples/deployment-runtime-config-cedar.yaml`](https://github.com/jonasz-lasut/function-wasm/blob/main/examples/deployment-runtime-config-cedar.yaml).
