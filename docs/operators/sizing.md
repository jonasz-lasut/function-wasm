# Sizing

This page is for operators choosing CPU, memory and disk for a function-wasm pod. It gives the formulas first and the measurements they rest on after. Those are the module sizes and costs, by guest toolchain, that the formulas take as inputs.

What a request and a module cost depends on the guest's toolchain, and the Measurements section gives the per-toolchain figures the formulas use.

## Memory

Budget a pod's memory as the sum of three terms:

- the resident compiled modules;
- `--max-concurrent-compiles` times the peak memory of compiling your largest module (about 1 GB for a large Go module, far less for Rust, per Measurements);
- concurrent runs times the per-run figure (about 16 MB for a large Go module, per Measurements).

`--max-concurrent-runs` caps the third term, which is useful when the callers' concurrency isn't the number you want to size for.

## Cold start

A cold start compiles every module once. `--max-module-size` bounds a
compile's input, `--max-concurrent-compiles` how many run at once (each
about 1 GB at peak for a large Go module, per Measurements) and
`--module-compile-timeout` how long one may take. The request's own deadline
doesn't cut a compile short, so leave `--max-concurrent-compiles` times that
peak in the pod's memory limit. A volume under `/tmp/function-wasm-cache` that survives restarts turns that compile into a load of the stored artifact. `--warm-modules` moves either ahead of readiness, as [readiness](readiness.md) describes.

## Disk

Disk grows by one module and one compiled artifact per digest ever served, until `--max-cache-size` bounds it.

## Throughput

Runs execute in parallel up to `--max-concurrent-runs` and a step's `limits.concurrency`. The `--max-total-run-memory` pool bounds how many can hold their memory at once, as [runtime flags](runtime-flags.md) describes. A large observed state costs on every request whatever the guest is.

## Measurements

The figures below were measured on 2026-08-16 on linux/arm64, on the Go host with ABI v1 guests, and weren't re-measured on the Rust host, so treat them as a rough scale.

| | Go (~75 MB) | Go, raw proto (~20 MB) | Rust (~240 KB) |
|---|---|---|---|
| one request (CPU) | 8-11 ms | 1.2 ms | 0.05 ms |
| first compile of a module | 23-28 CPU-seconds, ~1 GB peak | 6 CPU-s | 0.1 CPU-s |
| resident in memory | ~90 MB, file-backed | ~40 MB | 0.7 MB |
| per in-flight run | 11-16 MB | 4-8 MB | < 1 MB |
| on disk per module version | ~230 MB (module + artifact) | ~60 MB | 0.9 MB |
| load from a warm volume | 6-8 ms | 2 ms | 0.05 ms |

"Go" is the go scaffold and "Go, raw proto" a Go guest that uses only the scaffold's own code, without `function-sdk-go`. The full model of bounds and budgets is in [the resource governance one-pager](../one-pager-resource-governance.md).
