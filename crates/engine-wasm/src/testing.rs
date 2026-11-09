//! The WAT component fixtures every suite in the workspace drives the
//! engine with: hand-written components implementing the wasmfn:function
//! world with a sync-lifted `run` (the canonical ABI accepts a sync
//! implementation of the world's async run), each misbehaving in one way or
//! returning fixed bytes, plus the smallest component the runtime's and
//! guestfn's suites load. Behind the `testutil` feature, so a test never
//! declares a component of its own; a host import that moves values through
//! memory is lowered against a `$libc` core module instantiated first, so the
//! main module can import both the memory and the lowered function.

use std::fmt::Write as _;

/// Assembles WAT into a wasm binary.
pub fn wasm(wat: &str) -> Vec<u8> {
    wat::parse_str(wat).expect("the fixture's WAT assembles")
}

/// Escapes bytes for a WAT data-segment string.
pub fn wat_bytes(b: &[u8]) -> String {
    let mut out = String::new();
    for byte in b {
        let _ = write!(out, "\\{byte:02x}");
    }
    out
}

/// The core body shared by the component fixtures: a bump realloc (8-byte
/// aligned, so any canonical-ABI allocation is accepted) and a memory.
/// `run` is provided per fixture; a sync-lifted `run` returns the address
/// of its result (the ok/err discriminant, then the list's pointer and
/// length).
pub const COMPONENT_CORE_PRELUDE: &str = r#"
    (memory (export "memory") 4)
    (global $next (mut i32) (i32.const 131072))
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
      (local $p i32)
      global.get $next
      local.set $p
      global.get $next
      (i32.and (i32.add (local.get 3) (i32.const 7)) (i32.const -8))
      i32.add
      global.set $next
      local.get $p)
"#;

/// The world's `run` export lifted from a core instance's `run`.
pub fn lift_run(instance: &str, libc: &str) -> String {
    format!(
        r#"(func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func ${instance} "run") (memory (core memory ${libc} "memory")) (realloc (core func ${libc} "cabi_realloc"))))"#
    )
}

/// A component whose run returns the given bytes.
pub fn fixed_component(rsp: &[u8]) -> String {
    format!(
        r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func (export "run") (param i32 i32) (result i32)
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72) (i32.const {len}))
      (i32.const 64))
    (data (i32.const 1024) "{data}"))
  (core instance $i (instantiate $m))
  {lift}
)"#,
        len = rsp.len(),
        data = wat_bytes(rsp),
        lift = lift_run("i", "i"),
    )
}

/// A component whose run is the given core body (returning the result
/// address), with no imports.
pub fn component_with_run(body: &str) -> String {
    format!(
        r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func (export "run") (param i32 i32) (result i32)
      {body}))
  (core instance $i (instantiate $m))
  {lift}
)"#,
        lift = lift_run("i", "i"),
    )
}

/// A component whose main core module imports one WASI 0.2 function,
/// lowered against a `$libc` instance whose memory the main module shares:
/// `import_decl` is the component-level import, `lower` the canon lower
/// (naming `$libc_inst` and `$f` for the imported function), `core_import`
/// the core module's own import declaration of it as `$host`, and `body`
/// run's core body.
pub fn component_importing(
    import_decl: &str,
    lower: &str,
    core_import: &str,
    body: &str,
) -> String {
    format!(
        r#"(component
  {import_decl}
  (core module $libc
    {COMPONENT_CORE_PRELUDE})
  (core instance $libc_inst (instantiate $libc))
  (core func $lowered {lower})
  (core module $m
    (import "env" "memory" (memory 4))
    {core_import}
    (func (export "run") (param i32 i32) (result i32)
      {body}))
  (core instance $m_inst (instantiate $m
    (with "env" (instance (export "memory" (memory $libc_inst "memory"))))
    (with "host" (instance (export "f" (func $lowered))))))
  {lift}
)"#,
        lift = lift_run("m_inst", "libc_inst"),
    )
}

/// The result address for ok(list at ptr, len) stored at 64.
pub fn ok_result(ptr: &str, len: &str) -> String {
    format!(
        r#"(i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) {ptr})
      (i32.store (i32.const 72) {len})
      (i32.const 64)"#
    )
}

/// A guest that grows toward the exported response: empty means the grow
/// was denied (memory.grow returned -1), one byte means it succeeded.
pub fn grow_guest(pages: u32) -> String {
    component_with_run(&format!(
        r#"(i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72)
        (i32.ne (memory.grow (i32.const {pages})) (i32.const -1)))
      (i32.const 64)"#
    ))
}

/// A guest that grows a funcref table by the given number of elements and
/// reports the outcome as grow_guest does: empty means the grow was denied
/// (table.grow returned -1), one byte means it succeeded.
pub fn grow_table_guest(elements: u32) -> String {
    format!(
        r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (table 0 funcref)
    (func (export "run") (param i32 i32) (result i32)
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72)
        (i32.ne (table.grow (ref.null func) (i32.const {elements})) (i32.const -1)))
      (i32.const 64)))
  (core instance $i (instantiate $m))
  {lift}
)"#,
        lift = lift_run("i", "i"),
    )
}

/// A guest that spins for the given nanoseconds on the monotonic clock
/// (`wasi:clocks/monotonic-clock`), holding its memory meanwhile.
pub fn spinning_guest(nanos: u64) -> String {
    component_importing(
        r#"(import "wasi:clocks/monotonic-clock@0.2.0" (instance $clock
    (export "now" (func (result u64)))))"#,
        "(canon lower (func $clock \"now\"))",
        r#"(import "host" "f" (func $host (result i64)))"#,
        &format!(
            r#"(local $start i64)
      (local.set $start (call $host))
      (loop $l
        (br_if $l (i64.lt_u (i64.sub (call $host) (local.get $start)) (i64.const {nanos}))))
      {}"#,
            ok_result("(i32.const 1024)", "(i32.const 0)")
        ),
    )
}

/// The smallest ABI v2 component, byte for byte what the conformance
/// goldens were recorded with: a sync-lifted run returning an empty
/// response over a one-page memory and a fixed realloc.
pub const TINY_COMPONENT_WAT: &str = r#"(component
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

pub fn tiny_component() -> Vec<u8> {
    wasm(TINY_COMPONENT_WAT)
}

/// A component whose run returns the request list it was given: the
/// transparency proof's guest.
pub const ECHO_COMPONENT_WAT: &str = r#"(component
  (core module $m
    (memory (export "memory") 2)
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 4096)
    (func (export "run") (param i32 i32) (result i32)
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (local.get 0))
      (i32.store (i32.const 72) (local.get 1))
      (i32.const 64)))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory (core memory $i "memory")) (realloc (core func $i "cabi_realloc"))))
)"#;

pub fn echo_component() -> Vec<u8> {
    wasm(ECHO_COMPONENT_WAT)
}

/// A component that logs msg once at the world's `debug` level through the
/// typed `log` import and returns an empty response.
pub fn logging_component(msg: &str) -> Vec<u8> {
    wasm(&format!(
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
    (data (i32.const 2048) "{msg}"))
  (core instance $m_inst (instantiate $m
    (with "env" (instance (export "memory" (memory $libc_inst "memory"))))
    (with "host" (instance (export "log" (func $log_lowered))))))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $m_inst "run") (memory (core memory $libc_inst "memory")) (realloc (core func $libc_inst "cabi_realloc"))))
)"#,
        len = msg.len(),
    ))
}
