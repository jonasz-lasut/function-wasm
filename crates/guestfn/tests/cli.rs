//! End-to-end tests of the guestfn binary: push and inspect against an
//! in-memory registry with hand-assembled modules (no guest toolchain
//! needed), the offline scaffold, and the composition scaffold.

use std::path::Path;
use std::process::Command;

use function_wasm::oci::testregistry::{TestRegistry, serve};

/// A component implementing the world with a sync-lifted run that returns
/// an empty response.
const COMPONENT_WAT: &str = r#"(component
  (core module $m
    (memory (export "memory") 1)
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 4096)
    (func (export "run") (param i32 i32) (result i32)
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72) (i32.const 0))
      (i32.const 64)))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory (core memory $i "memory")) (realloc (core func $i "cabi_realloc"))))
)"#;

/// A component that does not implement the world.
const NO_RUN_WAT: &str = "(component)";

/// A core module in ABI v1's shape: what a guest built before 1.0.0 is.
const CORE_MODULE_WAT: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
  (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#;

const MANIFEST_YAML: &str = "abi: 2
name: greeter
version: v0.1.0
description: Greets the composite resource
requires:
  egress:
    http:
      - host: example.com
        methods: [GET]
  credentials: [cmdb]
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
        referrers_api: false,
        basic: false,
    })
}

#[test]
fn push_then_inspect_and_show() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        wat::parse_str(COMPONENT_WAT).expect("wat"),
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
    assert!(stdout.contains("credentials:\n  - cmdb"), "{stdout}");
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
    assert!(
        stdout.contains("manifest: greeter v0.1.0, requires egress example.com, credentials cmdb"),
        "{stdout}"
    );
    assert!(
        stdout.contains("org.opencontainers.image.title=greeter"),
        "{stdout}"
    );

    // Pulled and read as the runtime would.
    let (stdout, stderr, ok) = guestfn(dir.path(), &["inspect", &pinned, "--pull"]);
    assert!(ok, "inspect --pull failed: {stderr}");
    assert!(stdout.contains("ABI v2"), "{stdout}");
    assert!(stdout.contains("exports: run (func)"), "{stdout}");

    // The manifest layer, shown without pulling the module.
    let (stdout, stderr, ok) = guestfn(dir.path(), &["manifest", "show", &pinned]);
    assert!(ok, "manifest show failed: {stderr}");
    assert!(stdout.contains("name: greeter"), "{stdout}");
    assert!(stdout.contains("host: example.com"), "{stdout}");
    assert!(stdout.contains("- cmdb"), "{stdout}");
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
    assert!(
        stderr.contains("component does not implement the wasmfn:function@2.0.0-draft world"),
        "{stderr}"
    );
}

/// A core module - a guest built before 1.0.0 - is refused by every
/// command that reads a module, with the runtime's own sentence.
#[test]
fn a_core_module_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        wat::parse_str(CORE_MODULE_WAT).expect("wat"),
    )
    .expect("write");
    let (_, stderr, ok) = guestfn(dir.path(), &["inspect", "fn.wasm"]);
    assert!(!ok);
    assert!(
        stderr.contains(&format!(
            "fn.wasm: {}",
            function_wasm_engine::CORE_MODULE_REFUSAL
        )),
        "{stderr}"
    );

    let addr = empty_registry();
    let (_, stderr, ok) = guestfn(dir.path(), &["push", &format!("{addr}/example/greeter:v1")]);
    assert!(!ok);
    assert!(
        stderr.contains("would be refused by the runtime and is not pushed"),
        "{stderr}"
    );
    assert!(
        stderr.contains(function_wasm_engine::CORE_MODULE_REFUSAL),
        "{stderr}"
    );
}

#[test]
fn inspect_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        wat::parse_str(COMPONENT_WAT).expect("wat"),
    )
    .expect("write");
    let (stdout, stderr, ok) = guestfn(dir.path(), &["inspect", "fn.wasm"]);
    assert!(ok, "inspect failed: {stderr}");
    assert!(stdout.contains("ABI v2"), "{stdout}");
    assert!(stdout.contains("exports: run (func)"), "{stdout}");
    assert!(stdout.contains("imports: none"), "{stdout}");

    let (stdout, _, ok) = guestfn(dir.path(), &["inspect", "fn.wasm", "--output", "json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(v["module"]["abi"], "v2");
    assert_eq!(v["module"]["exports"][0]["name"], "run");
    assert_eq!(v["module"]["exports"][0]["kind"], "func");
    assert!(v["module"].get("warnings").is_none());
}

#[test]
fn manifest_validate() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("wasmfn.yaml"), MANIFEST_YAML).expect("write");
    let (stdout, stderr, ok) = guestfn(dir.path(), &["manifest", "validate"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains(
            "wasmfn.yaml: valid (greeter v0.1.0, requires egress example.com, credentials cmdb;"
        ),
        "{stdout}"
    );

    std::fs::write(dir.path().join("bad.yaml"), "abi: 2\nverion: nope\n").expect("write");
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

/// The component flavours have no Go module path: init names the project
/// after its directory and runs no toolchain.
#[test]
fn init_names_a_component_project_after_its_directory() {
    let cases = [
        ("ts", "my-ts", "package", "package.json"),
        ("python", "my-py", "project", "requirements.txt"),
    ];
    for (lang, name, kind, manifest) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let (stdout, stderr, ok) = guestfn(dir.path(), &["init", name, "--lang", lang]);
        assert!(ok, "{lang}: init failed: {stderr}");
        assert!(
            stdout.contains(&format!("Created {name} ({kind} {name})")),
            "{lang}: {stdout}"
        );
        let project = dir.path().join(name);
        let wasmfn = std::fs::read_to_string(project.join("wasmfn.yaml")).expect("wasmfn.yaml");
        assert!(
            wasmfn.contains(&format!("name: {name}\n")),
            "{lang}: {wasmfn}"
        );
        assert!(
            project.join(manifest).is_file(),
            "{lang}: missing {manifest}"
        );
        assert!(
            project.join("wit/world.wit").is_file(),
            "{lang}: missing wit/world.wit"
        );
    }
}

#[test]
fn scaffold_composition_from_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        wat::parse_str(COMPONENT_WAT).expect("wat"),
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
    assert!(
        stdout.contains("#   permit (principal, action == Action::\"spendCredential\", resource == Credential::\"cmdb\");"),
        "{stdout}"
    );

    let (stdout, _, ok) = guestfn(dir.path(), &["scaffold", "composition", "--full"]);
    assert!(ok);
    assert!(stdout.contains("kind: Composition"), "{stdout}");
    assert!(stdout.contains("mode: Pipeline"), "{stdout}");
}
