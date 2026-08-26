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
fn a_compiled_artifact_is_refused_by_name() {
    let e = engine();
    let rsp = response_bytes();
    let m = e
        .compile(&wat::parse_str(fixed(&rsp)).expect("wat"))
        .expect("compile");
    let artifact = e.serialize(&m).expect("serialize");
    let err = e.compile(&artifact).expect_err("should refuse the artifact");
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
