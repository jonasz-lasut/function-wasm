# Metrics

The runtime serves metrics where function-sdk-go puts them
(`:8080/metrics`), next to the [gRPC server series](#grpc-server-metrics).
The main exposition format is [OpenMetrics](https://prometheus.io/docs/specs/om/open_metrics_spec/)
1.0 (`application/openmetrics-text; version=1.0.0`), readable by any
OpenMetrics-capable collector, not only Prometheus; a scraper whose
`Accept` header asks for the classic Prometheus text format without
accepting OpenMetrics gets `text/plain; version=0.0.4` instead. The two
renderings carry identical series - same names, labels and values - so the
format never changes what a dashboard sees:

| metric | labels | meaning |
|---|---|---|
| `function_wasm_module_compile_duration_seconds` | | histogram of wasmtime compile time (compiled-cache misses) |
| `function_wasm_module_fetch_duration_seconds` | `source` = oci, http, path | histogram of fetch + verify time (blob-cache hits and served-file reads included) |
| `function_wasm_module_requests_total` | `outcome` = ok, refused, error, skipped | requests by outcome: refused = declined before the module ran (input, policy, grants, limits, resolution, verification — each also logged as `Request ended with a fatal result` with the reason), error = the load or the run failed, skipped = the composite resource chose no module and `module.allowEmpty` allowed it, so nothing ran (logged as `No module chosen by the composite resource`) |
| `function_wasm_module_run_duration_seconds` | `outcome` = ok, error, timeout | histogram of one guest run, instantiate to response (a wait for a run slot is not part of it, and a request that never got one is not counted) |
| `function_wasm_module_runs_in_flight` | | gauge of guest runs executing right now; pinned at `--max-concurrent-runs`, the bound is what requests wait on |
| `function_wasm_module_cache_events_total` | `cache` = compiled (memory), compiled-disk, blob; `event` = hit, miss, stale (compiled-disk only: an artifact wasmtime refused) | cache lookups |
| `function_wasm_module_cache_bytes` | `cache` = compiled-disk, blob | bytes on disk per store, measured every ten minutes |
| `function_wasm_module_http_requests_total` | `outcome` = ok, refused, budget, error | HTTP requests modules made through the host (`sandbox.egress`): the server answered; refused by the grant or the egress policy; a per-run budget or the timeout was hit; the request failed. No host label — the audit log line names it |
| `function_wasm_module_hostcall_duration_seconds` | | histogram of the slice of a run spent inside host imports (the `log` import, HTTP egress over `wasi:http`, and WASI); the rest of `run_duration_seconds` is guest compute. A run that is slow here is waiting on the host - usually an upstream the module's HTTP requests talk to |
| `function_wasm_module_memory_denials_total` | `reason` = limit, pool | guest memory growths denied - at the run's ceiling (`limits.memory` or `--module-memory-limit`) or because `--max-total-run-memory` could not serve the growth before the run's deadline. The guest sees `memory.grow` fail |

No metric carries a module identity: the set of digests a Function serves is
unbounded. Logs carry the module reference and digest.

## gRPC server metrics

The transport also serves the gRPC server series the Go runtime got from
function-sdk-go's grpc-prometheus interceptor, with the same names, labels
and meanings - dashboards and alerts built on them keep working:
`grpc_server_started_total`, `grpc_server_handled_total` (with a
`grpc_code` label carrying the gRPC code name, `OK` … `Unauthenticated`),
`grpc_server_msg_received_total` and `grpc_server_msg_sent_total`, each
labelled `grpc_type`/`grpc_service`/`grpc_method`. As under the Go
runtime - whose interceptor was unary-only - unary calls (`RunFunction`,
health `Check`) are counted and streaming methods (reflection, health
`Watch`) exist as permanently zero series; every method the server carries
is pre-created at zero on startup, so a scrape sees the full set before
the first request. There is no `grpc_server_handling_seconds`:
function-sdk-go never enabled the histogram, and
`function_wasm_module_run_duration_seconds` covers latency.
