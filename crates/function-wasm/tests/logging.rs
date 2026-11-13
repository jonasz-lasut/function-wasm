//! The served runtime's log filter, end to end (#135): `--debug` turns on the
//! runtime's own DEBUG lines and nobody else's, and a `RUST_LOG` that is set
//! wins over the flag. The guest's own debug record (the world's `log`
//! import at `debug`, forwarded by the engine at DEBUG) is the runtime's
//! line; the other crates'
//! are h2's per-frame lines on every connection and wasmtime's per-section
//! ones on every load (cranelift's per-pass ones need a module big enough to
//! matter, so the test asserts on the class, not on cranelift by name).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use function_sdk_rust::proto::v1::function_runner_service_client::FunctionRunnerServiceClient;
use function_sdk_rust::proto::v1::{RequestMeta, RunFunctionRequest};
use function_sdk_rust::resource;
use guestfn_runtime::input::API_VERSION;

const GUEST_MSG: &str = "a debug record from the guest";

/// Serves the guest with the given flags and environment, runs it once and
/// returns every log line the process wrote (the subscriber writes to stdout).
fn serve_and_run(flags: &[&str], rust_log: Option<&str>) -> Vec<String> {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        guestfn_runtime::testing::guests::logging_component(GUEST_MSG),
    )
    .expect("write");

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    // The function takes an address and reports no port of its own, so
    // the port is learned from a listener dropped before the function
    // binds it, and another process can take it in between. A function
    // that exits instead of serving is started again on another port.
    let (mut child, channel) = (0..5)
        .find_map(|_| {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .expect("bind")
                .local_addr()
                .expect("addr")
                .port();
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_function-wasm"));
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
            let channel = rt.block_on(connect(port, &mut child))?;
            Some((child, channel))
        })
        .expect("the function found no free port in five tries");
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

    rt.block_on(async {
        let mut client = FunctionRunnerServiceClient::new(channel);
        let req = RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".to_string(),
                ..Default::default()
            }),
            input: Some(resource::json_to_struct(
                serde_json::json!({
                    "apiVersion": API_VERSION,
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

    // The guest's record is written from the run, before the response, and
    // the subscriber writes each line to stdout as it is logged: every
    // line of the run is in the pipe by now. A kill leaves the pipe's
    // content in place and the reader goes on to its end, so nothing has
    // to be waited for, least of all a line a test expects not to find.
    child.kill().expect("kill");
    child.wait().expect("wait");
    reader.join().expect("reader");
    lines.lock().expect("lock").clone()
}

/// Connects to the function once it serves. None when the function has
/// exited instead, which is what it does when its port was taken; the
/// connection made, the function must still be running, or the listener
/// that answered was another process's.
async fn connect(port: u16, function: &mut Child) -> Option<tonic::transport::Channel> {
    let exited = |function: &mut Child| function.try_wait().expect("try_wait").is_some();
    for _ in 0..100 {
        if let Ok(channel) =
            tonic::transport::Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
                .expect("uri")
                .connect()
                .await
        {
            return (!exited(function)).then_some(channel);
        }
        if exited(function) {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("cannot connect to the function under test");
}

/// DEBUG lines from any crate but the runtime's own.
fn foreign_debug_lines(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.contains("DEBUG") && !l.contains("guestfn_"))
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
        guest[0].contains("DEBUG") && guest[0].contains("guestfn_engine_wasm::component"),
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
