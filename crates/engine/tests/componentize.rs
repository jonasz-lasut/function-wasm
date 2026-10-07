//! componentize over core modules built the way wit-bindgen's C generator
//! builds them: a core module exporting the world's `run` at the core level
//! plus `cabi_realloc`, with the runtime's own world embedded as the
//! `component-type` custom section. The wrapped result must be what the
//! runtime takes for an ABI v2 guest - inspect's verdict and a run both say.

use std::fmt::Write as _;
use std::path::Path;

use function_wasm_engine::componentize::{
    StringEncoding, carries_component_type, componentize, embed_world,
};
use function_wasm_engine::{Config, Engine, RunOptions};

/// The runtime's own wit/ directory: the world the engine compiles.
const RUNTIME_WIT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit");

/// Escapes bytes for a WAT data-segment string.
fn wat_bytes(b: &[u8]) -> String {
    let mut out = String::new();
    for byte in b {
        let _ = write!(out, "\\{byte:02x}");
    }
    out
}

/// A core module in the shape wit-bindgen's C generator leaves behind for
/// the world: `run` lifted sync (the canonical ABI's result pointer: the ok
/// discriminant, then the list's pointer and length) and a bump
/// `cabi_realloc`. `imports` is spliced in verbatim.
fn core_module(rsp: &[u8], imports: &str) -> String {
    format!(
        r#"(module
  {imports}
  (memory (export "memory") 4)
  (global $next (mut i32) (i32.const 131072))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (local $p i32)
    global.get $next
    local.set $p
    global.get $next
    local.get 3
    i32.add
    global.set $next
    local.get $p)
  (func (export "run") (param i32 i32) (result i32)
    (i32.store8 (i32.const 64) (i32.const 0))
    (i32.store (i32.const 68) (i32.const 1024))
    (i32.store (i32.const 72) (i32.const {len}))
    (i32.const 64))
  (data (i32.const 1024) "{data}"))"#,
        len = rsp.len(),
        data = wat_bytes(rsp),
    )
}

const FD_WRITE: &str =
    r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#;

/// The runtime's world (wit/wasmfn-function.wit) embedded into the module
/// as wit-bindgen embeds it - through the engine's own embed, the one
/// guestfn build runs over a Go guest's wit/. The world is taken as is, its
/// `run` declared async: wit-component lifts a plain `run` export with the
/// sync ABI, since async-ness is a canonical option, not part of the
/// component type.
fn with_world(wasm: Vec<u8>) -> Vec<u8> {
    embed_world(
        &wasm,
        Path::new(RUNTIME_WIT),
        "function",
        StringEncoding::Utf8,
    )
    .expect("embed the world")
}

fn engine() -> Engine {
    Engine::new(Config::default()).expect("engine")
}

/// The wrapped component through the runtime: inspect's verdict, then a run.
fn assert_serves(e: &Engine, wasm: &[u8], rsp: &[u8]) -> function_wasm_engine::Inspection {
    let shape = e.inspect(wasm).expect("inspect");
    assert_eq!(shape.abi_error, None, "the world typechecks");
    assert!(
        shape
            .exports
            .iter()
            .any(|x| x.name == "run" && x.kind == "func"),
        "{:?}",
        shape.exports
    );
    let m = e.compile(wasm).expect("compile");
    let out = e.run(&m, b"", RunOptions::default()).expect("run");
    assert_eq!(out, rsp);
    shape
}

#[test]
fn a_core_module_without_preview1_imports_is_wrapped_without_the_adapter() {
    let rsp = b"RunFunctionResponse bytes";
    let core = with_world(wat::parse_str(core_module(rsp, "")).expect("wat"));
    assert!(carries_component_type(&core));

    let c = componentize(&core).expect("componentize");
    assert!(!c.adapter, "nothing imported from wasi_snapshot_preview1");
    assert!(
        !carries_component_type(&c.wasm),
        "a component is left alone"
    );

    let e = engine();
    let shape = assert_serves(&e, &c.wasm, rsp);
    assert!(
        shape.imports.iter().all(|i| !i.name.starts_with("wasi:")),
        "no adapter, no WASI imports: {:?}",
        shape.imports
    );
}

#[test]
fn a_core_module_with_preview1_imports_gets_the_adapter() {
    let rsp = b"RunFunctionResponse bytes";
    let core = with_world(wat::parse_str(core_module(rsp, FD_WRITE)).expect("wat"));
    assert!(carries_component_type(&core));

    let c = componentize(&core).expect("componentize");
    assert!(c.adapter, "fd_write comes from wasi_snapshot_preview1");

    let e = engine();
    let shape = assert_serves(&e, &c.wasm, rsp);
    // The adapter turned fd_write into the WASI 0.2 interfaces the runtime
    // links (wasmtime-wasi's p2 linker serves them under the sandbox).
    assert!(
        shape
            .imports
            .iter()
            .any(|i| i.name.starts_with("wasi:cli/") || i.name.starts_with("wasi:io/")),
        "{:?}",
        shape.imports
    );
}

#[test]
fn only_a_core_module_with_the_section_is_componentized() {
    let rsp = b"RunFunctionResponse bytes";
    let bare = wat::parse_str(core_module(rsp, "")).expect("wat");
    assert!(!carries_component_type(&bare));
    let err = componentize(&bare).expect_err("no section");
    assert!(
        err.to_string()
            .starts_with("cannot componentize module: it carries no component-type custom section"),
        "{err}"
    );

    let component = componentize(&with_world(bare)).expect("componentize").wasm;
    assert!(!carries_component_type(&component));
    let err = componentize(&component).expect_err("already a component");
    assert_eq!(
        err.to_string(),
        "cannot componentize module: it is already a component"
    );

    assert!(!carries_component_type(b"not wasm at all"));
}

/// The embed takes a bare core module once: a module that already carries
/// a section (another generator's, or an earlier embed) and a component are
/// refused, as are a wit/ directory that is not there and a world the
/// package does not define - each with the reason, never a module carrying
/// two worlds.
#[test]
fn embed_world_takes_a_bare_core_module_once() {
    let rsp = b"RunFunctionResponse bytes";
    let bare = wat::parse_str(core_module(rsp, "")).expect("wat");
    let embedded = with_world(bare.clone());
    assert!(carries_component_type(&embedded));

    let err = embed_world(
        &embedded,
        Path::new(RUNTIME_WIT),
        "function",
        StringEncoding::Utf8,
    )
    .expect_err("twice");
    assert_eq!(
        err.to_string(),
        "cannot embed the world into module: it already carries a component-type custom section"
    );

    let component = componentize(&embedded).expect("componentize").wasm;
    let err = embed_world(
        &component,
        Path::new(RUNTIME_WIT),
        "function",
        StringEncoding::Utf8,
    )
    .expect_err("component");
    assert_eq!(
        err.to_string(),
        "cannot embed the world into module: it is already a component"
    );

    let err = embed_world(
        &bare,
        Path::new(RUNTIME_WIT),
        "no-such-world",
        StringEncoding::Utf8,
    )
    .expect_err("world");
    let msg = err.to_string();
    assert!(
        msg.starts_with("cannot embed the world into module: ") && msg.contains("no-such-world"),
        "{msg}"
    );

    let missing = Path::new(RUNTIME_WIT).join("no-such-dir");
    let err = embed_world(&bare, &missing, "function", StringEncoding::Utf8).expect_err("dir");
    let msg = err.to_string();
    assert!(
        msg.starts_with(&format!(
            "cannot embed the world into module: cannot read {}: ",
            missing.display()
        )),
        "{msg}"
    );
}

/// A core module whose `run` returns `err(string)` with the string laid
/// out in UTF-16 (two code units per character here, the length counted
/// in code units), the way MoonBit stores its strings.
fn erring_module(utf16: &[u8], units: usize) -> String {
    format!(
        r#"(module
  (memory (export "memory") 4)
  (global $next (mut i32) (i32.const 131072))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (local $p i32)
    global.get $next
    local.set $p
    global.get $next
    local.get 3
    i32.add
    global.set $next
    local.get $p)
  (func (export "run") (param i32 i32) (result i32)
    (i32.store8 (i32.const 64) (i32.const 1))
    (i32.store (i32.const 68) (i32.const 1024))
    (i32.store (i32.const 72) (i32.const {units}))
    (i32.const 64))
  (data (i32.const 1024) "{data}"))"#,
        data = wat_bytes(utf16),
    )
}

/// The embed states the guest's string encoding and the canonical ABI
/// transcodes by it: the same bytes a MoonBit guest lowers reach the host
/// as the string they are under a UTF-16 embed, and as something else
/// under the UTF-8 one the Go path uses (here, an invalid sequence the
/// canonical ABI traps on).
#[test]
fn the_embed_states_the_string_encoding() {
    let message = "héllo";
    let utf16: Vec<u8> = message
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    let bare = wat::parse_str(erring_module(&utf16, message.encode_utf16().count())).expect("wat");
    let e = engine();

    let utf16_embed = embed_world(
        &bare,
        Path::new(RUNTIME_WIT),
        "function",
        StringEncoding::Utf16,
    )
    .expect("embed utf16");
    let c = componentize(&utf16_embed).expect("componentize").wasm;
    let m = e.compile(&c).expect("compile");
    let err = e
        .run(&m, b"", RunOptions::default())
        .expect_err("run returns an error");
    assert_eq!(err.to_string(), format!("run returned an error: {message}"));

    let utf8_embed = embed_world(
        &bare,
        Path::new(RUNTIME_WIT),
        "function",
        StringEncoding::Utf8,
    )
    .expect("embed utf8");
    let c = componentize(&utf8_embed).expect("componentize").wasm;
    let m = e.compile(&c).expect("compile");
    let err = e
        .run(&m, b"", RunOptions::default())
        .expect_err("run fails");
    assert_ne!(err.to_string(), format!("run returned an error: {message}"));
}

/// A section that is there but says nothing wit-component can read is a
/// failed wrap carrying wit-component's words - never a silent pass-through
/// to the runtime's core-module refusal, which would name the wrong fix.
#[test]
fn a_malformed_section_fails_the_wrap() {
    let wasm = wat::parse_str(r#"(module (@custom "component-type:x" "nope"))"#).expect("wat");
    assert!(carries_component_type(&wasm));
    let err = componentize(&wasm).expect_err("malformed metadata");
    let msg = err.to_string();
    assert!(msg.starts_with("cannot componentize module: "), "{msg}");
    assert!(msg.contains("component-type:x"), "{msg}");
}
