# Sizing

What a request and a module cost depends almost entirely on the guest's
toolchain - the host adds about 60 µs. A historical measurement
(2026-08-16, linux/arm64, the ABI v1 guests of the time: a
`function-sdk-go` Go guest, a raw-proto Go guest using only the vendored
glue, TinyGo, Rust), kept as the order of magnitude per toolchain: the
TinyGo flavour has since been retired and the other guests are ABI v2
components (the Go module's size and compile cost did not move; the Rust
scaffold is now ~240 KB):

| | Go (~75 MB) | Go, raw proto (~20 MB) | TinyGo (~1.4 MB, retired) | Rust (~150 KB) |
|---|---|---|---|---|
| one request (CPU) | 8–11 ms, 91 % of it the Go runtime's own init | 1.2 ms | 0.4 ms | 0.05 ms |
| first compile of a module | 23–28 CPU-seconds, ~1 GB peak | 6 CPU-s | 1 CPU-s | 0.1 CPU-s |
| resident in memory | ~90 MB, file-backed | ~40 MB | 3.5 MB | 0.7 MB |
| per in-flight run | 11–16 MB | 4–8 MB | < 1 MB | < 1 MB |
| on disk per module version | ~230 MB (module + artifact) | ~60 MB | 5 MB | 0.9 MB |
| load from a warm volume | 6–8 ms | 2 ms | 0.2 ms | 0.05 ms |

Rules of thumb: memory ≈ resident modules + `--max-concurrent-compiles` × 1 GB
+ concurrent runs × 16 MB (`--max-concurrent-runs` caps that last term when
the caller's concurrency is not the number you want to size for); a cold
start compiles every module once, so ten Go modules on two cores take about
two minutes and a warm volume under `/tmp/function-wasm-cache` turns that
into a second, `--warm-modules` moves either ahead of readiness; disk grows
by one module + artifact per digest ever served unless `--max-cache-size`
bounds it. Requests scale linearly with cores — nothing in the run path is
serialised unless `--max-concurrent-runs` says so. Large observed state
costs on every request regardless of the guest (about 20 ms per MB of
composite resource, four protobuf passes). The full model of bounds and
budgets is
[docs/one-pager-resource-governance.md](../one-pager-resource-governance.md).
