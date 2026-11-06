//! The engine over a real Firecracker VM: the process contract end to end
//! with the committed fixture process. Needs KVM, the Firecracker binary,
//! a guest kernel and the init and fixture binaries built for musl, named
//! by GUESTFN_FIRECRACKER, GUESTFN_KERNEL, GUESTFN_INIT and
//! GUESTFN_FIXTURE (`make oci.tools` sets them up); without them every test
//! is skipped, or fails under FUNCTION_WASM_REQUIRE_KVM=1, which the OCI
//! workflow sets.

use std::path::PathBuf;
use std::time::Duration;

use function_wasm_engine_oci::testing::fixture_image;
use function_wasm_engine_oci::{Config, Engine, Oci, Options, RunOptions};

struct Tools {
    firecracker: PathBuf,
    kernel: PathBuf,
    init: PathBuf,
    fixture: Vec<u8>,
}

/// The tools, or None (a skip) when the environment has none and does not
/// insist.
fn tools() -> Option<Tools> {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    match (
        var("GUESTFN_FIRECRACKER"),
        var("GUESTFN_KERNEL"),
        var("GUESTFN_INIT"),
        var("GUESTFN_FIXTURE"),
    ) {
        (Some(firecracker), Some(kernel), Some(init), Some(fixture)) => Some(Tools {
            firecracker,
            kernel,
            init,
            fixture: std::fs::read(&fixture).expect("the fixture binary"),
        }),
        _ => {
            if std::env::var_os("FUNCTION_WASM_REQUIRE_KVM").is_some() {
                panic!(
                    "FUNCTION_WASM_REQUIRE_KVM is set but GUESTFN_FIRECRACKER, GUESTFN_KERNEL, GUESTFN_INIT or GUESTFN_FIXTURE is not"
                );
            }
            eprintln!(
                "skipping: GUESTFN_FIRECRACKER, GUESTFN_KERNEL, GUESTFN_INIT and GUESTFN_FIXTURE name the VM tools"
            );
            None
        }
    }
}

fn engine(t: &Tools, config: Config) -> Oci {
    Oci::new(
        config,
        Options {
            vmm: t.firecracker.clone(),
            kernel: t.kernel.clone(),
            init: t.init.clone(),
            vcpus: 1,
        },
    )
    .expect("engine")
}

fn opts() -> RunOptions {
    RunOptions {
        module: "oci fixture".to_string(),
        digest: "sha256:fixture".to_string(),
        ..Default::default()
    }
}

#[test]
fn a_process_echoes_the_request_as_the_response() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "echo"],
            "app",
            &[],
        ))
        .unwrap();
    let request = vec![7u8; 300_000];
    let started = std::time::Instant::now();
    let out = e.run(&m, &request, opts()).unwrap();
    eprintln!("first run took {:?}", started.elapsed());
    assert_eq!(out, request);
    // A second run boots a fresh VM; nothing of the first is seen.
    let out = e.run(&m, b"again", opts()).unwrap();
    assert_eq!(out, b"again");
}

#[test]
fn the_process_runs_as_the_image_says() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(&t.fixture, &["/fixture", "id"], "app", &[]))
        .unwrap();
    let out = String::from_utf8(e.run(&m, b"", opts()).unwrap()).unwrap();
    assert_eq!(out.trim(), "uid=1000 gid=1000 groups=");
    let m = e
        .compile(&fixture_image(&t.fixture, &["/fixture", "cwd"], "", &[]))
        .unwrap();
    assert_eq!(
        String::from_utf8(e.run(&m, b"", opts()).unwrap())
            .unwrap()
            .trim(),
        "/home/app"
    );
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "env"],
            "",
            &["FROM_IMAGE=yes"],
        ))
        .unwrap();
    let mut o = opts();
    o.env
        .insert("FROM_BINDING".to_string(), "bound".to_string());
    let out = String::from_utf8(e.run(&m, b"", o).unwrap()).unwrap();
    assert!(out.contains("FROM_IMAGE=yes\n"), "{out}");
    assert!(out.contains("FROM_BINDING=bound\n"), "{out}");
    assert!(out.contains("PATH=/usr/local/sbin"), "{out}");
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "hostname"],
            "",
            &[],
        ))
        .unwrap();
    assert_eq!(
        String::from_utf8(e.run(&m, b"", opts()).unwrap())
            .unwrap()
            .trim(),
        "guestfn"
    );
}

#[test]
fn the_root_filesystem_is_writable_and_discarded() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(&t.fixture, &["/fixture", "tmp"], "", &[]))
        .unwrap();
    for _ in 0..2 {
        let out = String::from_utf8(e.run(&m, b"", opts()).unwrap()).unwrap();
        assert_eq!(out.trim(), "tmp=scratch root=writable");
    }
}

#[test]
fn a_non_zero_exit_names_the_status_and_the_last_line() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "exit", "3"],
            "",
            &[],
        ))
        .unwrap();
    assert_eq!(
        e.run(&m, b"", opts()).unwrap_err().0,
        "process exited with status 3: exiting as asked"
    );
    let m = e
        .compile(&fixture_image(&t.fixture, &["/fixture", "stderr"], "", &[]))
        .unwrap();
    assert_eq!(e.run(&m, b"with logs", opts()).unwrap(), b"with logs");
    let m = e
        .compile(&fixture_image(&t.fixture, &["/fixture", "silent"], "", &[]))
        .unwrap();
    assert_eq!(
        e.run(&m, b"", opts()).unwrap_err().0,
        "process exited with status 0 and wrote no response to its standard output"
    );
    let m = e
        .compile(&fixture_image(&t.fixture, &["/nowhere"], "", &[]))
        .unwrap();
    let err = e.run(&m, b"", opts()).unwrap_err().0;
    assert!(
        err.starts_with("cannot start the process: cannot start /nowhere:"),
        "{err}"
    );
}

#[test]
fn the_deadline_stops_a_spinning_process() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(&t.fixture, &["/fixture", "spin"], "", &[]))
        .unwrap();
    let mut o = opts();
    o.timeout = Some(Duration::from_secs(3));
    let started = std::time::Instant::now();
    assert_eq!(
        e.run(&m, b"", o).unwrap_err().0,
        "run failed: module exceeded its execution deadline (3s)"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn the_memory_limit_kills_a_growing_process() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "grow", "200"],
            "",
            &[],
        ))
        .unwrap();
    let mut o = opts();
    o.memory_limit = Some(96 << 20);
    let err = e.run(&m, b"", o).unwrap_err().0;
    assert!(
        err.starts_with("process was killed: the run's memory limit of 96Mi was exceeded")
            || err.starts_with("process was killed by signal 9"),
        "{err}"
    );
}
