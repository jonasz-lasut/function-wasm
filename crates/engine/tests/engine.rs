//! Engine tests over WAT fixtures: components implementing the
//! wasmfn:function world, in the spirit of the old Go tree's
//! internal/testwasm - a component that returns fixed response bytes,
//! components that misbehave in one way each, the world typecheck refusals,
//! and the sandbox reached through WASI 0.2's own interfaces.
//!
//! The fixtures are hand-written component text, sync-lifted: the canonical
//! ABI accepts a sync implementation of the world's `async` run, which is
//! also what keeps a stable-toolchain guest possible. A host import that
//! moves values through memory (a list or a string) is lowered against a
//! `$libc` core module instantiated first, so the main module can import
//! both the memory and the lowered function.

use std::fmt::Write as _;
use std::time::Duration;

use function_sdk_rust::proto::v1::{Result as FnResult, RunFunctionResponse, Severity, Target};
use function_wasm_engine::{CORE_MODULE_REFUSAL, Config, Engine, RunOptions};
use prost::Message;

/// Escapes bytes for a WAT data-segment string.
fn wat_bytes(b: &[u8]) -> String {
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
const COMPONENT_CORE_PRELUDE: &str = r#"
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
fn lift_run(instance: &str, libc: &str) -> String {
    format!(
        r#"(func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func ${instance} "run") (memory (core memory ${libc} "memory")) (realloc (core func ${libc} "cabi_realloc"))))"#
    )
}

/// A component whose run returns the given bytes.
fn fixed_component(rsp: &[u8]) -> String {
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
fn component_with_run(body: &str) -> String {
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
fn component_importing(import_decl: &str, lower: &str, core_import: &str, body: &str) -> String {
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
fn ok_result(ptr: &str, len: &str) -> String {
    format!(
        r#"(i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) {ptr})
      (i32.store (i32.const 72) {len})
      (i32.const 64)"#
    )
}

fn engine() -> Engine {
    Engine::new(Config::default()).expect("engine")
}

fn response_bytes() -> Vec<u8> {
    RunFunctionResponse {
        results: vec![FnResult {
            severity: Severity::Normal as i32,
            message: "ok".to_string(),
            reason: None,
            target: Some(Target::Composite as i32),
        }],
        ..Default::default()
    }
    .encode_to_vec()
}

fn compile(e: &Engine, wat: &str) -> function_wasm_engine::Module {
    e.compile(&wat::parse_str(wat).expect("wat"))
        .expect("compile")
}

#[test]
fn fixed_response_round_trip() {
    let e = engine();
    let rsp = response_bytes();
    let m = compile(&e, &fixed_component(&rsp));
    let out = e.run(&m, b"anything", RunOptions::default()).expect("run");
    assert_eq!(out, rsp);
}

/// A core module - whatever it exports, ABI v1's shape or none - is refused
/// at every load path with the one sentence that names the fix, never with
/// a world typecheck message that would send its author to the wrong one.
#[test]
fn a_core_module_is_refused_by_name() {
    let e = engine();
    let cases = [
        ("empty", "(module)"),
        (
            "abi v1 shape",
            r#"(module (memory (export "memory") 1)
              (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
              (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
        ),
    ];
    for (name, wat) in cases {
        let wasm = wat::parse_str(wat).expect("wat");
        let err = e.compile(&wasm).expect_err(name);
        assert_eq!(err.to_string(), CORE_MODULE_REFUSAL, "{name}");
        let err = e.inspect(&wasm).expect_err(name);
        assert_eq!(err.to_string(), CORE_MODULE_REFUSAL, "{name}: inspect");
    }

    // An artifact a pre-1.0 runtime compiled from a core module, found in
    // the cache: refused the same way, so the cache treats it as a miss and
    // the module itself is refused at compile.
    let wasmtime = wasmtime::Engine::default();
    let artifact = wasmtime::Module::new(&wasmtime, "(module)")
        .expect("core module")
        .serialize()
        .expect("serialize");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("core.bin");
    std::fs::write(&path, &artifact).expect("write");
    let err = e.deserialize_file(&path).expect_err("core artifact");
    assert_eq!(err.to_string(), CORE_MODULE_REFUSAL);
}

#[test]
fn a_run_observes_the_hostcall_split() {
    let e = engine();
    let m = compile(&e, &fixed_component(&response_bytes()));
    let name = "function_wasm_module_hostcall_duration_seconds";
    let before = function_wasm_engine::metrics::sample(name, &[]).unwrap_or(0.0);
    e.run(&m, b"", RunOptions::default()).expect("run");
    // Other tests' runs share the process-global registry, so the count is
    // only monotonic: this run added at least its own observation.
    let after = function_wasm_engine::metrics::sample(name, &[]).expect("series registered");
    assert!(after >= before + 1.0, "the run observes the split");
}

#[test]
fn the_stack_limit_bounds_recursion() {
    // 4000 frames fit in the default 512 KiB stack and overflow an 8 KiB
    // one; the trap carries the Go runtime's wording.
    let wat = format!(
        r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func $rec (param i32) (result i32)
      local.get 0
      i32.eqz
      (if (result i32)
        (then i32.const 0)
        (else local.get 0 i32.const 1 i32.sub call $rec)))
    (func (export "run") (param i32 i32) (result i32)
      (drop (call $rec (i32.const 4000)))
      {ok}))
  (core instance $i (instantiate $m))
  {lift}
)"#,
        ok = ok_result("(i32.const 1024)", "(i32.const 0)"),
        lift = lift_run("i", "i"),
    );

    let e = engine();
    let m = compile(&e, &wat);
    e.run(&m, b"", RunOptions::default())
        .expect("fits in the default stack");

    let small = Engine::new(Config {
        stack_limit: 8 << 10,
        ..Config::default()
    })
    .expect("engine");
    let m = compile(&small, &wat);
    let err = small
        .run(&m, b"", RunOptions::default())
        .expect_err("should overflow");
    assert_eq!(err.to_string(), "run failed: trap: call stack exhausted");
}

#[test]
fn a_compiled_artifact_is_refused_by_name() {
    let e = engine();
    let m = compile(&e, &fixed_component(&response_bytes()));
    let artifact = e.serialize(&m).expect("serialize");
    let err = e
        .compile(&artifact)
        .expect_err("should refuse the artifact");
    assert_eq!(
        err.to_string(),
        "module is a wasmtime compiled artifact (.cwasm), not a wasm module"
    );
}

/// The epoch deadline interrupts a spinning guest with the message the
/// timeout metric outcome matches on.
#[test]
fn run_deadline() {
    let e = engine();
    let m = compile(
        &e,
        &component_with_run("(loop $l br $l)\n      i32.const 64"),
    );
    let opts = RunOptions {
        timeout: Some(Duration::from_millis(50)),
        ..Default::default()
    };
    let err = e.run(&m, b"", opts).expect_err("should time out");
    assert_eq!(
        err.to_string(),
        "run failed: module exceeded its execution deadline (50ms)"
    );
}

/// A guest that exits through WASI (`wasi:cli/exit`, what a language's
/// `exit()` reaches) reports its status: `exit(err)` is status 1.
#[test]
fn run_exit_status() {
    let e = engine();
    let wat = component_importing(
        r#"(import "wasi:cli/exit@0.2.0" (instance $exit
    (export "exit" (func (param "status" (result))))))"#,
        "(canon lower (func $exit \"exit\"))",
        r#"(import "host" "f" (func $host (param i32)))"#,
        &format!(
            "(call $host (i32.const 1))\n      {}",
            ok_result("(i32.const 1024)", "(i32.const 0)")
        ),
    );
    let m = compile(&e, &wat);
    let err = e
        .run(&m, b"", RunOptions::default())
        .expect_err("should exit");
    assert_eq!(err.to_string(), "run failed: module exited with status 1");
}

/// The guest's Err(string) becomes a run error naming the export - the
/// world's third channel beside a response and a trap.
#[test]
fn guest_error_string() {
    let e = engine();
    let wat = format!(
        r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func (export "run") (param i32 i32) (result i32)
      (i32.store8 (i32.const 64) (i32.const 1))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72) (i32.const 4))
      (i32.const 64))
    (data (i32.const 1024) "boom"))
  (core instance $i (instantiate $m))
  {lift}
)"#,
        lift = lift_run("i", "i"),
    );
    let m = compile(&e, &wat);
    let err = e
        .run(&m, b"", RunOptions::default())
        .expect_err("guest error");
    assert_eq!(err.to_string(), "run returned an error: boom");
}

/// A guest that grows toward the exported response: empty means the grow
/// was denied (memory.grow returned -1), one byte means it succeeded.
fn grow_guest(pages: u32) -> String {
    component_with_run(&format!(
        r#"(i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72)
        (i32.ne (memory.grow (i32.const {pages})) (i32.const -1)))
      (i32.const 64)"#
    ))
}

/// The per-run memory ceiling denies growth: the guest sees memory.grow
/// fail rather than the run trapping.
#[test]
fn the_memory_limit_denies_growth() {
    let e = Engine::new(Config {
        memory_limit: 8 << 16, // eight pages
        ..Config::default()
    })
    .expect("engine");
    let m = compile(&e, &grow_guest(16));
    let out = e.run(&m, b"", RunOptions::default()).expect("run");
    assert_eq!(out.len(), 0, "growth past the ceiling should be denied");
    let denied = function_wasm_engine::metrics::sample(
        "function_wasm_module_memory_denials_total",
        &[("reason", "limit")],
    )
    .unwrap_or(0.0);
    assert!(denied >= 1.0, "the denial should be counted");
}

/// A guest that spins for the given nanoseconds on the monotonic clock
/// (`wasi:clocks/monotonic-clock`), holding its memory meanwhile.
fn spinning_guest(nanos: u64) -> String {
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

/// The shared pool reserves incrementally: a run holding the pool denies
/// another run's growth (its guest sees memory.grow fail), while the
/// second run's initial memory still fits and runs.
#[test]
fn the_pool_denies_growth_it_cannot_serve() {
    // A component reserves nothing before it runs: its initial memory is
    // charged when instantiation claims it. The prelude's four pages each
    // fill half of the eight-page pool.
    let e = std::sync::Arc::new(
        Engine::new(Config {
            memory_limit: 8 << 16,
            max_total_run_memory: 8 << 16,
            ..Config::default()
        })
        .expect("engine"),
    );
    let blocker = compile(&e, &spinning_guest(300_000_000));
    let grower = compile(&e, &grow_guest(1));

    let eb = std::sync::Arc::clone(&e);
    let blocking = std::thread::spawn(move || {
        eb.run(&blocker, b"", RunOptions::default())
            .expect("blocker run");
    });
    std::thread::sleep(Duration::from_millis(50));
    // Pool: the blocker holds four pages, the grower's initial four fit; its
    // growth to five needs a page the pool cannot serve before the short
    // deadline.
    let opts = RunOptions {
        timeout: Some(Duration::from_millis(100)),
        ..Default::default()
    };
    let out = e.run(&grower, b"", opts).expect("grower run");
    assert_eq!(
        out.len(),
        0,
        "growth the pool cannot serve should be denied"
    );
    blocking.join().expect("join");
    let denied = function_wasm_engine::metrics::sample(
        "function_wasm_module_memory_denials_total",
        &[("reason", "pool")],
    )
    .unwrap_or(0.0);
    assert!(denied >= 1.0, "the denial should be counted");
}

/// The request's own deadline is the hard wall-clock cap whatever the
/// credit: a guest that never returns is interrupted there.
#[test]
fn the_request_deadline_caps_the_run() {
    let e = engine();
    let m = compile(
        &e,
        &component_with_run("(loop $l br $l)\n      i32.const 64"),
    );
    let opts = RunOptions {
        deadline: Some(std::time::Instant::now() + Duration::from_millis(80)),
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let err = e.run(&m, b"", opts).expect_err("should be cut short");
    assert!(
        err.to_string().contains("exceeded its execution deadline"),
        "unexpected error: {err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the hard deadline did not cap the run"
    );
}

/// The environment reaches the guest through WASI 0.2
/// (`wasi:cli/environment`), sorted by key: the guest renders every
/// `key=value` pair, NUL-terminated, as its response.
#[test]
fn env_reaches_the_guest_sorted() {
    let e = engine();
    let wat = component_importing(
        r#"(import "wasi:cli/environment@0.2.0" (instance $environment
    (export "get-environment" (func (result (list (tuple string string)))))))"#,
        r#"(canon lower (func $environment "get-environment") (memory (core memory $libc_inst "memory")) (realloc (core func $libc_inst "cabi_realloc")))"#,
        r#"(import "host" "f" (func $host (param i32)))"#,
        r#"(local $i i32) (local $n i32) (local $ent i32) (local $out i32)
      ;; the list lands at the return pointer: [entries ptr @128][len @132]
      (call $host (i32.const 128))
      (local.set $ent (i32.load (i32.const 128)))
      (local.set $n (i32.load (i32.const 132)))
      (local.set $out (i32.const 4096))
      (block $done
        (loop $each
          (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
          ;; an entry is (key ptr, key len, value ptr, value len)
          (memory.copy (local.get $out) (i32.load (local.get $ent)) (i32.load offset=4 (local.get $ent)))
          (local.set $out (i32.add (local.get $out) (i32.load offset=4 (local.get $ent))))
          (i32.store8 (local.get $out) (i32.const 61))
          (local.set $out (i32.add (local.get $out) (i32.const 1)))
          (memory.copy (local.get $out) (i32.load offset=8 (local.get $ent)) (i32.load offset=12 (local.get $ent)))
          (local.set $out (i32.add (local.get $out) (i32.load offset=12 (local.get $ent))))
          (i32.store8 (local.get $out) (i32.const 0))
          (local.set $out (i32.add (local.get $out) (i32.const 1)))
          (local.set $ent (i32.add (local.get $ent) (i32.const 16)))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $each)))
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 4096))
      (i32.store (i32.const 72) (i32.sub (local.get $out) (i32.const 4096)))
      (i32.const 64)"#,
    );
    let m = compile(&e, &wat);
    let opts = RunOptions {
        env: [
            ("B".to_string(), "2".to_string()),
            ("A".to_string(), "1".to_string()),
        ]
        .into(),
        ..Default::default()
    };
    let out = e.run(&m, b"", opts).expect("run");
    // A BTreeMap serves the environ sorted, like the Go engine's SetEnv.
    assert_eq!(String::from_utf8_lossy(&out), "A=1\0B=2\0");
}

/// The private /tmp is the only pre-opened directory, mounted at /tmp
/// (`wasi:filesystem/preopens`): the guest answers with the first
/// pre-open's path, or nothing when the run has none.
#[test]
fn private_tmp_is_the_only_preopen() {
    let e = engine();
    let wat = component_importing(
        r#"(import "wasi:filesystem/types@0.2.0" (instance $types
    (export "descriptor" (type (sub resource)))))
  (alias export $types "descriptor" (type $descriptor))
  (import "wasi:filesystem/preopens@0.2.0" (instance $preopens
    (alias outer 1 $descriptor (type $d))
    (export "get-directories" (func (result (list (tuple (own $d) string)))))))"#,
        r#"(canon lower (func $preopens "get-directories") (memory (core memory $libc_inst "memory")) (realloc (core func $libc_inst "cabi_realloc")))"#,
        r#"(import "host" "f" (func $host (param i32)))"#,
        r#"(local $ent i32)
      ;; the list lands at the return pointer: [entries ptr @128][len @132]
      (call $host (i32.const 128))
      (local.set $ent (i32.load (i32.const 128)))
      (i32.store8 (i32.const 64) (i32.const 0))
      (if (i32.eqz (i32.load (i32.const 132)))
        (then
          (i32.store (i32.const 68) (i32.const 1024))
          (i32.store (i32.const 72) (i32.const 0)))
        (else
          ;; an entry is (descriptor handle, path ptr, path len)
          (i32.store (i32.const 68) (i32.load offset=4 (local.get $ent)))
          (i32.store (i32.const 72) (i32.load offset=8 (local.get $ent)))))
      (i32.const 64)"#,
    );
    let m = compile(&e, &wat);
    let with_tmp = e
        .run(
            &m,
            b"",
            RunOptions {
                private_tmp: true,
                ..Default::default()
            },
        )
        .expect("run");
    assert_eq!(String::from_utf8_lossy(&with_tmp), "/tmp");
    let without = e.run(&m, b"", RunOptions::default()).expect("run");
    assert_eq!(without.len(), 0, "no pre-open without a private /tmp");
}

/// A component that does not implement the world is refused at load with
/// the world named - the ABI check.
#[test]
fn world_typecheck_refusals() {
    let e = engine();
    for (name, wat) in [
        ("empty", "(component)".to_string()),
        (
            "wrong type",
            format!(
                r#"(component
  (core module $m
    {COMPONENT_CORE_PRELUDE}
    (func (export "run") (param i32) (result i32) i32.const 0))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" u32) (result u32)
    (canon lift (core func $i "run")))
)"#
            ),
        ),
    ] {
        let err = e
            .compile(&wat::parse_str(&wat).expect("wat"))
            .expect_err(name);
        assert!(
            err.to_string()
                .starts_with("component does not implement the wasmfn:function@2.0.0-draft world:"),
            "{name}: unexpected: {err}"
        );
    }
}

/// Records the level and message of every guest log line dispatched while it
/// is the thread's default subscriber. A run drives the guest on the calling
/// thread, so a scoped default sees the lines its log import emits.
#[derive(Clone, Default)]
struct GuestLogLines(std::sync::Arc<std::sync::Mutex<Vec<(tracing::Level, String)>>>);

impl tracing::Subscriber for GuestLogLines {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        // WASI's own tracing runs through the same dispatcher; only the
        // world's log import is under test.
        if event.metadata().target() != "function_wasm_engine::component" {
            return;
        }
        let mut msg = Message(String::new());
        event.record(&mut msg);
        self.0
            .lock()
            .expect("poisoned")
            .push((*event.metadata().level(), msg.0));
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// A component that imports the world's log is linked against the host's
/// typed import, and each of the world's levels lands at the runtime's level
/// of the same name.
#[test]
fn log_levels_land() {
    let e = engine();
    let rsp = response_bytes();
    let lines = [
        (tracing::Level::DEBUG, "at debug"),
        (tracing::Level::INFO, "at info"),
        (tracing::Level::WARN, "at warn"),
        (tracing::Level::ERROR, "at error"),
    ];
    // log(<case i>, <message i>, []) for each case, in the enum's order; the
    // messages sit 16 bytes apart from offset 2048.
    let (mut calls, mut data) = (String::new(), String::new());
    for (i, (_, msg)) in lines.iter().enumerate() {
        let at = 2048 + 16 * i;
        let _ = write!(
            calls,
            "\n      (call $host (i32.const {i}) (i32.const {at}) (i32.const {len}) (i32.const 0) (i32.const 0))",
            len = msg.len()
        );
        let _ = write!(data, "\n    (data (i32.const {at}) \"{msg}\")");
    }
    // The enum must reach the import's signature as an eq-bound type import
    // (a defined type is not importable directly).
    let wat = format!(
        r#"(component
  (type $level_def (enum "debug" "info" "warn" "error"))
  (import "level" (type $level (eq $level_def)))
  (import "log" (func $log (param "level" $level) (param "msg" string) (param "kv" (list (tuple string string)))))
  (core module $libc
    {COMPONENT_CORE_PRELUDE})
  (core instance $libc_inst (instantiate $libc))
  (core func $lowered (canon lower (func $log) (memory (core memory $libc_inst "memory")) (realloc (core func $libc_inst "cabi_realloc"))))
  (core module $m
    (import "env" "memory" (memory 4))
    (import "host" "f" (func $host (param i32 i32 i32 i32 i32)))
    (func (export "run") (param i32 i32) (result i32){calls}
      {ok})
    (data (i32.const 1024) "{rsp_data}"){data})
  (core instance $m_inst (instantiate $m
    (with "env" (instance (export "memory" (memory $libc_inst "memory"))))
    (with "host" (instance (export "f" (func $lowered))))))
  {lift}
)"#,
        ok = ok_result("(i32.const 1024)", &format!("(i32.const {})", rsp.len())),
        rsp_data = wat_bytes(&rsp),
        lift = lift_run("m_inst", "libc_inst"),
    );
    let m = compile(&e, &wat);
    let logged = GuestLogLines::default();
    let out =
        tracing::subscriber::with_default(logged.clone(), || e.run(&m, b"", RunOptions::default()))
            .expect("run");
    assert_eq!(out, rsp);
    let want: Vec<_> = lines
        .iter()
        .map(|(level, msg)| (*level, msg.to_string()))
        .collect();
    assert_eq!(*logged.0.lock().expect("poisoned"), want);
}

/// A serialized artifact loads back through the cache path.
#[test]
fn serialize_round_trip() {
    let e = engine();
    let rsp = response_bytes();
    let m = compile(&e, &fixed_component(&rsp));
    let artifact = e.serialize(&m).expect("serialize");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("c.bin");
    std::fs::write(&path, &artifact).expect("write");
    let loaded = e.deserialize_file(&path).expect("deserialize");
    let out = e.run(&loaded, b"", RunOptions::default()).expect("run");
    assert_eq!(out, rsp);
}

/// inspect reports the world verdict and the component's items, and a
/// serialized artifact is still refused as a module source.
#[test]
fn inspection() {
    let e = engine();
    let shape = e
        .inspect(&wat::parse_str(fixed_component(&response_bytes())).expect("wat"))
        .expect("inspect");
    assert_eq!(shape.abi_error, None);
    assert!(
        shape
            .exports
            .iter()
            .any(|x| x.name == "run" && x.kind == "func")
    );
    assert!(shape.imports.is_empty());
    assert!(shape.host_imports.is_empty());

    let bad = e.inspect(b"(component)".as_ref());
    assert!(bad.is_err(), "text is not a module source");
}
