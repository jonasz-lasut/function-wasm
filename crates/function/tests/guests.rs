//! The guests - the same greeting function written as ABI v2 components
//! with function-sdk-go (Go over wit-bindgen's Go bindings, sync-lifted;
//! go build emits the core module and the suite embeds its world and wraps
//! it as guestfn build does), in async Rust with wit-bindgen, and as
//! components in TypeScript and Python: the four languages with an
//! official function-sdk, the ones `guestfn init` scaffolds (#264) -
//! through the whole host: path and OCI sources, compile, per-request
//! instance, egress through wasi:http, guest logging. Every guest must
//! produce the same response.
//!
//! A language with a scaffold is built from what `guestfn init` writes (its
//! golden under crates/guestfn/testdata, which the scaffold tests keep
//! byte-identical to the templates), so this suite proves the projects users
//! start from; the examples under examples/ solve their own use cases and are
//! checked by their render jobs.
//!
//! A guest whose toolchain is not on PATH is skipped, naming the guest,
//! unless GUESTFN_REQUIRE_TOOLCHAINS
//! is set, as ci.yml's guest-suite job sets it, when a missing
//! toolchain fails the guest instead: a runner that lost a toolchain would
//! otherwise shrink the suite and stay green. A skip is said on the
//! process's own stderr (`SKIPPED: ...`, with the count so far), since the
//! harness shows nothing a passing test printed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use function_sdk_rust::proto::v1::{
    Capability, Condition, RequestMeta, Resource, ResponseMeta, Result as FnResult,
    RunFunctionRequest, RunFunctionResponse, Severity, State, Status, Target,
};
use function_sdk_rust::resource;
use guestfn_engine::Engine as _;
use guestfn_engine_wasm::componentize;
use guestfn_runtime::oci::testregistry;
use guestfn_runtime::testing::{http, policy, runtime};
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

/// Where a guest is built: a fresh copy of its scaffold's golden.
fn guest_dir(guest: &str) -> PathBuf {
    let golden = match guest {
        "rust-v2" => "rust",
        other => other,
    };
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../guestfn/testdata")
        .join(golden);
    // Outside the repository: a scaffold is a standalone project, and under
    // this checkout cargo would take it for a member of the workspace. Keyed
    // by the checkout, so worktrees never build on one another's caches,
    // which go stale whenever the scaffolds differ between them.
    let checkout = hex::encode(&sha2::Sha256::digest(env!("CARGO_MANIFEST_DIR").as_bytes())[..6]);
    let dir = std::env::temp_dir()
        .join(format!("function-wasm-guest-scaffolds-{checkout}"))
        .join(guest);
    // Build caches survive between runs; every other file is the golden's,
    // so a file the scaffold no longer writes cannot linger.
    const CACHES: [&str; 3] = [".venv", "node_modules", "target"];
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if CACHES.iter().any(|c| entry.file_name() == *c) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path).expect("clear scaffold dir");
            } else {
                std::fs::remove_file(&path).expect("clear scaffold file");
            }
        }
    }
    copy_tree(&src, &dir);
    dir
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create dir");
    for entry in std::fs::read_dir(src).expect("read golden").flatten() {
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).expect("copy golden file");
        }
    }
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(name).is_file()))
}

/// Skips a guest for a missing toolchain, naming it - or fails, where
/// GUESTFN_REQUIRE_TOOLCHAINS says every guest must build.
fn skip(guest: &str, reason: &str) -> Option<Vec<u8>> {
    if std::env::var_os("GUESTFN_REQUIRE_TOOLCHAINS").is_some() {
        panic!("the {guest} guest cannot build ({reason}) and GUESTFN_REQUIRE_TOOLCHAINS is set");
    }
    report_skip(&format!("the {guest} guest: {reason}"));
    None
}

/// Says that the calling test asserted nothing, on the process's own
/// stderr: the harness captures what a passing test prints with eprintln!,
/// so a skipped suite would otherwise read as a passed one. The count is
/// this binary's so far, and the last line carries the total.
fn report_skip(reason: &str) {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SKIPPED: AtomicUsize = AtomicUsize::new(0);
    let skipped = SKIPPED.fetch_add(1, Ordering::SeqCst) + 1;
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("a test");
    // One write, so lines from tests running side by side stay whole.
    let line = format!(
        "SKIPPED: {test} asserted nothing: {reason} ({skipped} skipped so far in the guest suite)\n"
    );
    let _ = std::io::stderr().write_all(line.as_bytes());
}

fn command(dir: &Path, name: &str, args: &[&str], envs: &[(&str, &str)]) -> bool {
    let mut cmd = std::process::Command::new(name);
    // rustup exports RUSTUP_TOOLCHAIN to everything under the cargo running
    // this suite, which would override a guest's own rust-toolchain.toml.
    cmd.args(args)
        .current_dir(dir)
        .env_remove("RUSTUP_TOOLCHAIN");
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

/// The targets of the toolchain a directory's rust-toolchain.toml names.
/// The first rustup call in such a directory installs what the file lists
/// and the toolchain lacks, and two installs at once fail on each other's
/// download: the tests that build Rust take turns here, after which their
/// cargo runs find the toolchain complete.
fn installed_targets(dir: &Path) -> Option<String> {
    static RUSTUP: Mutex<()> = Mutex::new(());
    let _one_at_a_time = RUSTUP.lock().unwrap_or_else(|e| e.into_inner());
    // A proxied tool is what makes rustup install; the listing alone may
    // only read.
    let _ = std::process::Command::new("cargo")
        .arg("--version")
        .current_dir(dir)
        .env_remove("RUSTUP_TOOLCHAIN")
        .output();
    let targets = std::process::Command::new("rustup")
        .args(["target", "list", "--installed"])
        .current_dir(dir)
        .env_remove("RUSTUP_TOOLCHAIN")
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&targets.stdout).into_owned())
}

/// Builds one guest with its toolchain; None skips the guest.
/// Pulls what a scaffold's guestfn.lock pins into its wit/deps, as guestfn
/// build does first for a user. The suite drives each toolchain itself, and
/// guestfn is a binary this crate cannot link. This cargo runs from the
/// workspace with the environment it was given: command() drops
/// RUSTUP_TOOLCHAIN for the guests' own builds, under which rustup would act
/// on any toolchain file a dependency's build script runs beside.
fn fetch_wit(dir: &Path) -> bool {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut run = std::process::Command::new(cargo);
    run.current_dir(workspace)
        .args(["run", "--quiet", "-p", "guestfn"]);
    if !cfg!(debug_assertions) {
        run.arg("--release");
    }
    run.args(["--", "wit", "fetch", "--dir"]).arg(dir);
    match run.status() {
        Ok(status) => status.success(),
        Err(e) => {
            eprintln!("cannot run guestfn wit fetch: {e}");
            false
        }
    }
}

fn build_guest(guest: &str, out: &Path) -> Option<Vec<u8>> {
    let dir = guest_dir(guest);
    let out_s = out.to_string_lossy().into_owned();
    let built = match guest {
        "go" => {
            if !on_path("go") {
                return skip(guest, "go not on PATH");
            }
            if !fetch_wit(&dir) {
                return Some(Vec::new());
            }
            // A scaffold's go.mod has no go.sum yet; guestfn init tidies it.
            // -checklinkname=0 admits the bindings runtime's linkname to
            // runtime.sbrk, as guestfn build passes it.
            if !command(&dir, "go", &["mod", "tidy"], &[])
                || !command(
                    &dir,
                    "go",
                    &[
                        "build",
                        "-buildmode=c-shared",
                        "-ldflags=-checklinkname=0",
                        "-o",
                        &out_s,
                        ".",
                    ],
                    &[("GOOS", "wasip1"), ("GOARCH", "wasm")],
                )
            {
                return Some(Vec::new());
            }
            // go build emits a wasip1 core module with no component type:
            // the scaffold's world is embedded from its wit/ and the module
            // wrapped (with the wasip1 adapter) here, as guestfn build does
            // for a user.
            let core = std::fs::read(out).ok()?;
            let embedded = componentize::embed_world(
                &core,
                &dir.join("wit"),
                "function",
                componentize::StringEncoding::Utf8,
            )
            .expect("embed the go scaffold's world");
            let wasm = componentize::componentize(&embedded)
                .expect("componentize the go-built core module")
                .wasm;
            std::fs::write(out, wasm).ok()?;
            true
        }
        "rust-v2" => {
            if !on_path("cargo") || !on_path("rustup") {
                return skip(guest, "cargo/rustup not on PATH");
            }
            // Asked in the guest's directory, so its rust-toolchain.toml
            // decides which toolchain's targets count (rustup installs a
            // pinned toolchain on first use).
            if !installed_targets(&dir)?.contains("wasm32-wasip3") {
                return skip(guest, "wasm32-wasip3 target not installed");
            }
            if !fetch_wit(&dir) {
                return Some(Vec::new());
            }
            if !command(
                &dir,
                "cargo",
                &["build", "--release", "--target", "wasm32-wasip3"],
                &[],
            ) {
                return Some(Vec::new());
            }
            let release = dir.join("target/wasm32-wasip3/release");
            let wasm = std::fs::read_dir(&release)
                .ok()?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|e| e == "wasm"))?;
            std::fs::copy(&wasm, out).ok()?;
            true
        }
        "ts" => {
            if !on_path("npm") {
                return skip(guest, "npm not on PATH");
            }
            if !fetch_wit(&dir) {
                return Some(Vec::new());
            }
            // A scaffold has no lockfile: npm install resolves it the way a
            // user's first build does.
            if !command(&dir, "npm", &["install", "--no-audit", "--no-fund"], &[])
                || !command(&dir, "npm", &["run", "build"], &[])
            {
                return Some(Vec::new());
            }
            std::fs::copy(dir.join("fn.wasm"), out).ok()?;
            true
        }
        "python" => {
            if !on_path("python3") {
                return skip(guest, "python3 not on PATH");
            }
            if !fetch_wit(&dir) {
                return Some(Vec::new());
            }
            // The venv is a kept cache; the requirements are installed every
            // run, which is cheap once they are satisfied.
            if (!dir.join(".venv").is_dir()
                && !command(&dir, "python3", &["-m", "venv", ".venv"], &[]))
                || !command(
                    &dir,
                    ".venv/bin/pip",
                    &["install", "--quiet", "-r", "requirements.txt"],
                    &[],
                )
            {
                return Some(Vec::new());
            }
            let venv = dir.join(".venv").canonicalize().ok()?;
            let venv = venv.to_string_lossy().into_owned();
            if !command(
                &dir,
                ".venv/bin/componentize-py",
                &[
                    "-d",
                    "wit",
                    "-w",
                    "function",
                    "componentize",
                    "app",
                    "-p",
                    "src",
                    "-p",
                    "src/gen",
                    "-o",
                    "fn.wasm",
                ],
                &[("VIRTUAL_ENV", venv.as_str())],
            ) {
                return Some(Vec::new());
            }
            std::fs::copy(dir.join("fn.wasm"), out).ok()?;
            true
        }
        _ => false,
    };
    assert!(built, "building the {guest} guest failed");
    Some(std::fs::read(out).expect("read built module"))
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

/// One case of the behavioural suite: the Input as JSON, the configuration
/// the composite resource carries under spec.config, the expected response
/// and the log lines the run must have written.
type Case = (
    &'static str,
    String,
    Option<serde_json::Value>,
    RunFunctionResponse,
    Vec<String>,
);

fn run_guest(guest: &str) {
    // CI's unit-test job is deliberately toolchain-free: the render jobs
    // prove each guest end to end, and the runner's preinstalled Go would
    // otherwise build the go guest with an unpinned toolchain.
    if std::env::var_os("SKIP_GUEST_BUILDS").is_some() {
        report_skip("SKIP_GUEST_BUILDS is set");
        return;
    }
    let logs = LOGS.clone();
    let dir = tempfile::tempdir().expect("tempdir");
    let file = format!("{guest}.wasm");
    let Some(wasm) = build_guest(guest, &dir.path().join(&file)) else {
        return; // toolchain not available
    };
    assert!(!wasm.is_empty(), "building the {guest} guest failed");

    // The runtime the guest runs through: --module-dir on the build, the
    // open operator policy, and the loopback ranges opened in the egress
    // block list so the greeting server can be dialled (every resolved
    // address is judged).
    let f = runtime::builder()
        .module_dir(dir.path())
        .policy(policy::permissive())
        .allow_loopback()
        .build();

    // What guestfn inspect and validate --resolve report: a component that
    // implements the world and exports its run.
    let shape = f.engine.inspect(&wasm).expect("inspect");
    assert!(shape.abi_error.is_none(), "{:?}", shape.abi_error);
    assert!(
        shape.exports.iter().any(|x| x.name == "run"),
        "guest exports {:?}, want run",
        shape.exports
    );

    let greetings = http::greeting_server();
    let greetings_host = greetings.split(':').next().expect("host").to_string();
    let egress_manifest = format!(
        r#"{{"contract":"wasm@1.0","requires":{{"egress":{{"http":[{{"host":"{greetings_host}","methods":["GET"]}}]}}}}}}"#
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

    let module_digest = digest_of(&wasm);
    // The Input references the configuration under spec.config of the
    // composite resource (config.from); the value travels there.
    let cases: Vec<Case> = vec![
        (
            "Default",
            format!(
                r#"{{"apiVersion":"guestfn.crossplane.io/v1","kind":"Input","module":{{"type":"Path","path":"{file}"}}}}"#
            ),
            None,
            response("hello my-xr"),
            vec![
                "Running function".to_string(),
                format!("digest=\"{module_digest}\""),
            ],
        ),
        (
            "Configured",
            format!(
                r#"{{"apiVersion":"guestfn.crossplane.io/v1","kind":"Input","module":{{"type":"Path","path":"{file}"}},"config":{{"from":"spec.config"}}}}"#
            ),
            Some(serde_json::json!({"greeting": "hi"})),
            response("hi my-xr"),
            vec!["Running function".to_string()],
        ),
        (
            "GreetingFromURL",
            format!(
                r#"{{"apiVersion":"guestfn.crossplane.io/v1","kind":"Input","module":{{"type":"OCI","oci":{{"ref":"{egress_ref}"}}}},"config":{{"from":"spec.config"}}}}"#
            ),
            Some(serde_json::json!({"greetingUrl": format!("http://{greetings}/en")})),
            response("howdy my-xr"),
            vec!["outcome=\"ok\"".to_string()],
        ),
        (
            "GreetingFromURLPath",
            format!(
                r#"{{"apiVersion":"guestfn.crossplane.io/v1","kind":"Input","module":{{"type":"Path","path":"{file}","manifestPath":"{path_manifest}"}},"config":{{"from":"spec.config"}}}}"#
            ),
            Some(serde_json::json!({"greetingUrl": format!("http://{greetings}/en")})),
            response("howdy my-xr"),
            vec!["outcome=\"ok\"".to_string()],
        ),
        (
            "GreetingURLWithoutGrant",
            format!(
                r#"{{"apiVersion":"guestfn.crossplane.io/v1","kind":"Input","module":{{"type":"Path","path":"{file}"}},"config":{{"from":"spec.config"}}}}"#
            ),
            Some(serde_json::json!({"greetingUrl": format!("http://{greetings}/en")})),
            fatal_response(),
            vec!["outcome=\"refused\"".to_string()],
        ),
        (
            "BadConfig",
            format!(
                r#"{{"apiVersion":"guestfn.crossplane.io/v1","kind":"Input","module":{{"type":"Path","path":"{file}"}},"config":{{"from":"spec.config"}}}}"#
            ),
            Some(serde_json::json!({"greeting": 7})),
            fatal_response(),
            Vec::new(),
        ),
    ];

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    for (name, input, config, want, want_logs) in cases {
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
                            "spec": {"config": config},
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
fn rust_v2_guest() {
    run_guest("rust-v2");
}

#[test]
fn ts_guest() {
    run_guest("ts");
}

#[test]
fn python_guest() {
    run_guest("python");
}

/// The end-to-end suite's test module (crates/e2e/guest) is a Cargo
/// workspace of its own, so neither `cargo test --workspace` nor the lint
/// over the workspace reaches it. Its unit tests and clippy run here, native
/// and on the toolchain its rust-toolchain.toml names, with the process
/// build's code included.
#[test]
fn the_end_to_end_guest_passes_its_own_tests() {
    if std::env::var_os("SKIP_GUEST_BUILDS").is_some() {
        report_skip("SKIP_GUEST_BUILDS is set");
        return;
    }
    // Its build script runs prost-build, which shells out to protoc.
    if !on_path("protoc") && std::env::var_os("PROTOC").is_none() {
        skip("end-to-end", "protoc not on PATH");
        return;
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../e2e/guest");
    // Its rust-toolchain.toml lists a target and components rustup may
    // have to install first, in turn with the rust scaffold's.
    if !on_path("rustup") || installed_targets(&dir).is_none() {
        skip("end-to-end", "rustup not on PATH");
        return;
    }
    assert!(
        command(
            &dir,
            "cargo",
            &["test", "--locked", "--features", "process"],
            &[]
        ),
        "the end-to-end guest's unit tests failed"
    );
    assert!(
        command(
            &dir,
            "cargo",
            &[
                "clippy",
                "--locked",
                "--all-targets",
                "--features",
                "process",
                "--",
                "-D",
                "warnings",
            ],
            &[],
        ),
        "clippy refused the end-to-end guest"
    );
}
