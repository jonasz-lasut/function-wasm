//! End-to-end tests of the guestfn binary: push and inspect against an
//! in-memory registry with hand-assembled modules (no guest toolchain
//! needed), the offline scaffold, and the composition scaffold.

use std::path::Path;
use std::process::Command;

use guestfn_engine_wasm::componentize::decode_wit_package;
use guestfn_runtime::testing::guests::{
    encoded_wit_package, encoded_wit_package_over, tiny_component,
};
use guestfn_runtime::testing::registry::Registry;

/// A component that does not implement the world.
const NO_RUN_WAT: &str = "(component)";

/// A core module in ABI v1's shape: what a guest built before 1.0.0 is.
const CORE_MODULE_WAT: &str = r#"(module
  (memory (export "memory") 1)
  (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
  (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#;

const MANIFEST_YAML: &str = "contract: wasm@1.0
name: greeter
version: v0.1.0
description: Greets the composite resource
requires:
  egress:
    http:
      - host: example.com
        methods: [GET]
  credentials: [cmdb]
  resources:
    - kind: apiextensions.crossplane.io/EnvironmentConfig
      by: [name]
composes:
  - kind: policy/PodDisruptionBudget
    changes: [add]
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
    Registry::new().serve()
}

#[test]
fn push_then_inspect_and_show() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("fn.wasm"), tiny_component()).expect("write");
    std::fs::write(dir.path().join("guestfn.yaml"), MANIFEST_YAML).expect("write");
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
        stdout.contains("layer: application/vnd.guestfn.manifest.v1+json"),
        "{stdout}"
    );
    assert!(
        stdout.contains("module layer: application/wasm"),
        "{stdout}"
    );
    assert!(
        stdout.contains("manifest: greeter v0.1.0, requires egress example.com, credentials cmdb, resources apiextensions.crossplane.io/EnvironmentConfig (name), composes policy/PodDisruptionBudget (add)"),
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

/// A module image for two platforms is an image index: push appends the
/// manifest to each platform's image, and inspect and manifest show descend
/// the index, where the runtime runs the entry of its node's platform. The
/// published function-oci-go was the first such module, and inspect read
/// its index as a manifest with no layers.
#[test]
fn push_a_two_platform_image_then_inspect_and_show() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("guestfn.yaml"),
        "contract: oci@0.1\nname: tagger\nversion: 0.3.0\n",
    )
    .expect("write");
    let mut registry = Registry::new();
    let index_digest = registry.plain_index("tagger");
    let addr = registry.serve();
    let image = format!("{addr}/example/tagger@{index_digest}");
    let reference = format!("{addr}/example/tagger:v0.3.0");

    // The plain index: two platforms, neither a module image.
    let (stdout, stderr, ok) = guestfn(dir.path(), &["inspect", &image]);
    assert!(ok, "inspect failed: {stderr}");
    assert!(
        stdout.starts_with(&format!("{image}: index {index_digest} (")),
        "{stdout}"
    );
    assert_eq!(stdout.matches("    manifest: none").count(), 2, "{stdout}");
    let (_, stderr, ok) = guestfn(dir.path(), &["manifest", "show", &image]);
    assert!(!ok);
    assert!(
        stderr.contains("carries no module manifest layer"),
        "{stderr}"
    );

    let (stdout, stderr, ok) = guestfn(dir.path(), &["push", &reference, "--image", &image]);
    assert!(ok, "push failed: {stderr}");
    let pinned = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Pushed "))
        .and_then(|l| l.split(' ').next())
        .expect("pinned reference")
        .to_string();

    let (stdout, stderr, ok) = guestfn(dir.path(), &["inspect", &pinned]);
    assert!(ok, "inspect failed: {stderr}");
    assert!(stdout.contains(": index sha256:"), "{stdout}");
    for platform in ["linux/amd64", "linux/arm64"] {
        assert!(
            stdout.contains(&format!("  {platform}: manifest sha256:")),
            "{stdout}"
        );
    }
    assert_eq!(
        stdout
            .matches(&format!(
                "    module: an OCI image (oci@0.1), which the OCI runtime, function-oci, boots; built from {image}"
            ))
            .count(),
        2,
        "{stdout}"
    );
    assert_eq!(
        stdout
            .matches(&format!(
                "    manifest: tagger 0.3.0, contract oci@0.1, image {image}"
            ))
            .count(),
        2,
        "{stdout}"
    );
    assert!(!stdout.contains("the runtime would refuse it"), "{stdout}");

    let (stdout, _, ok) = guestfn(dir.path(), &["inspect", &pinned, "--output", "json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let platforms = v["reference"]["platforms"].as_array().expect("platforms");
    assert_eq!(
        platforms
            .iter()
            .map(|p| p["platform"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["linux/amd64", "linux/arm64"]
    );
    for p in platforms {
        assert_eq!(p["image"], image);
        assert_eq!(p["manifest"]["contract"], "oci@0.1");
        assert_eq!(p["layers"].as_array().expect("layers").len(), 2);
    }
    assert!(v["reference"].get("config").is_none(), "{stdout}");
    assert!(v["reference"].get("moduleLayerError").is_none(), "{stdout}");

    let (_, stderr, ok) = guestfn(dir.path(), &["inspect", &pinned, "--pull"]);
    assert!(!ok);
    assert!(stderr.contains("has no module file to pull"), "{stderr}");

    let (stdout, stderr, ok) = guestfn(dir.path(), &["manifest", "show", &pinned]);
    assert!(ok, "manifest show failed: {stderr}");
    assert!(stdout.contains("contract: oci@0.1"), "{stdout}");
    assert!(stdout.contains("name: tagger"), "{stdout}");
}

/// An oci@0.1 manifest publishes the image with the manifest appended as
/// one more layer (docs/one-pager-oci-runtime.md): no module file is read,
/// the image's digest goes into the published manifest, the result sits in
/// the image's repository, and inspect reads the manifest out of the layer.
#[test]
fn push_a_module_image_then_inspect_and_show() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("guestfn.yaml"),
        "contract: oci@0.1\nname: tagger\nversion: 0.3.0\nrequires:\n  credentials: [cmdb]\n",
    )
    .expect("write");
    let mut registry = Registry::new();
    let image_digest = registry.plain_image("tagger");
    let addr = registry.serve();
    let image = format!("{addr}/example/tagger@{image_digest}");
    let reference = format!("{addr}/example/tagger:v0.3.0-module");

    // The pin is checked before anything is pushed.
    let (_, stderr, ok) = guestfn(
        dir.path(),
        &[
            "push",
            &reference,
            "--image",
            &format!("{addr}/example/tagger:v1"),
        ],
    );
    assert!(!ok);
    assert!(
        stderr.contains("image must be an OCI reference pinned by digest"),
        "{stderr}"
    );
    let (_, stderr, ok) = guestfn(dir.path(), &["push", &reference]);
    assert!(!ok);
    assert!(
        stderr.contains("names the image it is built from"),
        "{stderr}"
    );
    // The module goes where the image's layers already are.
    let (_, stderr, ok) = guestfn(
        dir.path(),
        &[
            "push",
            &format!("{addr}/example/other:v1"),
            "--image",
            &image,
        ],
    );
    assert!(!ok);
    assert!(
        stderr.contains("is not in the image's repository"),
        "{stderr}"
    );

    let (stdout, stderr, ok) = guestfn(dir.path(), &["push", &reference, "--image", &image]);
    assert!(ok, "push failed: {stderr}");
    assert!(
        stdout.contains(&format!("(the image {image} with its manifest layer)")),
        "{stdout}"
    );
    assert!(stdout.contains("credentials:\n  - cmdb"), "{stdout}");
    let pinned = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Pushed "))
        .and_then(|l| l.split(' ').next())
        .expect("pinned reference")
        .to_string();
    assert!(
        pinned.starts_with(&format!("{addr}/example/tagger:v0.3.0-module@sha256:"))
            && !pinned.ends_with(&image_digest),
        "the module has a digest of its own: {pinned}"
    );

    let (stdout, stderr, ok) = guestfn(dir.path(), &["inspect", &pinned]);
    assert!(ok, "inspect failed: {stderr}");
    assert!(
        stdout.contains("config: application/vnd.oci.image.config.v1+json"),
        "{stdout}"
    );
    assert!(
        stdout.contains("io.crossplane.guestfn=manifest"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "module: an OCI image (oci@0.1), which the OCI runtime, function-oci, boots; built from {image}"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "manifest: tagger 0.3.0, contract oci@0.1, image {image}, credentials cmdb"
        )),
        "{stdout}"
    );
    let (stdout, _, ok) = guestfn(dir.path(), &["inspect", &pinned, "--output", "json"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(v["reference"]["image"], image);
    assert_eq!(v["reference"]["manifest"]["contract"], "oci@0.1");
    assert_eq!(
        v["reference"]["layers"].as_array().expect("layers").len(),
        2
    );
    let (_, stderr, ok) = guestfn(dir.path(), &["inspect", &pinned, "--pull"]);
    assert!(!ok);
    assert!(stderr.contains("has no module file to pull"), "{stderr}");

    let (stdout, stderr, ok) = guestfn(dir.path(), &["manifest", "show", &pinned]);
    assert!(ok, "manifest show failed: {stderr}");
    assert!(stdout.contains("contract: oci@0.1"), "{stdout}");
    // The emitter folds the long reference onto its own line.
    assert!(
        stdout.contains("image:") && stdout.contains(&image),
        "{stdout}"
    );

    // A WebAssembly project refuses --image, and guestfn build refuses an
    // OCI manifest.
    let wasm_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(wasm_dir.path().join("fn.wasm"), tiny_component()).expect("write");
    std::fs::write(wasm_dir.path().join("guestfn.yaml"), MANIFEST_YAML).expect("write");
    let (_, stderr, ok) = guestfn(wasm_dir.path(), &["push", &reference, "--image", &image]);
    assert!(!ok);
    assert!(
        stderr.contains("--image is for an oci@0.1 manifest"),
        "{stderr}"
    );
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
        stderr.contains("component does not implement the guestfn:function@1.0.0 world"),
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
            guestfn_engine_wasm::CORE_MODULE_REFUSAL
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
        stderr.contains(guestfn_engine_wasm::CORE_MODULE_REFUSAL),
        "{stderr}"
    );
}

/// A core module carrying wit-bindgen's component-type section is what a
/// C, Zig or Go guest built outside guestfn is: the refusal says that
/// guestfn build wraps it.
#[test]
fn a_core_module_with_the_section_is_told_to_build() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bare = wat::parse_str(CORE_MODULE_WAT).expect("wat");
    let wit = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit");
    let with_section = guestfn_engine_wasm::componentize::embed_world(
        &bare,
        std::path::Path::new(wit),
        "function",
        guestfn_engine_wasm::componentize::StringEncoding::Utf8,
    )
    .expect("embed the world");
    std::fs::write(dir.path().join("fn.wasm"), with_section).expect("write");
    let (_, stderr, ok) = guestfn(dir.path(), &["inspect", "fn.wasm"]);
    assert!(!ok);
    assert!(
        stderr.contains(guestfn_engine_wasm::CORE_MODULE_REFUSAL),
        "{stderr}"
    );
    assert!(
        stderr.contains("; it carries wit-bindgen's component-type section, which guestfn build wraps into a component"),
        "{stderr}"
    );

    let addr = empty_registry();
    let (_, stderr, ok) = guestfn(dir.path(), &["push", &format!("{addr}/example/greeter:v1")]);
    assert!(!ok);
    assert!(
        stderr.contains("which guestfn build wraps into a component"),
        "{stderr}"
    );
}

#[test]
fn inspect_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("fn.wasm"), tiny_component()).expect("write");
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
    std::fs::write(dir.path().join("guestfn.yaml"), MANIFEST_YAML).expect("write");
    let (stdout, stderr, ok) = guestfn(dir.path(), &["manifest", "validate"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains(
            "guestfn.yaml: valid (greeter v0.1.0, requires egress example.com, credentials cmdb, resources apiextensions.crossplane.io/EnvironmentConfig (name), composes policy/PodDisruptionBudget (add);"
        ),
        "{stdout}"
    );

    std::fs::write(
        dir.path().join("bad.yaml"),
        "contract: wasm@1.0\nverion: nope\n",
    )
    .expect("write");
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
        "guestfn.yaml",
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
        let wasmfn = std::fs::read_to_string(project.join("guestfn.yaml")).expect("guestfn.yaml");
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
    std::fs::write(dir.path().join("fn.wasm"), tiny_component()).expect("write");
    std::fs::write(dir.path().join("guestfn.yaml"), MANIFEST_YAML).expect("write");
    let (stdout, stderr, ok) = guestfn(dir.path(), &["scaffold", "composition"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("- step: greeter"), "{stdout}");
    assert!(stdout.contains("type: Path"), "{stdout}");
    assert!(stdout.contains("path: fn.wasm"), "{stdout}");
    assert!(
        stdout.contains("config:\n      from: spec.config"),
        "{stdout}"
    );
    assert!(stdout.contains("#           greeting:"), "{stdout}");
    assert!(stdout.contains("#             default: hello"), "{stdout}");
    assert!(
        stdout.contains("#   permit (principal, action == Action::\"grantEgress\", resource in HostPattern::\"example.com\");"),
        "{stdout}"
    );
    assert!(
        stdout.contains("#   permit (principal, action == Action::\"spendCredential\", resource == Credential::\"cmdb\");"),
        "{stdout}"
    );
    assert!(
        stdout.contains("#   permit (principal, action == Action::\"composeResource\", resource == Kind::\"policy/PodDisruptionBudget\") when { context.change == \"add\" };"),
        "{stdout}"
    );
    assert!(
        stdout.contains("#   permit (principal, action == Action::\"requireResource\", resource == Kind::\"apiextensions.crossplane.io/EnvironmentConfig\") when { context has name };"),
        "{stdout}"
    );

    let (stdout, _, ok) = guestfn(dir.path(), &["scaffold", "composition", "--full"]);
    assert!(ok);
    assert!(stdout.contains("kind: Composition"), "{stdout}");
    assert!(stdout.contains("mode: Pipeline"), "{stdout}");
}

fn contract(version: &str) -> String {
    format!(
        "package guestfn:function@{version};\n\nworld function-sync {{\n  export run: func(request: list<u8>) -> result<list<u8>, string>;\n}}\n"
    )
}

/// Publishes a package under a tag; `title` keeps two pushes of one tag apart.
fn publish(registry: &mut Registry, tag: &str, package: &str, title: &str) -> String {
    let wasm = encoded_wit_package(package);
    let digest = registry.module(&wasm, None, title);
    registry.tag(tag, &digest);
    digest
}

fn printed(package: &str) -> String {
    decode_wit_package(&encoded_wit_package(package))
        .expect("decode the package")
        .text
}

fn guest_on(version: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("wit/deps")).expect("wit/deps");
    std::fs::write(
        dir.path().join("wit/deps/guestfn-function.wit"),
        printed(&contract(version)),
    )
    .expect("vendored package");
    std::fs::write(
        dir.path().join("wit/world.wit"),
        format!(
            "package local:guest;\n\nworld function {{\n    include guestfn:function/function-sync@{version};\n}}\n"
        ),
    )
    .expect("guest world");
    dir
}

fn pin(dir: &Path, repository: &str, version: &str, digest: &str) {
    std::fs::write(
        dir.join("guestfn.lock"),
        format!(
            "packages:\n  - name: guestfn:function\n    version: {version}\n    repository: {repository}\n    digest: {digest}\n"
        ),
    )
    .expect("write the lock");
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).expect("read the project's file")
}

fn wit_update(dir: &Path, registry: &str, extra: &[&str]) -> (String, String, bool) {
    let repository = format!("{registry}/guestfn/function");
    let mut args = vec!["wit", "update", "--repository", repository.as_str()];
    args.extend_from_slice(extra);
    guestfn(dir, &args)
}

#[test]
fn wit_update_bumps_to_the_newest_of_the_major_and_pins_it() {
    let mut registry = Registry::new();
    let mut newest_digest = String::new();
    for version in ["1.0.0", "1.1.0", "1.2.0-rc.1", "2.0.0"] {
        let digest = publish(&mut registry, version, &contract(version), version);
        if version == "1.1.0" {
            newest_digest = digest;
        }
    }
    let registry = registry.serve();
    let guest = guest_on("1.0.0");

    let (stdout, stderr, ok) = wit_update(guest.path(), &registry, &[]);
    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("guestfn:function 1.0.0 -> 1.1.0"),
        "{stdout}"
    );
    assert!(
        stdout.contains("guestfn:function 2.0.0 is published"),
        "{stdout}"
    );
    assert_eq!(
        read(guest.path(), "wit/deps/guestfn-function.wit"),
        printed(&contract("1.1.0"))
    );
    assert!(
        read(guest.path(), "wit/world.wit").contains("function-sync@1.1.0;"),
        "the include did not move"
    );
    let lock = read(guest.path(), "guestfn.lock");
    assert!(lock.contains("version: 1.1.0"), "{lock}");
    assert!(lock.contains(&format!("digest: {newest_digest}")), "{lock}");

    let (stdout, stderr, ok) = wit_update(guest.path(), &registry, &[]);
    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("guestfn:function 1.1.0 is up to date"),
        "{stdout}"
    );
}

#[test]
fn wit_update_takes_a_new_major_only_when_named() {
    let mut registry = Registry::new();
    publish(&mut registry, "1.0.0", &contract("1.0.0"), "a");
    publish(&mut registry, "2.0.0", &contract("2.0.0"), "b");
    let registry = registry.serve();
    let guest = guest_on("1.0.0");

    let (stdout, stderr, ok) = wit_update(guest.path(), &registry, &["--version", "2.0.0"]);
    assert!(ok, "stderr: {stderr}");
    assert!(
        stdout.contains("guestfn:function 1.0.0 -> 2.0.0"),
        "{stdout}"
    );
    assert!(read(guest.path(), "wit/world.wit").contains("function-sync@2.0.0;"));
}

#[test]
fn wit_update_refuses_another_package_and_leaves_the_guest_alone() {
    let mut registry = Registry::new();
    publish(
        &mut registry,
        "1.1.0",
        "package other:thing@1.1.0;\n\nworld w {\n}\n",
        "a",
    );
    let registry = registry.serve();
    let guest = guest_on("1.0.0");
    let before = read(guest.path(), "wit/deps/guestfn-function.wit");

    let (_, stderr, ok) = wit_update(guest.path(), &registry, &[]);
    assert!(!ok, "a foreign package was installed");
    assert!(
        stderr.contains("holds other:thing, not guestfn:function"),
        "{stderr}"
    );
    assert_eq!(read(guest.path(), "wit/deps/guestfn-function.wit"), before);
    assert!(!guest.path().join("guestfn.lock").exists());
}

#[test]
fn wit_update_names_what_it_needs_in_a_directory_that_is_no_guest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_, stderr, ok) = wit_update(dir.path(), &empty_registry(), &[]);
    assert!(!ok);
    assert!(stderr.contains("wit/world.wit"), "{stderr}");
    assert!(stderr.contains("guestfn init writes it"), "{stderr}");
}

#[test]
fn wit_fetch_restores_a_copy_that_drifted_from_the_lock() {
    let mut registry = Registry::new();
    let digest = publish(&mut registry, "1.0.0", &contract("1.0.0"), "a");
    let registry = registry.serve();
    let guest = guest_on("1.0.0");
    pin(
        guest.path(),
        &format!("{registry}/guestfn/function"),
        "1.0.0",
        &digest,
    );

    let (stdout, stderr, ok) = guestfn(guest.path(), &["wit", "fetch"]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("matches guestfn.lock"), "{stdout}");

    std::fs::write(guest.path().join("wit/deps/guestfn-function.wit"), "edited").expect("drift");
    let (stdout, stderr, ok) = guestfn(guest.path(), &["wit", "fetch"]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("wrote 1 of 1 packages"), "{stdout}");
    assert_eq!(
        read(guest.path(), "wit/deps/guestfn-function.wit"),
        printed(&contract("1.0.0"))
    );
}

#[test]
fn wit_fetch_pulls_the_pinned_digest_after_the_tag_moved() {
    let mut registry = Registry::new();
    let digest = publish(&mut registry, "1.0.0", &contract("1.0.0"), "first");
    let moved = format!("/// Pushed again.\n{}", contract("1.0.0"));
    publish(&mut registry, "1.0.0", &moved, "second");
    let registry = registry.serve();
    let guest = guest_on("1.0.0");
    std::fs::remove_file(guest.path().join("wit/deps/guestfn-function.wit")).expect("remove");
    pin(
        guest.path(),
        &format!("{registry}/guestfn/function"),
        "1.0.0",
        &digest,
    );

    let (_, stderr, ok) = guestfn(guest.path(), &["wit", "fetch"]);
    assert!(ok, "stderr: {stderr}");
    assert_eq!(
        read(guest.path(), "wit/deps/guestfn-function.wit"),
        printed(&contract("1.0.0"))
    );
}

#[test]
fn wit_fetch_refuses_an_include_the_lock_does_not_pin() {
    let guest = guest_on("1.0.0");
    pin(
        guest.path(),
        "ghcr.io/x/guestfn/function",
        "1.1.0",
        "sha256:unused",
    );

    let (_, stderr, ok) = guestfn(guest.path(), &["wit", "fetch"]);
    assert!(!ok);
    assert!(
        stderr.contains("includes guestfn:function 1.0.0, but guestfn.lock pins 1.1.0"),
        "{stderr}"
    );
}

#[test]
fn wit_fetch_says_what_writes_a_missing_lock() {
    let guest = guest_on("1.0.0");
    let (_, stderr, ok) = guestfn(guest.path(), &["wit", "fetch"]);
    assert!(!ok);
    assert!(stderr.contains("guestfn wit update writes it"), "{stderr}");
}

#[test]
fn build_pulls_what_the_lock_pins_before_the_toolchain_runs() {
    let registry = empty_registry();
    let guest = guest_on("1.0.0");
    let missing = format!("sha256:{}", "b".repeat(64));
    pin(
        guest.path(),
        &format!("{registry}/guestfn/function"),
        "1.0.0",
        &missing,
    );

    let (_, stderr, ok) = guestfn(guest.path(), &["build", "--lang", "go"]);
    assert!(!ok);
    assert!(
        stderr.contains(&format!("cannot fetch manifest {missing}")),
        "{stderr}"
    );
}

const WASI_IO: &str =
    "package wasi:io@0.2.12;\n\ninterface streams {\n  resource input-stream;\n}\n";
const WASI_HTTP: &str = "package wasi:http@0.2.12;\n\ninterface outgoing-handler {\n  use wasi:io/streams@0.2.12.{input-stream};\n}\n";

fn publish_wasi(registry: &mut Registry) {
    let io = encoded_wit_package(WASI_IO);
    let digest = registry.module(&io, None, "io");
    registry.tag_in("webassembly/wasi/io", "0.2.12", &digest);
    let http = encoded_wit_package_over(WASI_HTTP, &[WASI_IO]);
    let digest = registry.module(&http, None, "http");
    registry.tag_in("webassembly/wasi/http", "0.2.12", &digest);
}

fn guest_importing_http(version: &str) -> tempfile::TempDir {
    let guest = guest_on(version);
    std::fs::write(
        guest.path().join("wit/world.wit"),
        format!(
            "package local:guest;\n\nworld function {{\n    include guestfn:function/function-sync@{version};\n\n    import wasi:http/outgoing-handler@0.2.12;\n}}\n"
        ),
    )
    .expect("guest world");
    guest
}

fn wit_update_with_wasi(dir: &Path, registry: &str) -> (String, String, bool) {
    let repository = format!("{registry}/guestfn/function");
    let wasi = format!("{registry}/webassembly/wasi");
    guestfn(
        dir,
        &[
            "wit",
            "update",
            "--repository",
            repository.as_str(),
            "--wasi-repository",
            wasi.as_str(),
        ],
    )
}

#[test]
fn wit_update_pins_what_the_world_imports_and_what_that_refers_to() {
    let mut registry = Registry::new();
    publish(&mut registry, "1.0.0", &contract("1.0.0"), "contract");
    publish_wasi(&mut registry);
    let registry = registry.serve();
    let guest = guest_importing_http("1.0.0");

    let (stdout, stderr, ok) = wit_update_with_wasi(guest.path(), &registry);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("wrote 3 packages"), "{stdout}");
    assert!(read(guest.path(), "wit/deps/http.wit").contains("package wasi:http@0.2.12;"));
    assert!(read(guest.path(), "wit/deps/io.wit").contains("package wasi:io@0.2.12;"));
    let lock = read(guest.path(), "guestfn.lock");
    for name in ["guestfn:function", "wasi:http", "wasi:io"] {
        assert!(lock.contains(&format!("name: {name}\n")), "{lock}");
    }

    std::fs::remove_file(guest.path().join("wit/deps/io.wit")).expect("remove");
    let (stdout, stderr, ok) = guestfn(guest.path(), &["wit", "fetch"]);
    assert!(ok, "stderr: {stderr}");
    assert!(stdout.contains("wrote 1 of 3 packages"), "{stdout}");
    assert!(guest.path().join("wit/deps/io.wit").is_file());
}

#[test]
fn wit_fetch_refuses_an_import_the_lock_does_not_pin() {
    let guest = guest_importing_http("1.0.0");
    pin(
        guest.path(),
        "ghcr.io/x/guestfn/function",
        "1.0.0",
        "sha256:unused",
    );

    let (_, stderr, ok) = guestfn(guest.path(), &["wit", "fetch"]);
    assert!(!ok);
    assert!(
        stderr.contains("imports wasi:http 0.2.12, which guestfn.lock does not pin"),
        "{stderr}"
    );
}
