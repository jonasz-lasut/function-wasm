//! The validate-driven conformance harness: runs the Go runtime's `function
//! validate` as the reference and the Rust binary as the candidate over the
//! same fixtures (`cmd/function/testdata/validate/`) with the same flags,
//! and diffs stdout, stderr and the exit code.
//!
//! Every case is either expected to MATCH exactly - the parity contract, a
//! regression when it stops matching - or is a KNOWN GAP with a recorded
//! reason, expected to differ. A known gap that starts matching fails the
//! suite too, so the gap list only ever shrinks deliberately (a ratchet).
//!
//! The suite needs the Go toolchain (a CGo build of ./cmd/function); when
//! `go` is missing or the build fails, it skips rather than fails, like the
//! Go tree's guest tests skip without their toolchains.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

struct Case {
    name: &'static str,
    args: &'static [&'static str],
    stdin: &'static str,
    /// None: outputs must match exactly. Some(reason): a known gap that must
    /// still differ; remove the entry when the gap is closed.
    gap: Option<&'static str>,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "Admitted",
            args: &["testdata/validate/ok.yaml"],
            stdin: "",
            gap: None,
        },
        Case {
            name: "Refusals",
            args: &["testdata/validate/refusals.yaml"],
            stdin: "",
            gap: Some(
                "cedar-go and cedar-policy word parse errors differently; the wrong-shape refusal embeds Go's json decoder wording",
            ),
        },
        Case {
            name: "EgressRateLimitNegative",
            args: &[
                "testdata/validate/ok.yaml",
                "--egress-rate-limit-per-minute=-1",
            ],
            stdin: "",
            gap: None,
        },
        Case {
            name: "BadIPRule",
            args: &[
                "testdata/validate/ok.yaml",
                "--sandbox-policy-file",
                "testdata/validate/bad-iprule.cedar",
            ],
            stdin: "",
            gap: None,
        },
        Case {
            name: "FromWithoutXR",
            args: &["testdata/validate/from.yaml"],
            stdin: "",
            gap: None,
        },
        Case {
            name: "FromWithXR",
            args: &[
                "testdata/validate/from.yaml",
                "--xr",
                "testdata/validate/xr.yaml",
            ],
            stdin: "",
            gap: None,
        },
        Case {
            name: "UnknownFields",
            args: &["testdata/validate/unknown.yaml"],
            stdin: "",
            gap: None,
        },
        Case {
            name: "LimitsEqualCeiling",
            args: &[
                "testdata/validate/ok.yaml",
                "--module-timeout",
                "5s",
                "--module-memory-limit",
                "128",
            ],
            stdin: "",
            gap: None,
        },
        Case {
            name: "NoInputs",
            args: &["testdata/validate/function.yaml"],
            stdin: "",
            gap: None,
        },
        Case {
            name: "FunctionName",
            args: &[
                "testdata/validate/refusals.yaml",
                "--function-name",
                "function-auto-ready",
            ],
            stdin: "",
            gap: None,
        },
        Case {
            name: "Unparsable",
            args: &["testdata/validate/broken.yaml"],
            stdin: "",
            gap: None,
        },
        Case {
            name: "Missing",
            args: &["testdata/validate/nope.yaml"],
            stdin: "",
            gap: None,
        },
        Case {
            name: "Stdin",
            args: &["-"],
            stdin: "apiVersion: wasm.fn.crossplane.io/v1beta1\nkind: Input\nmodule: {type: OCI, oci: {ref: ghcr.io/example/greeter@sha256:3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a}}\n",
            gap: None,
        },
        Case {
            name: "ConcurrencyDetail",
            args: &["-"],
            stdin: "apiVersion: wasm.fn.crossplane.io/v1beta1\nkind: Input\nmodule: {type: OCI, oci: {ref: ghcr.io/example/greeter@sha256:3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a}}\nlimits: {concurrency: 2}\n",
            gap: None,
        },
        Case {
            name: "SeveralFiles",
            args: &[
                "testdata/validate/ok.yaml",
                "testdata/validate/from.yaml",
                "--xr",
                "testdata/validate/xr.yaml",
            ],
            stdin: "",
            gap: None,
        },
        Case {
            name: "SignatureRequired",
            args: &[
                "testdata/validate/signature.yaml",
                "--sandbox-policy-file",
                "testdata/validate/signature-policy.cedar",
                "--resolve",
            ],
            stdin: "",
            gap: None,
        },
    ]
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root")
}

/// Builds the Go reference binary once; None skips the suite.
fn go_binary() -> Option<&'static Path> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| {
        let root = repo_root();
        let out = root.join("rust/target/conformance/function-go");
        std::fs::create_dir_all(out.parent()?).ok()?;
        let status = Command::new("go")
            .args(["build", "-o"])
            .arg(&out)
            .arg("./cmd/function")
            .current_dir(&root)
            .status()
            .ok()?;
        status.success().then_some(out)
    })
    .as_deref()
}

struct Output {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_validate(bin: &Path, args: &[&str], stdin: &str, cwd: &Path) -> Output {
    let mut cmd = Command::new(bin);
    cmd.arg("validate")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The flags under test must come from the command line alone.
    for var in [
        "MODULE_DIR",
        "COSIGN_KEY",
        "SANDBOX_POLICY_FILE",
        "EGRESS_RATE_LIMIT_PER_MINUTE",
        "EGRESS_RATE_LIMIT_BURST",
        "TLS_SERVER_CERTS_DIR",
        "DEBUG",
    ] {
        cmd.env_remove(var);
    }
    let mut child = cmd.spawn().expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait");
    Output {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

fn diff(name: &str, go: &Output, rust: &Output) -> Option<String> {
    if go.stdout == rust.stdout && go.stderr == rust.stderr && go.code == rust.code {
        return None;
    }
    Some(format!(
        "{name}:\n--- go stdout (exit {})\n{}--- rust stdout (exit {})\n{}--- go stderr\n{}--- rust stderr\n{}",
        go.code, go.stdout, rust.code, rust.stdout, go.stderr, rust.stderr
    ))
}

fn assert_case(name: &str, gap: Option<&str>, go: &Output, rust: &Output) -> Option<String> {
    match (diff(name, go, rust), gap) {
        (None, None) => None,
        (Some(d), None) => Some(format!("PARITY REGRESSION (not a known gap)\n{d}")),
        (None, Some(reason)) => Some(format!(
            "{name}: known gap now matches the Go runtime - remove its entry (was: {reason})"
        )),
        (Some(_), Some(reason)) => {
            eprintln!("known gap {name}: {reason}");
            None
        }
    }
}

#[test]
fn validate_matches_the_go_runtime() {
    let Some(go) = go_binary() else {
        eprintln!("skipping: no Go toolchain or the Go build failed");
        return;
    };
    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = repo_root().join("cmd/function");

    let mut failures = Vec::new();
    for case in cases() {
        let go_out = run_validate(go, case.args, case.stdin, &cwd);
        let rust_out = run_validate(rust, case.args, case.stdin, &cwd);
        if let Some(f) = assert_case(case.name, case.gap, &go_out, &rust_out) {
            failures.push(f);
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The --resolve path over generated modules, the shape of the Go tree's
/// TestValidateResolve: a valid ABI module with a wasmfn.log import, one
/// missing wasmfn_run, one that is not wasm at all, and one missing file.
#[test]
fn validate_resolve_matches_the_go_runtime() {
    let Some(go) = go_binary() else {
        eprintln!("skipping: no Go toolchain or the Go build failed");
        return;
    };
    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = repo_root().join("cmd/function");

    let dir = tempfile::tempdir().expect("tempdir");
    let ok = wat::parse_str(
        r#"(module
          (import "wasmfn" "log" (func $log (param i32 i32 i32)))
          (memory (export "memory") 1)
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
          (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
    )
    .expect("wat");
    let bad = wat::parse_str(
        r#"(module (memory (export "memory") 1)
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8))"#,
    )
    .expect("wat");
    std::fs::write(dir.path().join("fn.wasm"), &ok).expect("write");
    std::fs::write(dir.path().join("bad.wasm"), &bad).expect("write");
    std::fs::write(dir.path().join("notwasm.wasm"), b"hello").expect("write");
    let composition = dir.path().join("composition.yaml");
    let step = |name: &str, path: &str| {
        format!(
            "  - step: {name}\n    functionRef: {{name: function-wasm}}\n    input:\n      apiVersion: wasm.fn.crossplane.io/v1beta1\n      kind: Input\n      module: {{type: Path, path: {path}}}\n"
        )
    };
    std::fs::write(
        &composition,
        format!(
            "apiVersion: apiextensions.crossplane.io/v1\nkind: Composition\nmetadata:\n  name: resolve\nspec:\n  pipeline:\n{}{}{}{}",
            step("ok", "fn.wasm"),
            step("bad", "bad.wasm"),
            step("notwasm", "notwasm.wasm"),
            step("missing", "missing.wasm"),
        ),
    )
    .expect("write composition");

    let composition = composition.display().to_string();
    let module_dir = dir.path().display().to_string();
    for output in ["text", "json"] {
        let args = [
            composition.as_str(),
            "--resolve",
            "--module-dir",
            module_dir.as_str(),
            "--output",
            output,
        ];
        let go_out = run_validate(go, &args, "", &cwd);
        let rust_out = run_validate(rust, &args, "", &cwd);
        if let Some(f) = assert_case(&format!("ResolvePath/{output}"), None, &go_out, &rust_out) {
            panic!("\n{f}");
        }
    }
}

/// The manifest path of the three-layer decision over generated fixtures,
/// the shape of the Go tree's TestValidateResolvePathManifest: a module
/// whose wasmfn.yaml requires a private /tmp, env bindings, or egress, with
/// and without an operator grant policy. Egress that both layers permit is
/// the one recorded gap: this runtime does not carry the egress client yet.
#[test]
fn validate_resolve_manifest_matches_the_go_runtime() {
    let Some(go) = go_binary() else {
        eprintln!("skipping: no Go toolchain or the Go build failed");
        return;
    };
    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = repo_root().join("cmd/function");

    let dir = tempfile::tempdir().expect("tempdir");
    let ok = wat::parse_str(
        r#"(module (memory (export "memory") 1)
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
          (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
    )
    .expect("wat");
    std::fs::write(dir.path().join("fn.wasm"), &ok).expect("write");
    let manifests: &[(&str, &str)] = &[
        (
            "tmp",
            "abi: 1\nname: greeter\nversion: 0.1.0\nrequires:\n  filesystem:\n    privateTmp: true\n",
        ),
        (
            "egress",
            "abi: 1\nname: greeter\nversion: 0.1.0\nrequires:\n  egress:\n    http:\n    - host: api.example.com\n      methods: [GET]\n",
        ),
        (
            "env",
            "abi: 1\nname: greeter\nversion: 0.1.0\nrequires:\n  env:\n  - name: API_TOKEN\n    fromCredential: {name: apikeys, key: token}\n",
        ),
    ];
    let mut compositions = Vec::new();
    for (name, manifest) in manifests {
        std::fs::write(dir.path().join(format!("{name}-manifest.yaml")), manifest).expect("write");
        let composition = dir.path().join(format!("{name}.yaml"));
        std::fs::write(
            &composition,
            format!(
                "apiVersion: apiextensions.crossplane.io/v1\nkind: Composition\nmetadata:\n  name: {name}\nspec:\n  pipeline:\n  - step: {name}\n    functionRef: {{name: function-wasm}}\n    input:\n      apiVersion: wasm.fn.crossplane.io/v1beta1\n      kind: Input\n      module: {{type: Path, path: fn.wasm, manifestPath: {name}-manifest.yaml}}\n"
            ),
        )
        .expect("write composition");
        compositions.push(composition.display().to_string());
    }

    let module_dir = dir.path().display().to_string();
    let mut failures = Vec::new();
    // Without an operator policy every requirement is refused, identically.
    for (i, composition) in compositions.iter().enumerate() {
        let args = [
            composition.as_str(),
            "--resolve",
            "--module-dir",
            module_dir.as_str(),
        ];
        let go_out = run_validate(go, &args, "", &cwd);
        let rust_out = run_validate(rust, &args, "", &cwd);
        let name = format!("ManifestNoPolicy/{}", manifests[i].0);
        if let Some(f) = assert_case(&name, None, &go_out, &rust_out) {
            failures.push(f);
        }
    }
    // With a permissive operator policy every ask - /tmp, egress, env - is
    // granted exactly as the Go runtime grants it.
    let gaps: &[Option<&str>] = &[None, None, None];
    for (i, composition) in compositions.iter().enumerate() {
        let args = [
            composition.as_str(),
            "--resolve",
            "--module-dir",
            module_dir.as_str(),
            "--sandbox-policy-file",
            "testdata/validate/policy-permissive.cedar",
        ];
        let go_out = run_validate(go, &args, "", &cwd);
        let rust_out = run_validate(rust, &args, "", &cwd);
        let name = format!("ManifestPermissive/{}", manifests[i].0);
        if let Some(f) = assert_case(&name, gaps[i], &go_out, &rust_out) {
            failures.push(f);
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The HTTP module source over a local server both binaries fetch from:
/// a module pinned by its stated digest, a wasmfn.yaml manifest by
/// reference (manifestURL/manifestDigest) decided by the three layers, and
/// a stated digest the download does not match.
#[test]
fn validate_resolve_http_source_matches_the_go_runtime() {
    use sha2::Digest as _;

    let Some(go) = go_binary() else {
        eprintln!("skipping: no Go toolchain or the Go build failed");
        return;
    };
    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = repo_root().join("cmd/function");

    let wasm = wat::parse_str(
        r#"(module (memory (export "memory") 1)
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
          (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
    )
    .expect("wat");
    let manifest =
        b"abi: 1\nname: greeter\nversion: 0.1.0\nrequires:\n  filesystem:\n    privateTmp: true\n"
            .to_vec();
    let digest = |b: &[u8]| format!("sha256:{}", hex::encode(sha2::Sha256::digest(b)));
    let (wasm_digest, manifest_digest) = (digest(&wasm), digest(&manifest));

    // A tiny HTTP file server on loopback; both binaries fetch from it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    {
        let wasm = wasm.clone();
        let manifest = manifest.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut conn = conn;
                let mut buf = [0u8; 2048];
                let n = std::io::Read::read(&mut conn, &mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                let body: &[u8] = if head.starts_with("GET /fn.wasm") {
                    &wasm
                } else if head.starts_with("GET /wasmfn.yaml") {
                    &manifest
                } else {
                    b""
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = std::io::Write::write_all(&mut conn, header.as_bytes());
                let _ = std::io::Write::write_all(&mut conn, body);
            }
        });
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let step = |name: &str, module: &str| {
        format!(
            "apiVersion: apiextensions.crossplane.io/v1\nkind: Composition\nmetadata:\n  name: {name}\nspec:\n  pipeline:\n  - step: {name}\n    functionRef: {{name: function-wasm}}\n    input:\n      apiVersion: wasm.fn.crossplane.io/v1beta1\n      kind: Input\n      module: {module}\n"
        )
    };
    let base = format!("http://127.0.0.1:{port}");
    let compositions: Vec<(String, Option<&str>, Vec<&str>)> = vec![
        (
            step(
                "plain",
                &format!("{{type: HTTP, http: {{url: {base}/fn.wasm, digest: {wasm_digest}}}}}"),
            ),
            None,
            vec![],
        ),
        (
            step(
                "with-manifest",
                &format!(
                    "{{type: HTTP, http: {{url: {base}/fn.wasm, digest: {wasm_digest}, manifestURL: {base}/wasmfn.yaml, manifestDigest: {manifest_digest}}}}}"
                ),
            ),
            None,
            vec![],
        ),
        (
            step(
                "granted",
                &format!(
                    "{{type: HTTP, http: {{url: {base}/fn.wasm, digest: {wasm_digest}, manifestURL: {base}/wasmfn.yaml, manifestDigest: {manifest_digest}}}}}"
                ),
            ),
            None,
            vec![
                "--sandbox-policy-file",
                "testdata/validate/policy-permissive.cedar",
            ],
        ),
        (
            step(
                "bad-digest",
                &format!(
                    "{{type: HTTP, http: {{url: {base}/fn.wasm, digest: sha256:{}}}}}",
                    "0".repeat(64)
                ),
            ),
            None,
            vec![],
        ),
    ];

    let mut failures = Vec::new();
    for (i, (composition, gap, extra)) in compositions.iter().enumerate() {
        let file = dir.path().join(format!("http-{i}.yaml"));
        std::fs::write(&file, composition).expect("write");
        let file = file.display().to_string();
        let mut args = vec![file.as_str(), "--resolve"];
        args.extend(extra);
        let go_out = run_validate(go, &args, "", &cwd);
        let rust_out = run_validate(rust, &args, "", &cwd);
        if let Some(f) = assert_case(&format!("HTTPSource/{i}"), *gap, &go_out, &rust_out) {
            failures.push(f);
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
