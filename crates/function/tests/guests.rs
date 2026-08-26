//! The example guests - the same greeting function written with
//! function-sdk-go (Go), with TinyGo and vtprotobuf, in Rust with prost, in
//! Zig with zig-protobuf, in C with nanopb, in AssemblyScript with as-proto,
//! and as an ABI v2 component in async Rust with wit-bindgen - through the
//! whole host: path and OCI sources, compile, per-request instance, egress
//! through wasmfn.http or wasi:http, guest logging. Every guest must produce
//! the same response.
//! A guest whose toolchain is not on PATH is skipped, like the Go tree's
//! guest tests skip without theirs.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use function_sdk_rust::proto::v1::{
    Capability, Condition, RequestMeta, Resource, ResponseMeta, Result as FnResult,
    RunFunctionRequest, RunFunctionResponse, Severity, State, Status, Target,
};
use function_sdk_rust::resource;
use function_wasm::authz::{IpPrefix, IpRules, OperatorPolicy};
use function_wasm::cache::{CacheOptions, ModuleCache};
use function_wasm::oci::testregistry;
use function_wasm::resolver::Resolver;
use function_wasm::runner::WasmFunction;
use function_wasm_engine::{Config, Engine};
use sha2::Digest as _;

/// Every log line of the process, captured by a global subscriber so lines
/// from the engine's blocking threads land too.
static LOGS: LazyLock<Arc<Mutex<Vec<u8>>>> = LazyLock::new(|| {
    let buf: Arc<Mutex<Vec<u8>>> = Arc::default();
    let writer = buf.clone();
    struct W(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for W {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("poisoned").extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || W(writer.clone()))
            .finish(),
    )
    .expect("no other global subscriber");
    buf
});

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(name).is_file()))
}

fn command(dir: &Path, name: &str, args: &[&str], envs: &[(&str, &str)]) -> bool {
    let mut cmd = std::process::Command::new(name);
    cmd.args(args).current_dir(dir);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    match cmd.output() {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            eprintln!(
                "{name} {} failed:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
            false
        }
        Err(e) => {
            eprintln!("{name}: {e}");
            false
        }
    }
}

/// Builds one example guest with its toolchain; None skips the guest.
fn build_guest(guest: &str, out: &Path) -> Option<Vec<u8>> {
    let dir = examples().join(format!("hello-{guest}"));
    let out_s = out.to_string_lossy().into_owned();
    let built = match guest {
        "go" => {
            if !on_path("go") {
                eprintln!("skipping: go not on PATH");
                return None;
            }
            command(
                &dir,
                "go",
                &["build", "-buildmode=c-shared", "-o", &out_s, "."],
                &[("GOOS", "wasip1"), ("GOARCH", "wasm")],
            )
        }
        "tinygo" => {
            if !on_path("tinygo") {
                eprintln!("skipping: tinygo not on PATH");
                return None;
            }
            command(
                &dir,
                "tinygo",
                &[
                    "build",
                    "-target=wasip1",
                    "-buildmode=c-shared",
                    "-no-debug",
                    "-o",
                    &out_s,
                    ".",
                ],
                &[],
            )
        }
        "rust" => {
            if !on_path("cargo") || !on_path("rustup") {
                eprintln!("skipping: cargo/rustup not on PATH");
                return None;
            }
            let targets = std::process::Command::new("rustup")
                .args(["target", "list", "--installed"])
                .output()
                .ok()?;
            if !String::from_utf8_lossy(&targets.stdout).contains("wasm32-wasip1") {
                eprintln!("skipping: wasm32-wasip1 target not installed");
                return None;
            }
            if !command(
                &dir,
                "cargo",
                &["build", "--release", "--target", "wasm32-wasip1"],
                &[],
            ) {
                return Some(Vec::new());
            }
            let release = dir.join("target/wasm32-wasip1/release");
            let wasm = std::fs::read_dir(&release)
                .ok()?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == "wasm"))?;
            std::fs::copy(&wasm, out).ok()?;
            true
        }
        "rust-v2" => {
            if !on_path("cargo") || !on_path("rustup") {
                eprintln!("skipping: cargo/rustup not on PATH");
                return None;
            }
            let targets = std::process::Command::new("rustup")
                .args(["target", "list", "--installed"])
                .output()
                .ok()?;
            if !String::from_utf8_lossy(&targets.stdout).contains("wasm32-wasip2") {
                eprintln!("skipping: wasm32-wasip2 target not installed");
                return None;
            }
            if !command(
                &dir,
                "cargo",
                &["build", "--release", "--target", "wasm32-wasip2"],
                &[],
            ) {
                return Some(Vec::new());
            }
            let release = dir.join("target/wasm32-wasip2/release");
            let wasm = std::fs::read_dir(&release)
                .ok()?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == "wasm"))?;
            std::fs::copy(&wasm, out).ok()?;
            true
        }
        "assemblyscript" => {
            if !on_path("npm") {
                eprintln!("skipping: npm not on PATH");
                return None;
            }
            if !command(&dir, "npm", &["ci", "--no-audit", "--no-fund"], &[])
                || !command(&dir, "npm", &["run", "build"], &[])
            {
                return Some(Vec::new());
            }
            std::fs::copy(dir.join("fn.wasm"), out).ok()?;
            true
        }
        "zig" | "c" => {
            if !on_path("zig") {
                eprintln!("skipping: zig not on PATH");
                return None;
            }
            if !command(&dir, "zig", &["build", "-Doptimize=ReleaseSmall"], &[]) {
                return Some(Vec::new());
            }
            std::fs::copy(dir.join("zig-out/bin/fn.wasm"), out).ok()?;
            true
        }
        _ => false,
    };
    assert!(built, "building the {guest} guest failed");
    Some(std::fs::read(out).expect("read built module"))
}

/// A tiny greeting server: /en answers "howdy\n".
fn greeting_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let mut conn = conn;
            let mut buf = [0u8; 1024];
            let n = std::io::Read::read(&mut conn, &mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or_default();
            let (status, body) = if path == "/en" {
                ("200 OK", "howdy\n")
            } else {
                ("404 Not Found", "")
            };
            let _ = conn.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    addr.to_string()
}

/// An operator grant policy that enables every sandbox capability for any
/// caller - a fully open operator layer. The layer is the enabler, so a
/// function without one grants nothing.
fn permissive_policy() -> OperatorPolicy {
    OperatorPolicy::new(
        "test.cedar",
        r#"
permit (principal, action == Action::"usePrivateTmp", resource);
permit (principal, action == Action::"setEnv", resource);
permit (principal, action == Action::"grantEgress", resource);
permit (principal, action == Action::"spendCredential", resource);
"#,
    )
    .expect("policy")
}

fn digest_of(b: &[u8]) -> String {
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(b)))
}

fn response(greeting: &str) -> RunFunctionResponse {
    RunFunctionResponse {
        meta: Some(ResponseMeta {
            tag: "hello".to_string(),
            ttl: Some(pbjson_types::Duration {
                seconds: 60,
                nanos: 0,
            }),
        }),
        desired: Some(State {
            resources: [(
                "greeting".to_string(),
                Resource {
                    resource: Some(resource::json_to_struct(
                        serde_json::json!({
                            "apiVersion": "v1",
                            "kind": "ConfigMap",
                            "data": {"greeting": greeting},
                        })
                        .as_object()
                        .expect("object"),
                    )),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        }),
        results: vec![FnResult {
            severity: Severity::Normal as i32,
            message: "greeted my-xr".to_string(),
            target: Some(Target::Composite as i32),
            ..Default::default()
        }],
        conditions: vec![Condition {
            r#type: "FunctionSuccess".to_string(),
            status: Status::ConditionTrue as i32,
            reason: "Success".to_string(),
            target: Some(Target::CompositeAndClaim as i32),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn fatal_response() -> RunFunctionResponse {
    RunFunctionResponse {
        meta: Some(ResponseMeta {
            tag: "hello".to_string(),
            ttl: Some(pbjson_types::Duration {
                seconds: 60,
                nanos: 0,
            }),
        }),
        results: vec![FnResult {
            severity: Severity::Fatal as i32,
            target: Some(Target::Composite as i32),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn run_guest(guest: &str) {
    // CI's unit-test job is deliberately toolchain-free: the render jobs
    // prove each guest end to end, and the runner's preinstalled Go would
    // otherwise build the go guest with an unpinned toolchain.
    if std::env::var_os("SKIP_GUEST_BUILDS").is_some() {
        eprintln!("skipping: SKIP_GUEST_BUILDS is set");
        return;
    }
    let logs = LOGS.clone();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = format!("{guest}.wasm");
    let Some(wasm) = build_guest(guest, &dir.path().join(&file)) else {
        return; // toolchain not available
    };
    assert!(!wasm.is_empty(), "building the {guest} guest failed");

    // What guestfn inspect and validate --resolve report: a core-module
    // guest is ABI v1 and imports both wasmfn host functions; a component
    // guest is ABI v2 and exports the world's run.
    let engine = Arc::new(Engine::new(Config::default()).expect("engine"));
    let shape = engine.inspect(&wasm).expect("inspect");
    assert!(shape.abi_error.is_none(), "{:?}", shape.abi_error);
    let abi = shape.abi_version;
    if abi == 1 {
        for import in ["wasmfn.log", "wasmfn.http"] {
            assert!(
                shape.host_imports.iter().any(|i| i == import),
                "guest imports {:?}, want {import}",
                shape.host_imports
            );
        }
    } else {
        assert!(
            shape.exports.iter().any(|x| x.name == "run"),
            "guest exports {:?}, want run",
            shape.exports
        );
    }

    let greetings = greeting_server();
    let greetings_host = greetings.split(':').next().expect("host").to_string();
    let egress_manifest = format!(
        r#"{{"abi":{abi},"requires":{{"egress":{{"http":[{{"host":"{greetings_host}","methods":["GET"]}}]}}}}}}"#
    );
    // The guest with an egress request, two ways: an OCI artifact's manifest
    // layer, and a manifest a path module names by reference
    // (module.manifestPath) - the local-dev loop for a capability-needing
    // module.
    let (artifact_digest, registry) =
        testregistry::wasm_artifact(&wasm, Some(egress_manifest.as_bytes()), false);
    let egress_ref = format!("{registry}/hello-{guest}@{artifact_digest}");
    let path_manifest = format!("{guest}-manifest.yaml");
    std::fs::write(dir.path().join(&path_manifest), &egress_manifest).expect("write");

    // The egress ceiling lifts the loopback ranges the block list would
    // otherwise refuse (every resolved address is judged).
    let rules = IpRules {
        blocked: Vec::new(),
        allowed: vec![
            IpPrefix::parse("127.0.0.0/8").expect("prefix"),
            IpPrefix::parse("::1/128").expect("prefix"),
        ],
    };
    let f = WasmFunction {
        cache: Arc::new(ModuleCache::new(
            Arc::clone(&engine),
            CacheOptions::default(),
        )),
        engine,
        resolver: Arc::new(Resolver::new(Some(dir.path().to_owned()), 128 << 20, None)),
        ttl: std::time::Duration::from_secs(60),
        policy: Some(permissive_policy()),
        egress: Arc::new(function_wasm::egress::Egress::new(rules, 0.0, 0)),
        step_slots: Arc::new(function_wasm_engine::concurrency::StepSlots::new()),
        verifier: None,
    };

    let module_digest = digest_of(&wasm);
    let cases: Vec<(&str, String, RunFunctionResponse, Vec<String>)> = vec![
        (
            "Default",
            format!(
                r#"{{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{{"type":"Path","path":"{file}"}}}}"#
            ),
            response("hello my-xr"),
            vec![
                "Running function".to_string(),
                format!("digest=\"{module_digest}\""),
            ],
        ),
        (
            "Configured",
            format!(
                r#"{{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{{"type":"Path","path":"{file}"}},"config":{{"greeting":"hi"}}}}"#
            ),
            response("hi my-xr"),
            vec!["Running function".to_string()],
        ),
        (
            "GreetingFromURL",
            format!(
                r#"{{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{{"type":"OCI","oci":{{"ref":"{egress_ref}"}}}},"config":{{"greetingUrl":"http://{greetings}/en"}}}}"#
            ),
            response("howdy my-xr"),
            vec!["outcome=\"ok\"".to_string()],
        ),
        (
            "GreetingFromURLPath",
            format!(
                r#"{{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{{"type":"Path","path":"{file}","manifestPath":"{path_manifest}"}},"config":{{"greetingUrl":"http://{greetings}/en"}}}}"#
            ),
            response("howdy my-xr"),
            vec!["outcome=\"ok\"".to_string()],
        ),
        (
            "GreetingURLWithoutGrant",
            format!(
                r#"{{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{{"type":"Path","path":"{file}"}},"config":{{"greetingUrl":"http://{greetings}/en"}}}}"#
            ),
            fatal_response(),
            vec!["outcome=\"refused\"".to_string()],
        ),
        (
            "BadConfig",
            format!(
                r#"{{"apiVersion":"wasm.fn.crossplane.io/v1beta1","kind":"Input","module":{{"type":"Path","path":"{file}"}},"config":{{"greeting":7}}}}"#
            ),
            fatal_response(),
            Vec::new(),
        ),
    ];

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    for (name, input, want, want_logs) in cases {
        let start = logs.lock().expect("poisoned").len();
        let input: serde_json::Value = serde_json::from_str(&input).expect("input json");
        let req = RunFunctionRequest {
            // Capabilities ride along because crossplane always sends
            // them: protobuf packs the repeated enum, and a guest codec
            // that cannot decode the packed form desyncs on the whole
            // request (as-proto's generator did).
            meta: Some(RequestMeta {
                tag: "hello".to_string(),
                capabilities: vec![
                    Capability::Capabilities as i32,
                    Capability::Conditions as i32,
                ],
            }),
            input: Some(resource::json_to_struct(input.as_object().expect("object"))),
            observed: Some(State {
                composite: Some(Resource {
                    resource: Some(resource::json_to_struct(
                        serde_json::json!({
                            "apiVersion": "example.org/v1",
                            "kind": "XR",
                            "metadata": {"name": "my-xr"},
                        })
                        .as_object()
                        .expect("object"),
                    )),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        use prost::Message as _;
        let out = runtime
            .block_on(f.handle_raw(req.encode_to_vec(), None))
            .expect("handle_raw");
        let mut rsp = RunFunctionResponse::decode(out.as_slice()).expect("decode");
        // Fatal messages are worded per guest; only their presence is
        // asserted.
        if want
            .results
            .first()
            .is_some_and(|r| r.severity == Severity::Fatal as i32)
        {
            for r in &mut rsp.results {
                if r.severity == Severity::Fatal as i32 && !r.message.is_empty() {
                    r.message = String::new();
                }
            }
        }
        assert_eq!(want, rsp, "{guest}/{name}");
        let captured =
            String::from_utf8_lossy(&logs.lock().expect("poisoned")[start..]).into_owned();
        for needle in want_logs {
            assert!(
                captured.contains(&needle),
                "{guest}/{name}: logs missing {needle:?} in:\n{captured}"
            );
        }
    }
}

#[test]
fn go_guest() {
    run_guest("go");
}

#[test]
fn tinygo_guest() {
    run_guest("tinygo");
}

#[test]
fn rust_guest() {
    run_guest("rust");
}

#[test]
fn rust_v2_guest() {
    run_guest("rust-v2");
}

#[test]
fn zig_guest() {
    run_guest("zig");
}

#[test]
fn c_guest() {
    run_guest("c");
}

#[test]
fn assemblyscript_guest() {
    run_guest("assemblyscript");
}
