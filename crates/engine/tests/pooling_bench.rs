//! A hand-run benchmark comparing per-request instantiation under
//! wasmtime's on-demand allocator (with its default copy-on-write heap
//! images) against the pooling allocator, over an InstancePre - the
//! evidence behind the wasmtime-adoption decision "pooling: benchmark
//! first, adopt only on a clear win". Not part of the suite:
//!
//!   cargo test -p function-wasm-engine --test pooling_bench --release -- --ignored --nocapture

use std::time::Instant;

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
        p.total_component_instances(128);
        p.max_memory_size(512 << 20);
        c.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(p));
    }
    wasmtime::Engine::new(&c).expect("engine")
}

fn bench(name: &str, pooling: bool, wasm: &[u8]) {
    let e = engine(pooling);
    let c = wasmtime::component::Component::from_binary(&e, wasm).expect("compile");
    // The fixtures import nothing, so a bare linker resolves them.
    let linker: wasmtime::component::Linker<()> = wasmtime::component::Linker::new(&e);
    let pre = linker.instantiate_pre(&c).expect("pre");
    for _ in 0..200 {
        let mut s = wasmtime::Store::new(&e, ());
        s.set_epoch_deadline(u64::MAX);
        pre.instantiate(&mut s).expect("instantiate");
    }
    let n = 3000u32;
    let start = Instant::now();
    for _ in 0..n {
        let mut s = wasmtime::Store::new(&e, ());
        s.set_epoch_deadline(u64::MAX);
        pre.instantiate(&mut s).expect("instantiate");
    }
    let per = start.elapsed() / n;
    println!("{name:34} pooling={pooling:5}  {per:>10.2?} / store+instantiate");
}

/// A component implementing the world over a core module with the given
/// memory, in pages, and a data image.
fn component(pages: u32, data: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
  (core module $m
    (memory (export "memory") {pages})
    {data}
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 8)
    (func (export "run") (param i32 i32) (result i32) i32.const 64))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory (core memory $i "memory")) (realloc (core func $i "cabi_realloc"))))
)"#
    ))
    .expect("wat")
}

#[test]
#[ignore = "hand-run benchmark, prints to stdout"]
fn bench_instantiation() {
    let small = component(4, r#"(data (i32.const 1024) "hello")"#);
    // 64 MiB initial memory with a data image - the slice of instantiation
    // pooling is supposed to help.
    let large = component(
        1024,
        r#"(data (i32.const 1024) "hello") (data (i32.const 66060288) "world")"#,
    );
    for pooling in [false, true] {
        bench("small (4 pages)", pooling, &small);
        bench("large (1024 pages, 64 MiB)", pooling, &large);
    }
}
