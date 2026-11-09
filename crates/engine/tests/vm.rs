//! The engine over a real Firecracker VM: the process contract end to end
//! with the committed fixture process. Needs KVM, the Firecracker binary,
//! a guest kernel and the init and fixture binaries built for musl, named
//! by GUESTFN_FIRECRACKER, GUESTFN_KERNEL, GUESTFN_INIT and
//! GUESTFN_FIXTURE (`make oci.tools` and `make oci.init` set them up); without them every test
//! is skipped, or fails under GUESTFN_REQUIRE_KVM=1, which the OCI
//! workflow sets.

use std::path::PathBuf;
use std::time::Duration;

use guestfn_engine::oci::testing::fixture_image;
use guestfn_engine::oci::{Oci, Options};
use guestfn_engine::{Config, Engine, RunOptions};

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
            if std::env::var_os("GUESTFN_REQUIRE_KVM").is_some() {
                panic!(
                    "GUESTFN_REQUIRE_KVM is set but GUESTFN_FIRECRACKER, GUESTFN_KERNEL, GUESTFN_INIT or GUESTFN_FIXTURE is not"
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
    engine_with(t, config, false)
}

fn engine_with(t: &Tools, config: Config, snapshots: bool) -> Oci {
    Oci::new(
        config,
        Options {
            vmm: t.firecracker.clone(),
            kernel: t.kernel.clone(),
            init: t.init.clone(),
            vcpus: 1,
            snapshots,
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

/// With snapshots on (#255), the first run of an image boots and
/// snapshots the VM with its init parked, and every later run restores
/// it: the process contract is the same on both paths.
#[test]
fn a_restored_clone_runs_like_a_boot() {
    let Some(t) = tools() else { return };
    let e = engine_with(&t, Config::default(), true);
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "echo"],
            "app",
            &[],
        ))
        .unwrap();
    for (i, request) in [vec![7u8; 300_000], b"again".to_vec(), b"and again".to_vec()]
        .into_iter()
        .enumerate()
    {
        let started = std::time::Instant::now();
        let out = e.run(&m, &request, opts()).unwrap();
        // The first run boots and snapshots; the others restore.
        eprintln!("snapshots on: run {i} took {:?}", started.elapsed());
        assert_eq!(out, request);
    }
    let booting = engine(&t, Config::default());
    let booted = booting
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "echo"],
            "app",
            &[],
        ))
        .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(booting.run(&booted, b"again", opts()).unwrap(), b"again");
    eprintln!("snapshots off: a run took {:?}", started.elapsed());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "exit", "3"],
            "",
            &[],
        ))
        .unwrap();
    let err = e.run(&m, b"", opts()).unwrap_err().0;
    assert!(err.starts_with("process exited with status 3"), "{err}");
}

/// A restored clone is as fresh as a boot: its random state is its own,
/// its clock is the host's, and nothing one clone wrote reaches the next.
#[test]
fn restored_clones_share_nothing() {
    let Some(t) = tools() else { return };
    let e = engine_with(&t, Config::default(), true);
    let random = e
        .compile(&fixture_image(&t.fixture, &["/fixture", "random"], "", &[]))
        .unwrap();
    let draws: Vec<Vec<u8>> = (0..4)
        .map(|_| e.run(&random, b"", opts()).unwrap())
        .collect();
    for (i, a) in draws.iter().enumerate() {
        assert_eq!(a.len(), 32, "{:?}", String::from_utf8_lossy(a));
        for b in &draws[i + 1..] {
            assert_ne!(a, b, "two clones drew the same randomness");
        }
    }
    let clock = e
        .compile(&fixture_image(&t.fixture, &["/fixture", "clock"], "", &[]))
        .unwrap();
    // The first run made the snapshot a moment ago; wait, so a clock left
    // at the snapshot's moment would show.
    let _ = e.run(&clock, b"", opts()).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let guest: u64 = String::from_utf8(e.run(&clock, b"", opts()).unwrap())
        .unwrap()
        .parse()
        .unwrap();
    let host = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        host.abs_diff(guest) <= 1,
        "guest clock {guest}, host {host}"
    );
    let tmp = e
        .compile(&fixture_image(&t.fixture, &["/fixture", "tmp"], "", &[]))
        .unwrap();
    let first = e.run(&tmp, b"", opts()).unwrap();
    let second = e.run(&tmp, b"", opts()).unwrap();
    assert_eq!(first, second, "a clone saw what another wrote");
}

/// Boot against restore, by hand (#255):
///
///   GUESTFN_VM_TIMING=1 cargo test -p guestfn-engine --no-default-features \
///     --features oci,testutil --release --test vm -- --ignored --nocapture boot_against_restore
///
/// with the suite's GUESTFN_* files set. Prints each path's whole-run
/// latency over the echo fixture; GUESTFN_VM_TIMING adds the sandbox's
/// phases per run.
#[test]
#[ignore = "a hand-run benchmark"]
fn boot_against_restore() {
    let Some(t) = tools() else { return };
    let runs: usize = std::env::var("GUESTFN_BENCH_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let image = fixture_image(&t.fixture, &["/fixture", "echo"], "app", &[]);
    for (name, snapshots) in [("boot", false), ("restore", true)] {
        let e = engine_with(&t, Config::default(), snapshots);
        let m = e.compile(&image).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(e.run(&m, b"warm", opts()).unwrap(), b"warm");
        let first = started.elapsed();
        let mut millis: Vec<f64> = (0..runs)
            .map(|_| {
                let started = std::time::Instant::now();
                assert_eq!(e.run(&m, b"request", opts()).unwrap(), b"request");
                started.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        millis.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let at = |q: f64| millis[((millis.len() - 1) as f64 * q).round() as usize];
        eprintln!(
            "bench {name}: first {:.0} ms; then n={runs} min {:.0} p50 {:.0} p90 {:.0} max {:.0} ms",
            first.as_secs_f64() * 1e3,
            at(0.0),
            at(0.5),
            at(0.9),
            at(1.0),
        );
    }
}
