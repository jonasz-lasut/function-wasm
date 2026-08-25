//! End-to-end tests of the guestfn binary: push and inspect against an
//! in-memory registry with hand-assembled modules (no guest toolchain
//! needed), the offline scaffold, and the composition scaffold.

use std::path::Path;
use std::process::Command;

use function_wasm::oci::testregistry::{TestRegistry, serve};

const ABI_V1_WAT: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
  (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#;

const NO_RUN_WAT: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8))"#;

const MANIFEST_YAML: &str = "abi: 1
name: greeter
version: v0.1.0
description: Greets the composite resource
requires:
  egress:
    http:
      - host: example.com
        methods: [GET]
config:
  schema:
    type: object
    properties:
      greeting:
        type: string
        default: hello
";

fn guestfn(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_guestfn"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run guestfn");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn empty_registry() -> String {
    serve(TestRegistry {
        manifests: Default::default(),
        blobs: Default::default(),
        bearer: false,
    })
}

#[test]
fn push_then_inspect_and_show() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        wat::parse_str(ABI_V1_WAT).expect("wat"),
    )
    .expect("write");
    std::fs::write(dir.path().join("wasmfn.yaml"), MANIFEST_YAML).expect("write");
    let addr = empty_registry();
    let reference = format!("{addr}/example/greeter:v1");

    let (stdout, stderr, ok) = guestfn(dir.path(), &["push", &reference]);
    assert!(ok, "push failed: {stderr}");
    assert!(stdout.contains("Pushed "), "{stdout}");
    assert!(
        stdout.contains(&format!("ref: {reference}@sha256:")),
        "{stdout}"
    );
    assert!(stdout.contains("requires:"), "{stdout}");
    assert!(stdout.contains("host: example.com"), "{stdout}");
    let pinned = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Pushed "))
        .expect("pinned reference")
        .to_string();

    // The pushed artifact, described from its manifest.
    let (stdout, stderr, ok) = guestfn(dir.path(), &["inspect", &pinned]);
    assert!(ok, "inspect failed: {stderr}");
    assert!(stdout.contains("layer: application/wasm"), "{stdout}");
    assert!(
        stdout.contains("layer: application/vnd.wasmfn.manifest.v1+json"),
        "{stdout}"
    );
    assert!(
        stdout.contains("module layer: application/wasm"),
        "{stdout}"
    );
    assert!(stdout.contains("manifest: greeter v0.1.0"), "{stdout}");
    assert!(
        stdout.contains("org.opencontainers.image.title=greeter"),
        "{stdout}"
    );

    // Pulled and read as the runtime would.
    let (stdout, stderr, ok) = guestfn(dir.path(), &["inspect", &pinned, "--pull"]);
    assert!(ok, "inspect --pull failed: {stderr}");
    assert!(stdout.contains("ABI v1"), "{stdout}");
    assert!(stdout.contains("exports: memory (memory)"), "{stdout}");

    // The manifest layer, shown without pulling the module.
    let (stdout, stderr, ok) = guestfn(dir.path(), &["manifest", "show", &pinned]);
    assert!(ok, "manifest show failed: {stderr}");
    assert!(stdout.contains("name: greeter"), "{stdout}");
    assert!(stdout.contains("host: example.com"), "{stdout}");
}

#[test]
fn push_refuses_a_module_the_runtime_would_refuse() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        wat::parse_str(NO_RUN_WAT).expect("wat"),
    )
    .expect("write");
    let addr = empty_registry();
    let (_, stderr, ok) = guestfn(dir.path(), &["push", &format!("{addr}/example/greeter:v1")]);
    assert!(!ok);
    assert!(
        stderr.contains("would be refused by the runtime and is not pushed"),
        "{stderr}"
    );
    assert!(stderr.contains("wasmfn_run"), "{stderr}");
}

#[test]
fn inspect_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        wat::parse_str(ABI_V1_WAT).expect("wat"),
    )
    .expect("write");
    let (stdout, stderr, ok) = guestfn(dir.path(), &["inspect", "fn.wasm"]);
    assert!(ok, "inspect failed: {stderr}");
    assert!(stdout.contains("ABI v1"), "{stdout}");
    assert!(stdout.contains("wasmfn_alloc (i32) -> (i32)"), "{stdout}");
    assert!(
        stdout.contains("wasmfn_run (i32, i32) -> (i64)"),
        "{stdout}"
    );

    let (stdout, _, ok) = guestfn(dir.path(), &["inspect", "fn.wasm", "--output", "json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(v["module"]["abi"], "v1");
    assert_eq!(v["module"]["memories"][0]["minPages"], 1);
}

#[test]
fn manifest_validate() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("wasmfn.yaml"), MANIFEST_YAML).expect("write");
    let (stdout, stderr, ok) = guestfn(dir.path(), &["manifest", "validate"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("wasmfn.yaml: valid (greeter v0.1.0"),
        "{stdout}"
    );

    std::fs::write(dir.path().join("bad.yaml"), "abi: 1\nverion: nope\n").expect("write");
    let (_, stderr, ok) = guestfn(dir.path(), &["manifest", "validate", "bad.yaml"]);
    assert!(!ok);
    assert!(stderr.contains("unknown field \"verion\""), "{stderr}");
}

#[test]
fn init_offline_writes_a_project() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (stdout, stderr, ok) = guestfn(
        dir.path(),
        &[
            "init",
            "my-fn",
            "--lang",
            "go",
            "--module",
            "github.com/me/my-fn",
            "--offline",
        ],
    );
    assert!(ok, "init failed: {stderr}");
    assert!(
        stdout.contains("Created my-fn (module github.com/me/my-fn)"),
        "{stdout}"
    );
    for f in [
        "go.mod",
        "main.go",
        "fn.go",
        "wasmfn.yaml",
        "internal/wasmfn/register.go",
    ] {
        assert!(dir.path().join("my-fn").join(f).is_file(), "missing {f}");
    }
    let gomod = std::fs::read_to_string(dir.path().join("my-fn/go.mod")).expect("go.mod");
    assert!(gomod.contains("module github.com/me/my-fn"), "{gomod}");

    // A second init into the same directory refuses to overwrite.
    let (_, stderr, ok) = guestfn(
        dir.path(),
        &[
            "init",
            "my-fn",
            "--module",
            "github.com/me/my-fn",
            "--offline",
        ],
    );
    assert!(!ok);
    assert!(stderr.contains("already exists"), "{stderr}");
}

#[test]
fn scaffold_composition_from_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        wat::parse_str(ABI_V1_WAT).expect("wat"),
    )
    .expect("write");
    std::fs::write(dir.path().join("wasmfn.yaml"), MANIFEST_YAML).expect("write");
    let (stdout, stderr, ok) = guestfn(dir.path(), &["scaffold", "composition"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("- step: greeter"), "{stdout}");
    assert!(stdout.contains("type: Path"), "{stdout}");
    assert!(stdout.contains("path: fn.wasm"), "{stdout}");
    assert!(stdout.contains("greeting: hello"), "{stdout}");
    assert!(
        stdout.contains("#   permit (principal, action == Action::\"grantEgress\", resource in HostPattern::\"example.com\");"),
        "{stdout}"
    );

    let (stdout, _, ok) = guestfn(dir.path(), &["scaffold", "composition", "--full"]);
    assert!(ok);
    assert!(stdout.contains("kind: Composition"), "{stdout}");
    assert!(stdout.contains("mode: Pipeline"), "{stdout}");
}
