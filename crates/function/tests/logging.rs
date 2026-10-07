//! The served runtime's log filter, end to end (#135): `--debug` turns on the
//! runtime's own DEBUG lines and nobody else's, and a `RUST_LOG` that is set
//! wins over the flag. The guest's own debug record (the world's `log`
//! import at `debug`, forwarded by the engine at DEBUG) is the runtime's
//! line; the other crates'
//! are h2's per-frame lines on every connection and wasmtime's per-section
//! ones on every load (cranelift's per-pass ones need a module big enough to
//! matter, so the test asserts on the class, not on cranelift by name).

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use function_sdk_rust::proto::v1::function_runner_service_client::FunctionRunnerServiceClient;
use function_sdk_rust::proto::v1::{RequestMeta, RunFunctionRequest};
use function_sdk_rust::resource;

const GUEST_MSG: &str = "a debug record from the guest";

/// A component that logs one record at the world's `debug` level and
/// returns an empty response. The log import is lowered against the memory
/// of a core module instantiated first, as a toolchain's libc would be.
fn logging_guest() -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
          (type $level_def (enum "debug" "info" "warn" "error"))
          (import "level" (type $level (eq $level_def)))
          (import "log" (func $log (param "level" $level) (param "msg" string) (param "kv" (list (tuple string string)))))
          (core module $libc
            (memory (export "memory") 1)
            (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 4096))
          (core instance $libc_inst (instantiate $libc))
          (core func $log_lowered (canon lower (func $log) (memory (core memory $libc_inst "memory")) (realloc (core func $libc_inst "cabi_realloc"))))
          (core module $m
            (import "env" "memory" (memory 1))
            (import "host" "log" (func $log (param i32 i32 i32 i32 i32)))
            (func (export "run") (param i32 i32) (result i32)
              (call $log (i32.const 0) (i32.const 2048) (i32.const {len}) (i32.const 0) (i32.const 0))
              (i32.store8 (i32.const 64) (i32.const 0))
              (i32.store (i32.const 68) (i32.const 1024))
              (i32.store (i32.const 72) (i32.const 0))
              (i32.const 64))
            (data (i32.const 2048) "{GUEST_MSG}"))
          (core instance $m_inst (instantiate $m
            (with "env" (instance (export "memory" (memory $libc_inst "memory"))))
            (with "host" (instance (export "log" (func $log_lowered))))))
          (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
            (canon lift (core func $m_inst "run") (memory (core memory $libc_inst "memory")) (realloc (core func $libc_inst "cabi_realloc"))))
        )"#,
        len = GUEST_MSG.len(),
    ))
    .expect("wat")
}

/// Serves the guest with the given flags and environment, runs it once and
/// returns every log line the process wrote (the subscriber writes to stdout).
fn serve_and_run(flags: &[&str], rust_log: Option<&str>) -> Vec<String> {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("fn.wasm"), logging_guest()).expect("write");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_function"));
    cmd.args([
        "--insecure",
        "--address",
        &format!("127.0.0.1:{port}"),
        "--health-address",
        "",
        "--metrics-address",
        "",
        "--module-dir",
    ])
    .arg(dir.path())
    .args(flags)
    .env_remove("DEBUG")
    .env_remove("RUST_LOG")
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit());
    if let Some(v) = rust_log {
        cmd.env("RUST_LOG", v);
    }
    let mut child = cmd.spawn().expect("spawn function");
    let stdout = child.stdout.take().expect("stdout");
    let lines = Arc::new(Mutex::new(Vec::new()));
    let reader = {
        let lines = Arc::clone(&lines);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                lines.lock().expect("lock").push(line);
            }
        })
    };

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let channel = connect(port).await;
        let mut client = FunctionRunnerServiceClient::new(channel);
        let req = RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".to_string(),
                ..Default::default()
            }),
            input: Some(resource::json_to_struct(
                serde_json::json!({
                    "apiVersion": "wasm.fn.crossplane.io/v1beta1",
                    "kind": "Input",
                    "module": {"type": "Path", "path": "fn.wasm"},
                })
                .as_object()
                .expect("object"),
            )),
            ..Default::default()
        };
        let rsp = client
            .run_function(req)
            .await
            .expect("RunFunction")
            .into_inner();
        assert!(rsp.results.is_empty(), "the run failed: {:?}", rsp.results);
    });

    // The guest's record is written from the run, before the response; one
    // bounded wait for the reader to see it keeps the assertions honest.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline
        && !lines
            .lock()
            .expect("lock")
            .iter()
            .any(|l| l.contains(GUEST_MSG))
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    child.kill().expect("kill");
    child.wait().expect("wait");
    reader.join().expect("reader");
    lines.lock().expect("lock").clone()
}

async fn connect(port: u16) -> tonic::transport::Channel {
    for _ in 0..100 {
        if let Ok(channel) =
            tonic::transport::Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
                .expect("uri")
                .connect()
                .await
        {
            return channel;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("cannot connect to the function under test");
}

/// DEBUG lines from any crate but the runtime's own.
fn foreign_debug_lines(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.contains("DEBUG") && !l.contains("function_wasm"))
        .collect()
}

fn guest_lines(lines: &[String]) -> Vec<&String> {
    lines.iter().filter(|l| l.contains(GUEST_MSG)).collect()
}

#[test]
fn debug_is_scoped_to_the_runtime() {
    let lines = serve_and_run(&["--debug"], None);
    let guest = guest_lines(&lines);
    assert_eq!(
        guest.len(),
        1,
        "the guest's debug record is the runtime's own DEBUG line:\n{}",
        lines.join("\n")
    );
    assert!(
        guest[0].contains("DEBUG") && guest[0].contains("function_wasm_engine::component"),
        "the record is logged at DEBUG by the engine: {}",
        guest[0]
    );
    let foreign = foreign_debug_lines(&lines);
    assert!(
        foreign.is_empty(),
        "--debug must not turn on another crate's DEBUG lines:\n{}",
        foreign
            .iter()
            .take(5)
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn without_debug_the_runtime_logs_json_at_info() {
    let lines = serve_and_run(&[], None);
    assert!(
        guest_lines(&lines).is_empty(),
        "a debug record is not logged at info:\n{}",
        lines.join("\n")
    );
    assert!(foreign_debug_lines(&lines).is_empty());
    let served = lines
        .iter()
        .find(|l| l.contains("serving FunctionRunnerService"))
        .expect("the serving line");
    assert!(
        served.starts_with('{') && served.contains("\"level\":\"INFO\""),
        "the pod's lines are JSON: {served}"
    );
}

#[test]
fn rust_log_wins_over_the_flag() {
    let lines = serve_and_run(&["--debug"], Some("info,h2=debug"));
    assert!(
        foreign_debug_lines(&lines)
            .iter()
            .any(|l| l.contains("h2::")),
        "RUST_LOG names h2, so its lines are on:\n{}",
        lines.join("\n")
    );
    assert!(
        guest_lines(&lines).is_empty(),
        "RUST_LOG replaces the flag's directives, so the runtime stays at info:\n{}",
        lines.join("\n")
    );
}
