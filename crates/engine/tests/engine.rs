//! Engine tests over WAT fixtures implementing ABI v1, the way the Go
//! engine's tests use WAT fixtures in the spirit of the old Go tree's
//! internal/testwasm: a module that returns fixed
//! response bytes, modules that misbehave in one way each, and the ABI
//! shape refusals.

use std::fmt::Write as _;
use std::time::Duration;

use function_sdk_rust::proto::v1::{Result as FnResult, RunFunctionResponse, Severity, Target};
use function_wasm_engine::{Config, Engine, RunOptions};
use prost::Message;

/// Escapes bytes for a WAT data-segment string.
fn wat_bytes(b: &[u8]) -> String {
    let mut out = String::new();
    for byte in b {
        let _ = write!(out, "\\{byte:02x}");
    }
    out
}

const BUMP_ALLOC: &str = r#"
  (global $next (mut i32) (i32.const 131072))
  (func (export "wasmfn_alloc") (param i32) (result i32)
    (local $ptr i32)
    global.get $next
    local.tee $ptr
    local.get 0
    i32.add
    global.set $next
    local.get $ptr)
"#;

/// A module whose wasmfn_run returns the given bytes, stored at offset 1024.
fn fixed(rsp: &[u8]) -> String {
    let packed = (1024u64 << 32) | rsp.len() as u64;
    format!(
        r#"(module
  (memory (export "memory") 4)
  (data (i32.const 1024) "{data}")
  {BUMP_ALLOC}
  (func (export "wasmfn_run") (param i32 i32) (result i64)
    i64.const {packed}))"#,
        data = wat_bytes(rsp),
        packed = packed as i64,
    )
}

fn engine() -> Engine {
    Engine::new(Config::default()).expect("engine")
}

fn response_bytes() -> Vec<u8> {
    RunFunctionResponse {
        results: vec![FnResult {
            severity: Severity::Normal as i32,
            message: "ok".to_string(),
            reason: None,
            target: Some(Target::Composite as i32),
        }],
        ..Default::default()
    }
    .encode_to_vec()
}

#[test]
fn fixed_response_round_trip() {
    let e = engine();
    let rsp = response_bytes();
    let m = e
        .compile(&wat::parse_str(fixed(&rsp)).expect("wat"))
        .expect("compile");
    let out = e.run(&m, b"anything", RunOptions::default()).expect("run");
    assert_eq!(out, rsp);
}

#[test]
fn check_abi_refusals() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "NoRun",
            r#"(module (memory (export "memory") 1)
              (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8))"#,
            r#"module does not export "wasmfn_run""#,
        ),
        (
            "NoMemory",
            r#"(module
              (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
              (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
            r#"module does not export a memory named "memory""#,
        ),
        (
            "WrongAllocSignature",
            r#"(module (memory (export "memory") 1)
              (func (export "wasmfn_alloc") (param i32 i32) (result i32) i32.const 8)
              (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
            r#"export "wasmfn_alloc" has signature (i32, i32) -> (i32), ABI v1 requires (i32) -> (i32)"#,
        ),
        (
            "ForeignImport",
            r#"(module (import "env" "foo" (func))
              (memory (export "memory") 1)
              (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
              (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
            "module imports env.foo, which the host does not provide",
        ),
        (
            "WrongHTTPImportType",
            r#"(module (import "wasmfn" "http" (func (param i32)))
              (memory (export "memory") 1)
              (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
              (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
            "module imports wasmfn.http with the wrong type, ABI v1 requires (i32, i32) -> (i64)",
        ),
    ];
    let e = engine();
    for (name, wat, want) in cases {
        let err = e
            .compile(&wat::parse_str(wat).expect("wat"))
            .err()
            .unwrap_or_else(|| panic!("{name}: compile should fail"));
        assert_eq!(&err.to_string(), want, "{name}");
    }
}

#[test]
fn a_run_observes_the_hostcall_split() {
    let e = engine();
    let rsp = response_bytes();
    let m = e
        .compile(&wat::parse_str(fixed(&rsp)).expect("wat"))
        .expect("compile");
    let name = "function_wasm_module_hostcall_duration_seconds";
    let before = function_wasm_engine::metrics::sample(name, &[]).unwrap_or(0.0);
    e.run(&m, b"", RunOptions::default()).expect("run");
    // Other tests' runs share the process-global registry, so the count is
    // only monotonic: this run added at least its own observation.
    let after = function_wasm_engine::metrics::sample(name, &[]).expect("series registered");
    assert!(after >= before + 1.0, "the run observes the split");
}

#[test]
fn the_stack_limit_bounds_recursion() {
    // 4000 frames fit in the default 512 KiB stack and overflow an 8 KiB
    // one; the trap carries the Go runtime's wording.
    let wat = r#"(module (memory (export "memory") 1)
      (func $rec (param i32) (result i64)
        local.get 0
        i32.eqz
        (if (result i64)
          (then i64.const 0)
          (else local.get 0 i32.const 1 i32.sub call $rec)))
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64)
        i32.const 4000
        call $rec))"#;
    let wasm = wat::parse_str(wat).expect("wat");

    let e = engine();
    let m = e.compile(&wasm).expect("compile");
    e.run(&m, b"", RunOptions::default())
        .expect("fits in the default stack");

    let small = Engine::new(Config {
        stack_limit: 8 << 10,
        ..Config::default()
    })
    .expect("engine");
    let m = small.compile(&wasm).expect("compile");
    let err = small
        .run(&m, b"", RunOptions::default())
        .expect_err("should overflow");
    assert_eq!(
        err.to_string(),
        "wasmfn_run failed: trap: call stack exhausted"
    );
}

#[test]
fn a_compiled_artifact_is_refused_by_name() {
    let e = engine();
    let rsp = response_bytes();
    let m = e
        .compile(&wat::parse_str(fixed(&rsp)).expect("wat"))
        .expect("compile");
    let artifact = e.serialize(&m).expect("serialize");
    let err = e
        .compile(&artifact)
        .expect_err("should refuse the artifact");
    assert_eq!(
        err.to_string(),
        "module is a wasmtime compiled artifact (.cwasm), not a wasm module"
    );
}

#[test]
fn run_deadline() {
    let e = engine();
    let wat = r#"(module (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64)
        (loop $l br $l)
        i64.const 0))"#;
    let m = e
        .compile(&wat::parse_str(wat).expect("wat"))
        .expect("compile");
    let opts = RunOptions {
        timeout: Some(Duration::from_millis(50)),
        ..Default::default()
    };
    let err = e.run(&m, b"", opts).expect_err("should time out");
    assert_eq!(
        err.to_string(),
        "wasmfn_run failed: module exceeded its execution deadline (50ms)"
    );
}

#[test]
fn run_exit_status() {
    let e = engine();
    let wat = r#"(module
      (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
      (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64)
        i32.const 7
        call $exit
        i64.const 0))"#;
    let m = e
        .compile(&wat::parse_str(wat).expect("wat"))
        .expect("compile");
    let err = e
        .run(&m, b"", RunOptions::default())
        .expect_err("should exit");
    assert_eq!(
        err.to_string(),
        "wasmfn_run failed: module exited with status 7"
    );
}

#[test]
fn run_invalid_response_buffer() {
    let e = engine();
    let packed = (1_000_000u64 << 32) | 10;
    let wat = format!(
        r#"(module (memory (export "memory") 1)
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
          (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const {}))"#,
        packed as i64
    );
    let m = e
        .compile(&wat::parse_str(wat).expect("wat"))
        .expect("compile");
    let err = e
        .run(&m, b"", RunOptions::default())
        .expect_err("should refuse the buffer");
    assert!(
        err.to_string()
            .starts_with("wasmfn_run returned an invalid response buffer:"),
        "unexpected error: {err}"
    );
}

#[test]
fn http_without_grant_is_refused_in_band() {
    let e = engine();
    let request = br#"{"url":"http://example.com/x"}"#;
    let wat = format!(
        r#"(module
  (import "wasmfn" "http" (func $http (param i32 i32) (result i64)))
  (memory (export "memory") 4)
  (data (i32.const 1024) "{data}")
  {BUMP_ALLOC}
  (func (export "wasmfn_run") (param i32 i32) (result i64)
    i32.const 1024
    i32.const {len}
    call $http))"#,
        data = wat_bytes(request),
        len = request.len(),
    );
    let m = e
        .compile(&wat::parse_str(wat).expect("wat"))
        .expect("compile");
    let out = e.run(&m, b"", RunOptions::default()).expect("run");
    assert_eq!(
        String::from_utf8_lossy(&out),
        r#"{"status":0,"error":"sandbox.egress: HTTP egress is not granted to this module: its manifest requires no egress (requires.egress.http)"}"#
    );
}

/// A guest that grows toward the exported response: empty means the grow
/// was denied (memory.grow returned -1), one byte means it succeeded.
fn grow_guest(pages: u32) -> String {
    format!(
        r#"(module (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64)
        (i32.eq (memory.grow (i32.const {pages})) (i32.const -1))
        (if (result i64)
          (then i64.const {denied})
          (else i64.const {grown}))))"#,
        denied = (1024u64 << 32) as i64,
        grown = ((1024u64 << 32) | 1) as i64,
    )
}

/// The per-run memory ceiling denies growth: the guest sees memory.grow
/// fail rather than the run trapping.
#[test]
fn the_memory_limit_denies_growth() {
    let e = Engine::new(Config {
        memory_limit: 2 << 16, // two pages
        ..Config::default()
    })
    .expect("engine");
    let m = e
        .compile(&wat::parse_str(grow_guest(4)).expect("wat"))
        .expect("compile");
    let out = e.run(&m, b"", RunOptions::default()).expect("run");
    assert_eq!(out.len(), 0, "growth past the ceiling should be denied");
    let denied = function_wasm_engine::metrics::sample(
        "function_wasm_module_memory_denials_total",
        &[("reason", "limit")],
    )
    .unwrap_or(0.0);
    assert!(denied >= 1.0, "the denial should be counted");
}

/// The shared pool reserves incrementally: a run holding the pool denies
/// another run's growth (its guest sees memory.grow fail), while the
/// second run's initial memory still fits and runs.
#[test]
fn the_pool_denies_growth_it_cannot_serve() {
    let e = std::sync::Arc::new(
        Engine::new(Config {
            memory_limit: 3 << 16,         // three pages
            max_total_run_memory: 3 << 16, // the whole pool
            ..Config::default()
        })
        .expect("engine"),
    );
    // The blocker: two initial pages held for ~300ms inside wasmfn.http.
    let request = br#"{"url":"http://example.com/x"}"#;
    // The bump allocator starts on the second of the two pages, so the
    // host's re-entrant allocation stays in bounds.
    let blocker_wat = format!(
        r#"(module
  (import "wasmfn" "http" (func $http (param i32 i32) (result i64)))
  (memory (export "memory") 2)
  (data (i32.const 1024) "{data}")
  (global $next (mut i32) (i32.const 65536))
  (func (export "wasmfn_alloc") (param i32) (result i32)
    (local $ptr i32)
    global.get $next
    local.tee $ptr
    local.get 0
    i32.add
    global.set $next
    local.get $ptr)
  (func (export "wasmfn_run") (param i32 i32) (result i64)
    i32.const 1024
    i32.const {len}
    call $http))"#,
        data = wat_bytes(request),
        len = request.len(),
    );
    let blocker = e
        .compile(&wat::parse_str(blocker_wat).expect("wat"))
        .expect("compile");
    let grower = e
        .compile(&wat::parse_str(grow_guest(1)).expect("wat"))
        .expect("compile");

    let eb = std::sync::Arc::clone(&e);
    let blocking = std::thread::spawn(move || {
        let opts = RunOptions {
            http: Some(std::sync::Arc::new(SlowOk(Duration::from_millis(300)))),
            ..Default::default()
        };
        eb.run(&blocker, b"", opts).expect("blocker run");
    });
    std::thread::sleep(Duration::from_millis(50));
    // Pool: blocker holds 2 pages, the grower's initial 1 fits; its growth
    // to 2 needs a page the pool cannot serve before the short deadline.
    let opts = RunOptions {
        timeout: Some(Duration::from_millis(100)),
        ..Default::default()
    };
    let out = e.run(&grower, b"", opts).expect("grower run");
    assert_eq!(
        out.len(),
        0,
        "growth the pool cannot serve should be denied"
    );
    blocking.join().expect("join");
    let denied = function_wasm_engine::metrics::sample(
        "function_wasm_module_memory_denials_total",
        &[("reason", "pool")],
    )
    .unwrap_or(0.0);
    assert!(denied >= 1.0, "the denial should be counted");
}

struct SlowOk(Duration);

impl function_wasm_engine::HttpRequester for SlowOk {
    fn do_request(
        &self,
        _req: &function_wasm_engine::wire::Request,
        _deadline: std::time::Instant,
    ) -> function_wasm_engine::wire::Response {
        std::thread::sleep(self.0);
        function_wasm_engine::wire::Response {
            status: 200,
            ..Default::default()
        }
    }
}

/// A guest blocked in wasmfn.http three times longer than its whole budget
/// still finishes: the wait is credited back to the epoch deadline, so
/// limits.timeout meters guest compute.
#[test]
fn http_wait_does_not_consume_the_deadline() {
    let e = engine();
    let rsp = response_bytes();
    let request = br#"{"url":"http://example.com/x"}"#;
    let packed = (4096u64 << 32) | rsp.len() as u64;
    let wat = format!(
        r#"(module
  (import "wasmfn" "http" (func $http (param i32 i32) (result i64)))
  (memory (export "memory") 4)
  (data (i32.const 1024) "{req}")
  (data (i32.const 4096) "{data}")
  {BUMP_ALLOC}
  (func (export "wasmfn_run") (param i32 i32) (result i64)
    i32.const 1024
    i32.const {len}
    call $http
    drop
    i64.const {packed}))"#,
        req = wat_bytes(request),
        data = wat_bytes(&rsp),
        len = request.len(),
        packed = packed as i64,
    );
    let m = e
        .compile(&wat::parse_str(wat).expect("wat"))
        .expect("compile");
    let opts = RunOptions {
        timeout: Some(Duration::from_millis(100)),
        http: Some(std::sync::Arc::new(SlowOk(Duration::from_millis(300)))),
        ..Default::default()
    };
    let out = e.run(&m, b"", opts).expect("run should outlive the wait");
    assert_eq!(out, rsp);
}

/// The request's own deadline is the hard wall-clock cap whatever the
/// credit: a guest that never returns is interrupted there.
#[test]
fn the_request_deadline_caps_the_run() {
    let e = engine();
    let wat = r#"(module (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64)
        (loop $l br $l)
        i64.const 0))"#;
    let m = e
        .compile(&wat::parse_str(wat).expect("wat"))
        .expect("compile");
    let opts = RunOptions {
        deadline: Some(std::time::Instant::now() + Duration::from_millis(80)),
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let err = e.run(&m, b"", opts).expect_err("should be cut short");
    assert!(
        err.to_string().contains("exceeded its execution deadline"),
        "unexpected error: {err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the hard deadline did not cap the run"
    );
}

/// Runs the repository's real Rust example guest when its built module is
/// present (make -C examples/hello-rust build), the way the Go runtime's
/// guest tests skip without a toolchain. Whatever the guest thinks of an
/// empty request, a decodable RunFunctionResponse proves the ABI mechanics
/// against a real prost guest, not just WAT fixtures.
#[test]
fn runs_the_real_rust_example_guest() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../examples/hello-rust/fn.wasm"
    );
    let Ok(wasm) = std::fs::read(path) else {
        eprintln!("skipping: {path} not built");
        return;
    };
    let e = engine();
    let m = e.compile(&wasm).expect("compile the example guest");
    let out = e
        .run(
            &m,
            &function_sdk_rust::proto::v1::RunFunctionRequest::default().encode_to_vec(),
            RunOptions::default(),
        )
        .expect("run the example guest");
    let rsp = RunFunctionResponse::decode(out.as_slice()).expect("decode the guest's response");
    assert!(
        rsp.meta.is_some() || !rsp.results.is_empty(),
        "empty response: {rsp:?}"
    );
}

#[test]
fn env_reaches_the_guest_sorted() {
    let e = engine();
    let wat = r#"(module
      (import "wasi_snapshot_preview1" "environ_sizes_get" (func $sizes (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "environ_get" (func $get (param i32 i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64)
        (drop (call $sizes (i32.const 0) (i32.const 4)))
        (drop (call $get (i32.const 1024) (i32.const 2048)))
        (i64.or
          (i64.shl (i64.const 2048) (i64.const 32))
          (i64.extend_i32_u (i32.load (i32.const 4))))))"#;
    let m = e
        .compile(&wat::parse_str(wat).expect("wat"))
        .expect("compile");
    let opts = RunOptions {
        env: [
            ("B".to_string(), "2".to_string()),
            ("A".to_string(), "1".to_string()),
        ]
        .into(),
        ..Default::default()
    };
    let out = e.run(&m, b"", opts).expect("run");
    // A BTreeMap serves the environ sorted, like the Go engine's SetEnv.
    assert_eq!(String::from_utf8_lossy(&out), "A=1\0B=2\0");
}

#[test]
fn private_tmp_is_the_only_preopen() {
    let e = engine();
    // fd_prestat_get(3) answers 0 (success) exactly when a directory is
    // pre-opened at descriptor 3 - the private /tmp - and EBADF (8) when the
    // run has none; the errno travels back as the response length.
    let wat = r#"(module
      (import "wasi_snapshot_preview1" "fd_prestat_get" (func $prestat (param i32 i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64)
        (i64.extend_i32_u (call $prestat (i32.const 3) (i32.const 0)))))"#;
    let m = e
        .compile(&wat::parse_str(wat).expect("wat"))
        .expect("compile");
    let with_tmp = e
        .run(
            &m,
            b"",
            RunOptions {
                private_tmp: true,
                ..Default::default()
            },
        )
        .expect("run");
    assert_eq!(with_tmp.len(), 0, "errno should be 0 with a private /tmp");
    let without = e.run(&m, b"", RunOptions::default()).expect("run");
    assert_eq!(without.len(), 8, "errno should be EBADF (8) without one");
}

// --- ABI v2: components implementing the wasmfn:function world -------------
//
// The fixtures are hand-written component text, sync-lifted: the canonical
// ABI accepts a sync implementation of the world's `async` run, which is
// also what keeps a stable-toolchain guest possible.

/// The core body shared by the component fixtures: a bump realloc and a
/// memory. `run` is provided per fixture.
const COMPONENT_CORE_PRELUDE: &str = r#"
    (memory (export "memory") 4)
    (global $next (mut i32) (i32.const 131072))
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      global.get $next
      local.set $p
      global.get $next
      local.get 3
      i32.add
      global.set $next
      local.get $p)
"#;

/// A component whose run returns the given bytes.
fn fixed_component(rsp: &[u8]) -> String {
    format!(
        r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func (export "run") (param i32 i32) (result i32)
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72) (i32.const {len}))
      (i32.const 64))
    (data (i32.const 1024) "{data}"))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory $i "memory") (realloc (core func $i "cabi_realloc"))))
)"#,
        len = rsp.len(),
        data = wat_bytes(rsp),
    )
}

#[test]
fn component_fixed_response_round_trip() {
    let e = engine();
    let rsp = response_bytes();
    let m = e
        .compile(&wat::parse_str(fixed_component(&rsp)).expect("wat"))
        .expect("compile");
    assert_eq!(m.abi_version(), 2);
    let out = e.run(&m, b"", RunOptions::default()).expect("run");
    assert_eq!(out, rsp);
}

/// The guest's Err(string) becomes a run error naming the export - v2's
/// third error channel, which v1 does not have.
#[test]
fn component_guest_error_string() {
    let e = engine();
    let wat = format!(
        r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func (export "run") (param i32 i32) (result i32)
      (i32.store8 (i32.const 64) (i32.const 1))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72) (i32.const 4))
      (i32.const 64))
    (data (i32.const 1024) "boom"))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory $i "memory") (realloc (core func $i "cabi_realloc"))))
)"#
    );
    let m = e
        .compile(&wat::parse_str(&wat).expect("wat"))
        .expect("compile");
    let err = e
        .run(&m, b"", RunOptions::default())
        .expect_err("guest error");
    assert_eq!(err.to_string(), "run returned an error: boom");
}

/// The epoch deadline interrupts a spinning component with the same message
/// as a v1 guest - the wording the timeout metric outcome matches on.
#[test]
fn component_run_deadline() {
    let e = engine();
    let wat = format!(
        r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func (export "run") (param i32 i32) (result i32)
      (loop $l br $l)
      i32.const 64))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory $i "memory") (realloc (core func $i "cabi_realloc"))))
)"#
    );
    let m = e
        .compile(&wat::parse_str(&wat).expect("wat"))
        .expect("compile");
    let err = e
        .run(
            &m,
            b"",
            RunOptions {
                timeout: Some(Duration::from_millis(50)),
                ..Default::default()
            },
        )
        .expect_err("deadline");
    assert!(
        err.to_string()
            .contains("module exceeded its execution deadline"),
        "unexpected: {err}"
    );
}

/// The run's memory ceiling denies a component's growth exactly as a core
/// module's: the guest sees memory.grow fail.
#[test]
fn component_memory_limit_denies_growth() {
    let e = engine();
    let wat = format!(
        r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func (export "run") (param i32 i32) (result i32)
      ;; try to grow by 64 MiB; on failure return err("grow denied")
      (if (i32.eq (memory.grow (i32.const 1024)) (i32.const -1))
        (then
          (i32.store8 (i32.const 64) (i32.const 1))
          (i32.store (i32.const 68) (i32.const 1024))
          (i32.store (i32.const 72) (i32.const 11))
          (return (i32.const 64))))
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72) (i32.const 0))
      (i32.const 64))
    (data (i32.const 1024) "grow denied"))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory $i "memory") (realloc (core func $i "cabi_realloc"))))
)"#
    );
    let m = e
        .compile(&wat::parse_str(&wat).expect("wat"))
        .expect("compile");
    let err = e
        .run(
            &m,
            b"",
            RunOptions {
                memory_limit: Some(1 << 20),
                ..Default::default()
            },
        )
        .expect_err("denied growth surfaces as the guest's error");
    assert_eq!(err.to_string(), "run returned an error: grow denied");
}

/// A component that does not implement the world is refused at load with
/// the world named - v2's checkABI.
#[test]
fn component_world_typecheck_refusals() {
    let e = engine();
    for (name, wat) in [
        ("empty", "(component)".to_string()),
        (
            "wrong type",
            format!(
                r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func (export "run") (param i32) (result i32) i32.const 0))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" u32) (result u32)
    (canon lift (core func $i "run")))
)"#
            ),
        ),
    ] {
        let err = e
            .compile(&wat::parse_str(&wat).expect("wat"))
            .expect_err(name);
        assert!(
            err.to_string()
                .starts_with("component does not implement the wasmfn:function@2.0.0-draft world:"),
            "{name}: unexpected: {err}"
        );
    }
}

/// A component that imports the world's log is linked against the host's
/// typed import and runs.
#[test]
fn component_log_import_is_provided() {
    let e = engine();
    let rsp = response_bytes();
    // The memory lives in its own core module so the lowered import can name
    // it before the main module (which needs that import) is instantiated;
    // the enum must reach the import's signature as an eq-bound type import
    // (a defined type is not importable directly).
    let wat = format!(
        r#"(component
  (type $level_def (enum "debug" "info"))
  (import "level" (type $level (eq $level_def)))
  (import "log" (func $log (param "level" $level) (param "msg" string) (param "kv" (list (tuple string string)))))
  (core module $libc
    {COMPONENT_CORE_PRELUDE})
  (core instance $libc_inst (instantiate $libc))
  (core func $log_lowered (canon lower (func $log) (memory $libc_inst "memory") (realloc (core func $libc_inst "cabi_realloc"))))
  (core module $m
    (import "env" "memory" (memory 4))
    (import "host" "log" (func $log (param i32 i32 i32 i32 i32)))
    (func (export "run") (param i32 i32) (result i32)
      ;; log(info, "hello from v2", [])
      (call $log (i32.const 1) (i32.const 2048) (i32.const 13) (i32.const 0) (i32.const 0))
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72) (i32.const {len}))
      (i32.const 64))
    (data (i32.const 1024) "{data}")
    (data (i32.const 2048) "hello from v2"))
  (core instance $m_inst (instantiate $m
    (with "env" (instance (export "memory" (memory $libc_inst "memory"))))
    (with "host" (instance (export "log" (func $log_lowered))))))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $m_inst "run") (memory $libc_inst "memory") (realloc (core func $libc_inst "cabi_realloc"))))
)"#,
        len = rsp.len(),
        data = wat_bytes(&rsp),
    );
    let m = e
        .compile(&wat::parse_str(&wat).expect("wat"))
        .expect("compile");
    let out = e.run(&m, b"", RunOptions::default()).expect("run");
    assert_eq!(out, rsp);
}

/// A serialized component artifact loads back through the same cache path a
/// module's does; the artifact itself says which kind it is.
#[test]
fn component_serialize_round_trip() {
    let e = engine();
    let rsp = response_bytes();
    let m = e
        .compile(&wat::parse_str(fixed_component(&rsp)).expect("wat"))
        .expect("compile");
    let artifact = e.serialize(&m).expect("serialize");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("c.bin");
    std::fs::write(&path, &artifact).expect("write");
    let loaded = e.deserialize_file(&path).expect("deserialize");
    assert_eq!(loaded.abi_version(), 2);
    let out = e.run(&loaded, b"", RunOptions::default()).expect("run");
    assert_eq!(out, rsp);
}

/// inspect reports a component as ABI v2 with the world verdict, and a
/// serialized artifact is still refused as a module source.
#[test]
fn component_inspection() {
    let e = engine();
    let rsp = response_bytes();
    let shape = e
        .inspect(&wat::parse_str(fixed_component(&rsp)).expect("wat"))
        .expect("inspect");
    assert_eq!(shape.abi_version, 2);
    assert_eq!(shape.abi_error, None);
    assert!(
        shape
            .exports
            .iter()
            .any(|x| x.name == "run" && x.kind == "func")
    );
    assert!(shape.memories.is_empty());

    let bad = e.inspect(b"(component)".as_ref());
    assert!(bad.is_err(), "text is not a module source");
}
