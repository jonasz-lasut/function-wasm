//! A hand-run benchmark comparing whole runs under wasmtime's on-demand
//! allocator (with its default copy-on-write heap images), which the
//! runtime uses, against the pooling allocator, at 1, 4 and 16 threads.
//! Not part of the suite:
//!
//!   cargo test -p guestfn-engine-wasm --test pooling_bench --release -- --ignored --nocapture
//!
//! GUESTFN_BENCH_GUESTS adds built guests to the table, as
//! `name=path/fn.wasm` pairs separated by commas.
//!
//! Measured on 2026-11-13 with wasmtime 50.0.1, in runs per second over
//! every thread together, on-demand then pooling:
//!
//! | guest, threads    | macOS arm64, 15 cores | Linux x86_64, 4 vCPU | Linux arm64, 4 vCPU |
//! |-------------------|-----------------------|----------------------|---------------------|
//! | small fixture, 1  | 56k, 117k             | 20k, 45k             | 18k, 62k            |
//! | small fixture, 4  | 55k, 201k             | 19k, 113k            | 12k, 121k           |
//! | small fixture, 16 | 21k, 191k             | 18k, 113k            | 12k, 122k           |
//! | e2e guest, 1      | 30k, 35k              | 9.4k, 11.8k          | 7.8k, 13.0k         |
//! | e2e guest, 4      | 51k, 88k              | 12.3k, 24.8k         | 8.6k, 34.6k         |
//! | e2e guest, 16     | 22k, 96k              | 9.6k, 24.7k          | 7.6k, 33.8k         |
//! | Go example, 1     | 106, 107              | 39, 38               | 39, 39              |
//! | Go example, 4     | 359, 351              | 90, 88               | 142, 146            |
//! | Go example, 16    | 910, 912              | 89, 87               | 151, 148            |
//!
//! On-demand allocation maps and unmaps a linear memory and a fiber stack
//! per run, which the kernel serialises, so the throughput of a small
//! guest does not grow with threads; pooling reuses its slots. A guest
//! whose own work is milliseconds (Go everywhere, the Python and
//! TypeScript examples on macOS) gains nothing. The runtime keeps the
//! on-demand allocator: a pool has a fixed number of slots and fixed
//! limits per slot, a run beyond either fails where today it runs, and
//! each memory slot reserves the address space of a whole linear memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use function_sdk_rust::proto::v1::{Resource, RunFunctionRequest, State};
use guestfn_engine::Engine as _;
use guestfn_engine::{Config, RunOptions};
use guestfn_engine_wasm::Wasmtime;
use prost::Message as _;

const THREADS: [usize; 3] = [1, 4, 16];
const SLOTS: u32 = 64;
const WARM_UP: Duration = Duration::from_millis(300);
const MEASURED: Duration = Duration::from_secs(2);

fn engine(pooling: bool) -> Wasmtime {
    if pooling {
        Wasmtime::pooled(Config::default(), SLOTS).expect("engine")
    } else {
        Wasmtime::new(Config::default()).expect("engine")
    }
}

/// A component implementing the world over a core module with the given
/// memory, in pages, and a data image.
fn component(pages: u32, data: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
  (core module $m
    (memory (export "memory") {pages})
    {data}
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 4096)
    (func (export "run") (param i32 i32) (result i32) i32.const 64))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory (core memory $i "memory")) (realloc (core func $i "cabi_realloc"))))
)"#
    ))
    .expect("wat")
}

/// A request a real guest can decode: one observed composite resource.
fn request() -> Vec<u8> {
    let xr = serde_json::json!({
        "apiVersion": "e2e.guestfn.io/v1alpha1",
        "kind": "Guest",
        "metadata": {"name": "probe", "namespace": "e2e"},
        "spec": {"nopResources": 0},
    });
    RunFunctionRequest {
        observed: Some(State {
            composite: Some(Resource {
                resource: Some(serde_json::from_value(xr).expect("a Struct")),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

/// Runs per second over every thread together, and how a run ended when
/// the first one failed.
fn throughput(pooling: bool, wasm: &[u8], threads: usize) -> (f64, Option<String>) {
    let engine = Arc::new(engine(pooling));
    let module = engine.compile(wasm).expect("compile");
    let request = Arc::new(request());
    let failure = engine
        .run(&module, &request, RunOptions::default())
        .err()
        .map(|e| e.to_string());
    let counting = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let runs = Arc::new(AtomicU64::new(0));
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let (engine, module, request) =
                (Arc::clone(&engine), module.clone(), Arc::clone(&request));
            let (counting, stop, runs) =
                (Arc::clone(&counting), Arc::clone(&stop), Arc::clone(&runs));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = engine.run(&module, &request, RunOptions::default());
                    if counting.load(Ordering::Relaxed) {
                        runs.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();
    std::thread::sleep(WARM_UP);
    counting.store(true, Ordering::Relaxed);
    let start = Instant::now();
    std::thread::sleep(MEASURED);
    let n = runs.load(Ordering::Relaxed);
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Relaxed);
    for w in workers {
        w.join().expect("worker");
    }
    (n as f64 / elapsed.as_secs_f64(), failure)
}

#[test]
#[ignore = "hand-run benchmark, prints to stdout"]
fn bench_runs() {
    let mut guests = vec![
        (
            "small (4 pages)".to_string(),
            component(4, r#"(data (i32.const 1024) "hello")"#),
        ),
        // 64 MiB initial memory with a data image - the slice of
        // instantiation pooling is supposed to help.
        (
            "large (1024 pages, 64 MiB)".to_string(),
            component(
                1024,
                r#"(data (i32.const 1024) "hello") (data (i32.const 66060288) "world")"#,
            ),
        ),
        (
            "e2e guest (Rust)".to_string(),
            include_bytes!("../../e2e/guest/fn.wasm").to_vec(),
        ),
    ];
    for pair in std::env::var("GUESTFN_BENCH_GUESTS")
        .unwrap_or_default()
        .split(',')
        .filter(|p| !p.is_empty())
    {
        let (name, path) = pair.split_once('=').expect("name=path");
        guests.push((name.to_string(), std::fs::read(path).expect("guest")));
    }
    println!(
        "cores: {}",
        std::thread::available_parallelism().map_or(0, usize::from)
    );
    println!(
        "| guest | threads | on-demand runs/s | on-demand µs/run | pooling runs/s | pooling µs/run |"
    );
    println!("|---|---|---|---|---|---|");
    for (name, wasm) in &guests {
        for threads in THREADS {
            let (on_demand, failure) = throughput(false, wasm, threads);
            let (pooled, pooled_failure) = throughput(true, wasm, threads);
            let per_run = |rate: f64| threads as f64 * 1e6 / rate;
            println!(
                "| {name} | {threads} | {on_demand:.0} | {:.0} | {pooled:.0} | {:.0} |",
                per_run(on_demand),
                per_run(pooled)
            );
            for (allocator, failure) in [("on-demand", failure), ("pooling", pooled_failure)] {
                if let Some(failure) = failure {
                    println!("  {allocator} run of {name} failed: {failure}");
                }
            }
        }
    }
}
