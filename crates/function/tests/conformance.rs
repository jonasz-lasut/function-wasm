//! The validate conformance suite: runs `function validate` over the
//! fixture corpus (`testdata/validate/`) and generated modules, servers and
//! registries, and compares stdout, stderr and the exit code against
//! recorded goldens under `testdata/conformance/`.
//!
//! The goldens were recorded from this runtime the day it last diffed
//! byte-identical against the Go runtime's own `function validate` (the
//! original differential harness, retired with the Go tree) - so they are
//! the Go runtime's words wherever parity held, and a change is a parity
//! regression until re-recorded deliberately with UPDATE_CONFORMANCE=1.
//! Run-specific values (temp directories, loopback ports) are replaced
//! with stable placeholders before comparing.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Case {
    name: &'static str,
    args: &'static [&'static str],
    stdin: &'static str,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "Admitted",
            args: &["testdata/validate/ok.yaml"],
            stdin: "",
        },
        Case {
            name: "Refusals",
            args: &["testdata/validate/refusals.yaml"],
            stdin: "",
        },
        Case {
            name: "EgressRateLimitNegative",
            args: &[
                "testdata/validate/ok.yaml",
                "--egress-rate-limit-per-minute=-1",
            ],
            stdin: "",
        },
        Case {
            name: "BadIPRule",
            args: &[
                "testdata/validate/ok.yaml",
                "--sandbox-policy-file",
                "testdata/validate/bad-iprule.cedar",
            ],
            stdin: "",
        },
        Case {
            name: "FromWithoutXR",
            args: &["testdata/validate/from.yaml"],
            stdin: "",
        },
        Case {
            name: "FromWithXR",
            args: &[
                "testdata/validate/from.yaml",
                "--xr",
                "testdata/validate/xr.yaml",
            ],
            stdin: "",
        },
        Case {
            name: "UnknownFields",
            args: &["testdata/validate/unknown.yaml"],
            stdin: "",
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
        },
        Case {
            name: "NoInputs",
            args: &["testdata/validate/function.yaml"],
            stdin: "",
        },
        Case {
            name: "FunctionName",
            args: &[
                "testdata/validate/refusals.yaml",
                "--function-name",
                "function-auto-ready",
            ],
            stdin: "",
        },
        Case {
            name: "Unparsable",
            args: &["testdata/validate/broken.yaml"],
            stdin: "",
        },
        Case {
            name: "Missing",
            args: &["testdata/validate/nope.yaml"],
            stdin: "",
        },
        Case {
            name: "Stdin",
            args: &["-"],
            stdin: "apiVersion: wasm.fn.crossplane.io/v1beta1\nkind: Input\nmodule: {type: OCI, oci: {ref: ghcr.io/example/greeter@sha256:3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a}}\n",
        },
        Case {
            name: "ConcurrencyDetail",
            args: &["-"],
            stdin: "apiVersion: wasm.fn.crossplane.io/v1beta1\nkind: Input\nmodule: {type: OCI, oci: {ref: ghcr.io/example/greeter@sha256:3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a3f2a}}\nlimits: {concurrency: 2}\n",
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
        },
    ]
}

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
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

/// Compares one case's output against its golden; UPDATE_CONFORMANCE=1
/// records instead. subs replace run-specific values with stable
/// placeholders.
fn assert_golden(name: &str, out: &Output, subs: &[(String, &str)]) -> Option<String> {
    let mut rendered = format!(
        "exit {}\n--- stdout\n{}--- stderr\n{}",
        out.code, out.stdout, out.stderr
    );
    for (needle, placeholder) in subs {
        rendered = rendered.replace(needle.as_str(), placeholder);
    }
    let path = crate_dir()
        .join("testdata/conformance")
        .join(format!("{}.golden", name.replace('/', "-")));
    if std::env::var_os("UPDATE_CONFORMANCE").is_some() {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, &rendered).expect("write golden");
        return None;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{name}: cannot read {} ({e}); record with UPDATE_CONFORMANCE=1 cargo test",
            path.display()
        )
    });
    if want == rendered {
        return None;
    }
    Some(format!(
        "{name}: output differs from {}\n--- want\n{want}\n--- got\n{rendered}\n(re-record deliberately with UPDATE_CONFORMANCE=1 cargo test)",
        path.display()
    ))
}

#[test]
fn validate_matches_the_goldens() {
    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = crate_dir();

    let mut failures = Vec::new();
    for case in cases() {
        let rust_out = run_validate(rust, case.args, case.stdin, &cwd);
        if let Some(f) = assert_golden(case.name, &rust_out, &[]) {
            failures.push(f);
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The --resolve path over generated modules, the shape of the Go tree's
/// TestValidateResolve: a valid ABI module with a wasmfn.log import, one
/// missing wasmfn_run, one that is not wasm at all, and one missing file.
#[test]
fn validate_resolve_matches_the_goldens() {
    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = crate_dir();

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
    // A wasmtime compiled artifact (.cwasm) named as a module source: the
    // runtime refuses it by name rather than as malformed wasm.
    let artifact = {
        let e = function_wasm_engine::Engine::new(function_wasm_engine::Config::default())
            .expect("engine");
        let m = e.compile(&ok).expect("compile");
        e.serialize(&m).expect("serialize")
    };
    std::fs::write(dir.path().join("precompiled.wasm"), &artifact).expect("write");
    let composition = dir.path().join("composition.yaml");
    let step = |name: &str, path: &str| {
        format!(
            "  - step: {name}\n    functionRef: {{name: function-wasm}}\n    input:\n      apiVersion: wasm.fn.crossplane.io/v1beta1\n      kind: Input\n      module: {{type: Path, path: {path}}}\n"
        )
    };
    std::fs::write(
        &composition,
        format!(
            "apiVersion: apiextensions.crossplane.io/v1\nkind: Composition\nmetadata:\n  name: resolve\nspec:\n  pipeline:\n{}{}{}{}{}",
            step("ok", "fn.wasm"),
            step("bad", "bad.wasm"),
            step("notwasm", "notwasm.wasm"),
            step("missing", "missing.wasm"),
            step("precompiled", "precompiled.wasm"),
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
        let rust_out = run_validate(rust, &args, "", &cwd);
        let subs = [(module_dir.clone(), "<DIR>")];
        if let Some(f) = assert_golden(&format!("ResolvePath/{output}"), &rust_out, &subs) {
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
fn validate_resolve_manifest_matches_the_goldens() {
    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = crate_dir();

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
        let rust_out = run_validate(rust, &args, "", &cwd);
        let name = format!("ManifestNoPolicy/{}", manifests[i].0);
        if let Some(f) = assert_golden(&name, &rust_out, &[(module_dir.clone(), "<DIR>")]) {
            failures.push(f);
        }
    }
    // With a permissive operator policy every ask - /tmp, egress, env - is
    // granted exactly as the Go runtime granted it.
    for (i, composition) in compositions.iter().enumerate() {
        let args = [
            composition.as_str(),
            "--resolve",
            "--module-dir",
            module_dir.as_str(),
            "--sandbox-policy-file",
            "testdata/validate/policy-permissive.cedar",
        ];
        let rust_out = run_validate(rust, &args, "", &cwd);
        let name = format!("ManifestPermissive/{}", manifests[i].0);
        if let Some(f) = assert_golden(&name, &rust_out, &[(module_dir.clone(), "<DIR>")]) {
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
fn validate_resolve_http_source_matches_the_goldens() {
    use sha2::Digest as _;

    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = crate_dir();

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
    let compositions: Vec<(String, Vec<&str>)> = vec![
        (
            step(
                "plain",
                &format!("{{type: HTTP, http: {{url: {base}/fn.wasm, digest: {wasm_digest}}}}}"),
            ),
            vec![],
        ),
        (
            step(
                "with-manifest",
                &format!(
                    "{{type: HTTP, http: {{url: {base}/fn.wasm, digest: {wasm_digest}, manifestURL: {base}/wasmfn.yaml, manifestDigest: {manifest_digest}}}}}"
                ),
            ),
            vec![],
        ),
        (
            step(
                "granted",
                &format!(
                    "{{type: HTTP, http: {{url: {base}/fn.wasm, digest: {wasm_digest}, manifestURL: {base}/wasmfn.yaml, manifestDigest: {manifest_digest}}}}}"
                ),
            ),
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
            vec![],
        ),
    ];

    let mut failures = Vec::new();
    for (i, (composition, extra)) in compositions.iter().enumerate() {
        let file = dir.path().join(format!("http-{i}.yaml"));
        std::fs::write(&file, composition).expect("write");
        let file = file.display().to_string();
        let mut args = vec![file.as_str(), "--resolve"];
        args.extend(extra);
        let rust_out = run_validate(rust, &args, "", &cwd);
        let subs = [
            (dir.path().display().to_string(), "<DIR>"),
            (format!("127.0.0.1:{port}"), "<SERVER>"),
        ];
        if let Some(f) = assert_golden(&format!("HTTPSource/{i}"), &rust_out, &subs) {
            failures.push(f);
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The OCI source over a local registry both binaries pull from
/// (go-containerregistry speaks plain HTTP to 127.0.0.1, as this runtime
/// does): the module layer fetched and inspected, the artifact's manifest
/// layer decided by the three layers.
#[test]
fn validate_resolve_oci_source_matches_the_goldens() {
    use sha2::Digest as _;

    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = crate_dir();

    let wasm = wat::parse_str(
        r#"(module (memory (export "memory") 1)
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
          (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
    )
    .expect("wat");
    let module_manifest =
        br#"{"abi":1,"name":"greeter","version":"0.1.0","requires":{"filesystem":{"privateTmp":true}}}"#;
    let digest_of = |b: &[u8]| format!("sha256:{}", hex::encode(sha2::Sha256::digest(b)));
    let wasm_digest = digest_of(&wasm);
    let manifest_digest = digest_of(module_manifest);
    let config = br#"{}"#;
    let manifest_json = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(config), "size": config.len()},
        "layers": [
            {"mediaType": "application/wasm", "digest": wasm_digest, "size": wasm.len()},
            {"mediaType": "application/vnd.wasmfn.manifest.v1+json", "digest": manifest_digest, "size": module_manifest.len()},
        ],
    }))
    .expect("manifest json");
    let artifact_digest = digest_of(&manifest_json);

    // A tiny anonymous registry on loopback.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    {
        let blobs: std::collections::HashMap<String, Vec<u8>> = [
            (wasm_digest.clone(), wasm.clone()),
            (manifest_digest.clone(), module_manifest.to_vec()),
        ]
        .into();
        let manifests: std::collections::HashMap<String, Vec<u8>> =
            [(artifact_digest.clone(), manifest_json.clone())].into();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut conn = conn;
                let mut buf = [0u8; 4096];
                let n = std::io::Read::read(&mut conn, &mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                let (status, body, ctype): (&str, Vec<u8>, &str) =
                    if let Some(d) = path.split("/manifests/").nth(1) {
                        match manifests.get(d) {
                            Some(m) => (
                                "200 OK",
                                m.clone(),
                                "application/vnd.oci.image.manifest.v1+json",
                            ),
                            None => ("404 Not Found", Vec::new(), "text/plain"),
                        }
                    } else if let Some(d) = path.split("/blobs/").nth(1) {
                        match blobs.get(d) {
                            Some(b) => ("200 OK", b.clone(), "application/octet-stream"),
                            None => ("404 Not Found", Vec::new(), "text/plain"),
                        }
                    } else {
                        ("200 OK", Vec::new(), "text/plain")
                    };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = std::io::Write::write_all(&mut conn, header.as_bytes());
                let _ = std::io::Write::write_all(&mut conn, &body);
            }
        });
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let composition = dir.path().join("oci.yaml");
    std::fs::write(
        &composition,
        format!(
            "apiVersion: apiextensions.crossplane.io/v1\nkind: Composition\nmetadata:\n  name: oci\nspec:\n  pipeline:\n  - step: oci\n    functionRef: {{name: function-wasm}}\n    input:\n      apiVersion: wasm.fn.crossplane.io/v1beta1\n      kind: Input\n      module: {{type: OCI, oci: {{ref: {addr}/example/greeter@{artifact_digest}}}}}\n"
        ),
    )
    .expect("write");
    let composition = composition.display().to_string();

    let mut failures = Vec::new();
    for (name, extra) in [
        ("NoPolicy", vec![]),
        (
            "Granted",
            vec![
                "--sandbox-policy-file",
                "testdata/validate/policy-permissive.cedar",
            ],
        ),
    ] {
        let mut args = vec![composition.as_str(), "--resolve"];
        args.extend(extra);
        let rust_out = run_validate(rust, &args, "", &cwd);
        let subs = [
            (dir.path().display().to_string(), "<DIR>"),
            (addr.to_string(), "<REGISTRY>"),
        ];
        if let Some(f) = assert_golden(&format!("OCISource/{name}"), &rust_out, &subs) {
            failures.push(f);
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// Cosign verification against the Go runtime: the harness signs an
/// artifact with a P-256 key (ASN.1 DER ECDSA over SHA-256 of the
/// simple-signing payload, the shape cosign's key-based flow writes) and
/// both binaries verify it under the legacy all-or-nothing --cosign-key.
/// The unsigned refusal is a recorded wording-only gap.
#[test]
fn validate_resolve_cosign_matches_the_goldens() {
    use p256::ecdsa::signature::Signer as _;
    use p256::pkcs8::EncodePublicKey as _;
    use sha2::Digest as _;

    let rust = Path::new(env!("CARGO_BIN_EXE_function"));
    let cwd = crate_dir();
    let digest_of = |b: &[u8]| format!("sha256:{}", hex::encode(sha2::Sha256::digest(b)));

    let wasm = wat::parse_str(
        r#"(module (memory (export "memory") 1)
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
          (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
    )
    .expect("wat");
    let config = br#"{}"#;
    let module_manifest = |name: &str| {
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(config), "size": config.len()},
            "layers": [{"mediaType": "application/wasm", "digest": digest_of(&wasm), "size": wasm.len()}],
            "annotations": {"org.opencontainers.image.title": name},
        }))
        .expect("manifest json")
    };
    let signed_manifest = module_manifest("signed");
    let unsigned_manifest = module_manifest("unsigned");
    let (signed_digest, unsigned_digest) =
        (digest_of(&signed_manifest), digest_of(&unsigned_manifest));

    // Sign the signed artifact's digest.
    let key = p256::ecdsa::SigningKey::from_bytes((&[5u8; 32]).into()).expect("key");
    let payload = serde_json::to_vec(&serde_json::json!({
        "critical": {
            "identity": {"docker-reference": ""},
            "image": {"docker-manifest-digest": signed_digest},
            "type": "cosign container image signature",
        },
        "optional": null,
    }))
    .expect("payload");
    let signature: p256::ecdsa::DerSignature = key.sign(&payload);
    use base64::Engine as _;
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
    let sig_manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(config), "size": config.len()},
        "layers": [{
            "mediaType": "application/vnd.dev.cosign.simplesigning.v1+json",
            "digest": digest_of(&payload),
            "size": payload.len(),
            "annotations": {"dev.cosignproject.cosign/signature": sig_b64},
        }],
    }))
    .expect("sig manifest");

    // The registry: both artifacts, the signature under its cosign tag.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    {
        let mut manifests: std::collections::HashMap<String, Vec<u8>> = [
            (signed_digest.clone(), signed_manifest),
            (unsigned_digest.clone(), unsigned_manifest),
            (
                format!("{}.sig", signed_digest.replacen(':', "-", 1)),
                sig_manifest,
            ),
        ]
        .into();
        let blobs: std::collections::HashMap<String, Vec<u8>> = [
            (digest_of(&wasm), wasm.clone()),
            (digest_of(&payload), payload.clone()),
        ]
        .into();
        manifests.shrink_to_fit();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut conn = conn;
                let mut buf = [0u8; 4096];
                let n = std::io::Read::read(&mut conn, &mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                let (status, body): (&str, Vec<u8>) =
                    if let Some(d) = path.split("/manifests/").nth(1) {
                        match manifests.get(d) {
                            Some(m) => ("200 OK", m.clone()),
                            None => ("404 Not Found", Vec::new()),
                        }
                    } else if let Some(d) = path.split("/blobs/").nth(1) {
                        match blobs.get(d) {
                            Some(b) => ("200 OK", b.clone()),
                            None => ("404 Not Found", Vec::new()),
                        }
                    } else {
                        ("200 OK", Vec::new())
                    };
                let ctype = "application/vnd.oci.image.manifest.v1+json";
                let dcd = digest_of(&body);
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nDocker-Content-Digest: {dcd}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = std::io::Write::write_all(&mut conn, header.as_bytes());
                let _ = std::io::Write::write_all(&mut conn, &body);
            }
        });
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let key_path = dir.path().join("cosign.pub");
    std::fs::write(
        &key_path,
        key.verifying_key()
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .expect("pem"),
    )
    .expect("write key");
    let key_path = key_path.display().to_string();

    let composition = |name: &str, digest: &str| {
        format!(
            "apiVersion: apiextensions.crossplane.io/v1\nkind: Composition\nmetadata:\n  name: {name}\nspec:\n  pipeline:\n  - step: {name}\n    functionRef: {{name: function-wasm}}\n    input:\n      apiVersion: wasm.fn.crossplane.io/v1beta1\n      kind: Input\n      module: {{type: OCI, oci: {{ref: {addr}/example/greeter@{digest}}}}}\n"
        )
    };
    let mut failures = Vec::new();
    for (name, digest) in [("Signed", &signed_digest), ("Unsigned", &unsigned_digest)] {
        let file = dir.path().join(format!("{name}.yaml"));
        std::fs::write(&file, composition(name, digest)).expect("write");
        let file = file.display().to_string();
        let args = vec![
            file.as_str(),
            "--resolve",
            "--cosign-key",
            key_path.as_str(),
        ];
        let rust_out = run_validate(rust, &args, "", &cwd);
        let subs = [
            (dir.path().display().to_string(), "<DIR>"),
            (addr.to_string(), "<REGISTRY>"),
        ];
        if let Some(f) = assert_golden(&format!("Cosign/{name}"), &rust_out, &subs) {
            failures.push(f);
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
