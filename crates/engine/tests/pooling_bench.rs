//! A hand-run benchmark comparing per-request instantiation under
//! wasmtime's on-demand allocator (with its default copy-on-write heap
//! images) against the pooling allocator, over an InstancePre - the
//! evidence behind the wasmtime-adoption decision "pooling: benchmark
//! first, adopt only on a clear win". Not part of the suite:
//!
//!   cargo test -p function-wasm-engine --test pooling_bench --release -- --ignored --nocapture

use std::time::Instant;

use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::p1::WasiP1Ctx;

fn engine(pooling: bool) -> wasmtime::Engine {
    // The runtime's own settings (engine::new), so the comparison is over
    // the configuration the runtime would actually ship.
    let mut c = wasmtime::Config::new();
    c.epoch_interruption(true);
    c.native_unwind_info(false);
    if pooling {
        let mut p = wasmtime::PoolingAllocationConfig::default();
        p.total_memories(128);
        p.total_tables(128);
        p.total_core_instances(128);
        p.max_memory_size(512 << 20);
        c.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(p));
    }
    wasmtime::Engine::new(&c).expect("engine")
}

fn wasi_ctx() -> WasiP1Ctx {
    let mut b = WasiCtxBuilder::new();
    b.args(&["function"]);
    b.build_p1()
}

fn bench(name: &str, pooling: bool, wasm: &[u8]) {
    let e = engine(pooling);
    let m = wasmtime::Module::from_binary(&e, wasm).expect("compile");
    let mut linker: wasmtime::Linker<WasiP1Ctx> = wasmtime::Linker::new(&e);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |c| c).expect("wasi");
    linker
        .func_wrap("wasmfn", "log", |_: i32, _: i32, _: i32| {})
        .expect("log");
    linker
        .func_wrap("wasmfn", "http", |_: i32, _: i32| -> i64 { 0 })
        .expect("http");
    let pre = linker.instantiate_pre(&m).expect("pre");
    for _ in 0..200 {
        let mut s = wasmtime::Store::new(&e, wasi_ctx());
        s.set_epoch_deadline(u64::MAX);
        pre.instantiate(&mut s).expect("instantiate");
    }
    let n = 3000u32;
    let start = Instant::now();
    for _ in 0..n {
        let mut s = wasmtime::Store::new(&e, wasi_ctx());
        s.set_epoch_deadline(u64::MAX);
        pre.instantiate(&mut s).expect("instantiate");
    }
    let per = start.elapsed() / n;
    println!("{name:34} pooling={pooling:5}  {per:>10.2?} / store+instantiate");
}

#[test]
#[ignore = "hand-run benchmark, prints to stdout"]
fn bench_instantiation() {
    let small = wat::parse_str(
        r#"(module
          (memory (export "memory") 4)
          (data (i32.const 1024) "hello")
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
          (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
    )
    .expect("wat");
    // 64 MiB initial memory with a data image - the slice of instantiation
    // pooling is supposed to help.
    let large = wat::parse_str(
        r#"(module
          (memory (export "memory") 1024)
          (data (i32.const 1024) "hello")
          (data (i32.const 66060288) "world")
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
          (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
    )
    .expect("wat");
    for pooling in [false, true] {
        bench("small (4 pages)", pooling, &small);
        bench("large (1024 pages, 64 MiB)", pooling, &large);
    }
    // The repository's real Rust example guest, when built
    // (make -C examples/hello-rust build).
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/hello-rust/fn.wasm"
    );
    match std::fs::read(path) {
        Ok(wasm) => {
            for pooling in [false, true] {
                bench("hello-rust example guest", pooling, &wasm);
            }
        }
        Err(_) => println!("hello-rust guest not built, skipping ({path})"),
    }
}
