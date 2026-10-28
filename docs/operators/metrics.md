# Metrics

This page is for operators building dashboards and alerts on the runtime. It
lists every series served on `--metrics-address` and the formats it speaks.
The endpoint is `:8080/metrics` by default, and an empty value disables it.
Probes are on [Readiness and warm-up](readiness.md).

| metric | labels | meaning |
|---|---|---|
| `function_wasm_module_requests_total` | `outcome` = `ok`, `refused`, `error`, `skipped` | requests by outcome |
| `function_wasm_module_run_duration_seconds` | `outcome` = `ok`, `error`, `timeout` | histogram of one guest run, instantiate to response; a wait for a run slot isn't part of it, and a request cut short while waiting for one isn't counted |
| `function_wasm_module_runs_in_flight` | | gauge of guest runs executing now, at most `--max-concurrent-runs`; a gauge held at that value means requests are queuing for a slot |
| `function_wasm_module_hostcall_duration_seconds` | | histogram of the time a run spent inside host imports (the `log` import, HTTP egress over `wasi:http`, and WASI); the rest of `run_duration_seconds` is guest compute |
| `function_wasm_module_compile_duration_seconds` | | histogram of Wasmtime compile time on compiled-cache misses |
| `function_wasm_module_fetch_duration_seconds` | `source` = `oci`, `http`, `path` | histogram of fetch and verify time, blob-cache hits and served-file reads included |
| `function_wasm_module_cache_events_total` | `cache` = `compiled` (memory), `compiled-disk`, `blob`; `event` = `hit`, `miss`, `stale` | cache lookups; `stale` is an artifact on disk that Wasmtime refused, and appears for `compiled-disk` only |
| `function_wasm_module_cache_bytes` | `cache` = `compiled-disk`, `blob` | bytes on disk per store, measured every ten minutes |
| `function_wasm_module_http_requests_total` | `outcome` = `ok`, `refused`, `budget`, `error` | HTTP requests modules made through the host (`requires.egress.http`) |
| `function_wasm_module_memory_denials_total` | `reason` = `limit`, `pool` | guest memory growths denied; the guest sees `memory.grow` fail |
| `function_wasm_trusted_root_refreshes_total` | `outcome` = `updated`, `unchanged`, `error` | refreshes of the Sigstore trusted root |
| `function_wasm_trusted_root_last_refresh_timestamp_seconds` | | Unix time of the last successful refresh of the trusted root, 0 until the first; `time()` minus this value is the root's age |

The `requests_total` outcomes:

- `ok`: the module ran and returned a response.
- `refused`: the runtime declined the request before the run (input, policy, grants, limits, resolution or verification), or refused the module's output at the `composeResource` fence. In the second case the module ran, so the run is in `run_duration_seconds` too.
- `error`: the load or the run failed.
- `skipped`: the composite resource chose no module and `module.allowEmpty` allowed it, so nothing ran (logged as `No module chosen by the composite resource`).

The `http_requests_total` outcomes:

- `ok`: the server answered.
- `refused`: the grant or the egress policy refused the request ([HTTP egress](http-egress.md)).
- `budget`: a per-run budget or the timeout was hit.
- `error`: the request failed.

The `memory_denials_total` reasons:

- `limit`: the growth passed the run's ceiling (`limits.memory` or `--module-memory-limit`).
- `pool`: `--max-total-run-memory` couldn't serve the growth before the run's deadline.

The trusted-root series exist only with `--cosign-trust-instance` ([signatures](signatures.md)), and every outcome starts at zero. The `refreshes_total` outcomes:

- `updated`: a root was put in use, the first one or one that differs from the last. The verified-digest memo is forgotten with it.
- `unchanged`: the same root was fetched again.
- `error`: the fetch failed and the last good root stays. The log has the reason.

Alert on the root's age growing past a few refresh intervals (`--cosign-tuf-refresh`), and on the `error` outcome moving.

No metric has a module or host label, because the sets of digests and hosts a Function serves are unbounded. The logs include the module reference and digest, and the egress audit line states the host.

## Formats

The main format is [OpenMetrics](https://prometheus.io/docs/specs/om/open_metrics_spec/)
1.0 (`application/openmetrics-text; version=1.0.0`). A scraper whose `Accept`
header asks for the classic Prometheus text format without accepting
OpenMetrics gets `text/plain; version=0.0.4`. Both contain the same series.

## gRPC server metrics

The runtime serves the gRPC server series of the Prometheus gRPC interceptor, with the names and labels function-sdk-go uses:

- `grpc_server_started_total`
- `grpc_server_handled_total`, with a `grpc_code` label holding the gRPC code name
- `grpc_server_msg_received_total`
- `grpc_server_msg_sent_total`

Each is labelled `grpc_type`, `grpc_service` and `grpc_method`.

Only unary calls (`RunFunction`, health `Check`) are counted. Streaming methods (reflection, health `Watch`) exist from startup, at zero until the first call.

For latency, use `function_wasm_module_run_duration_seconds`.
