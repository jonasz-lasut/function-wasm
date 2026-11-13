//! The engine over a real Firecracker VM: the contract end to end with
//! the committed fixture function. Needs KVM, the Firecracker binary,
//! a guest kernel and the init and fixture binaries built for musl, named
//! by GUESTFN_FIRECRACKER, GUESTFN_KERNEL, GUESTFN_INIT and
//! GUESTFN_FIXTURE (`make oci.tools` and `make oci.init` set them up); without them every test
//! is skipped, or fails under GUESTFN_REQUIRE_KVM=1, which the OCI
//! workflow sets. A skip is said on the process's own stderr (`SKIPPED:
//! ...`, with the count so far), since the harness shows nothing a passing
//! test printed.

use std::path::PathBuf;
use std::time::Duration;

use guestfn_engine::{ArtifactStore, Config, Engine, RunOptions};
use guestfn_engine_oci::testing::fixture_image;
use guestfn_engine_oci::{Oci, Options};

use function_sdk_rust::proto::v1::{RequestMeta, RunFunctionRequest, RunFunctionResponse};
use guestfn_engine_oci::Module;
use prost::Message as _;

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
            report_skip(
                "the VM tools are not named (GUESTFN_FIRECRACKER, GUESTFN_KERNEL, GUESTFN_INIT, GUESTFN_FIXTURE; make oci.test sets them)",
            );
            None
        }
    }
}

/// Says that the calling test asserted nothing, on the process's own
/// stderr: the harness captures what a passing test prints with eprintln!,
/// so a skipped suite would otherwise read as a passed one. The count is
/// this binary's so far, and the last line carries the total.
fn report_skip(reason: &str) {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SKIPPED: AtomicUsize = AtomicUsize::new(0);
    let skipped = SKIPPED.fetch_add(1, Ordering::SeqCst) + 1;
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("a test");
    // One write, so lines from tests running side by side stay whole.
    let line = format!(
        "SKIPPED: {test} asserted nothing: {reason} ({skipped} skipped so far in the VM suite)\n"
    );
    let _ = std::io::stderr().write_all(line.as_bytes());
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

/// Calls the function with a tag and returns what it answered in its
/// response's tag.
fn ask(e: &Oci, m: &Module, tag: &str, o: RunOptions) -> Result<String, String> {
    let request = RunFunctionRequest {
        meta: Some(RequestMeta {
            tag: tag.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let response = e.run(m, &request, o).map_err(|e| e.to_string())?;
    Ok(RunFunctionResponse::decode(response.as_slice())
        .expect("a response")
        .meta
        .map(|m| m.tag)
        .unwrap_or_default())
}

#[test]
fn a_function_answers_the_call() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "echo"],
            "app",
            &[],
        ))
        .unwrap();
    let request = "7".repeat(300_000);
    let started = std::time::Instant::now();
    let out = ask(&e, &m, &request, opts()).unwrap();
    eprintln!("first run took {:?}", started.elapsed());
    assert_eq!(out, request);
    // A second run boots a fresh VM; nothing of the first is seen.
    let out = ask(&e, &m, "again", opts()).unwrap();
    assert_eq!(out, "again");
}

#[test]
fn the_process_runs_as_the_image_says() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "id"],
            "app",
            &[],
        ))
        .unwrap();
    let out = ask(&e, &m, "", opts()).unwrap();
    assert_eq!(out.trim(), "uid=1000 gid=1000 groups=");
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "cwd"],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(ask(&e, &m, "", opts()).unwrap().trim(), "/home/app");
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "env"],
            "app",
            &["FROM_IMAGE=yes"],
        ))
        .unwrap();
    let mut o = opts();
    o.env
        .insert("FROM_BINDING".to_string(), "bound".to_string());
    let out = ask(&e, &m, "", o).unwrap();
    assert!(out.contains("FROM_IMAGE=yes\n"), "{out}");
    assert!(out.contains("FROM_BINDING=bound\n"), "{out}");
    assert!(out.contains("PATH=/usr/local/sbin"), "{out}");
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "hostname"],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(ask(&e, &m, "", opts()).unwrap().trim(), "guestfn");
}

#[test]
fn the_guest_has_the_hosts_randomness() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "rng"],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(ask(&e, &m, "", opts()).unwrap().trim(), "virtio_rng.0");
}

/// The init leaves the process no capability in any set and no way to gain
/// one through a setuid binary or a file capability.
#[test]
fn the_process_holds_no_privilege() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "privileges"],
            "app",
            &[],
        ))
        .unwrap();
    let none = "0000000000000000";
    assert_eq!(
        ask(&e, &m, "", opts()).unwrap().trim(),
        format!(
            "CapInh={none} CapPrm={none} CapEff={none} CapBnd={none} CapAmb={none} NoNewPrivs=1"
        )
    );
}

/// What the image and the process supply is mounted nosuid and nodev: the
/// root filesystem through its overlay, and /tmp. A device node there
/// opens no device and a setuid bit changes nothing, whatever the image
/// carries.
#[test]
fn the_process_s_filesystems_are_nosuid_and_nodev() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "mounts"],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(
        ask(&e, &m, "", opts()).unwrap().trim(),
        "/=nosuid,nodev /tmp=nosuid,nodev"
    );
}

#[test]
fn the_root_filesystem_is_writable_and_discarded() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "tmp"],
            "app",
            &[],
        ))
        .unwrap();
    for _ in 0..2 {
        let out = ask(&e, &m, "", opts()).unwrap();
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
            &["/fixture", "--insecure", "exit", "3"],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(
        ask(&e, &m, "", opts()).unwrap_err(),
        "process exited with status 3 before answering the call: exiting as asked"
    );
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "stderr"],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(ask(&e, &m, "with logs", opts()).unwrap(), "with logs");
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "silent"],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(
        ask(&e, &m, "", opts()).unwrap_err(),
        "process exited with status 0 before answering the call"
    );
    let m = e
        .compile(&fixture_image(&t.fixture, &["/nowhere"], "app", &[]))
        .unwrap();
    let err = ask(&e, &m, "", opts()).unwrap_err();
    assert!(
        err.starts_with("cannot start the process: cannot start /nowhere:"),
        "{err}"
    );
}

/// A process that writes more than the host keeps of a run's output runs
/// as any other: the host reads all of it, so the process never blocks on
/// its own output, and the last line is in what is kept.
#[test]
fn output_over_the_bound_changes_no_result() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let bytes = (8 * guestfn_engine_oci::sandbox::OUTPUT_CAP).to_string();
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "flood", &bytes],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(
        ask(&e, &m, "after the flood", opts()).unwrap(),
        "after the flood"
    );
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "flood-exit", &bytes],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(
        ask(&e, &m, "", opts()).unwrap_err(),
        "process exited with status 3 before answering the call: the flood is over"
    );
}

/// A function that answers with an error fails the run with its words.
#[test]
fn a_function_s_error_is_the_run_s() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "refuse"],
            "app",
            &[],
        ))
        .unwrap();
    assert_eq!(
        ask(&e, &m, "", opts()).unwrap_err(),
        "run returned an error: refused as asked (gRPC status FailedPrecondition)"
    );
}

/// The manifest's `oci.args` replace the image's command and keep its
/// entrypoint: an SDK image is started with `--insecure` this way.
#[test]
fn the_manifest_s_args_start_the_function() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(&t.fixture, &["/fixture"], "app", &[]))
        .unwrap();
    let mut o = opts();
    o.args = vec!["--insecure".to_string(), "echo".to_string()];
    assert_eq!(ask(&e, &m, "with args", o).unwrap(), "with args");
    // No flag: the SDK's server refuses to start without credentials.
    let err = ask(&e, &m, "", opts()).unwrap_err();
    assert!(
        err.starts_with("process exited with status 1 before answering the call"),
        "{err}"
    );
}

/// The budget covers the boot, and a guest that has not connected when it
/// runs out is lost, not late (the sandbox's sentence, not the module's
/// deadline). So the limit is sized from a boot measured here: three times
/// what an answering function took from the call to its answer, on this
/// host, beside whatever else the suite is running, and never under three
/// seconds. The run must end with the deadline's sentence, soon after the
/// limit.
#[test]
fn the_deadline_stops_a_spinning_process() {
    let Some(t) = tools() else { return };
    // A ceiling no measured limit reaches: the limit is the step's own.
    let e = engine(
        &t,
        Config {
            timeout: Duration::from_secs(600),
            ..Config::default()
        },
    );
    let echo = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "echo"],
            "app",
            &[],
        ))
        .unwrap();
    let started = std::time::Instant::now();
    ask(&e, &echo, "boot", opts()).unwrap();
    let boot = started.elapsed();
    let limit = Duration::from_secs((boot * 3).as_secs_f64().ceil().max(3.0) as u64);
    eprintln!("a boot and an answer took {boot:?}: the limit is {limit:?}");
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "spin"],
            "app",
            &[],
        ))
        .unwrap();
    let mut o = opts();
    o.timeout = Some(limit);
    let started = std::time::Instant::now();
    assert_eq!(
        ask(&e, &m, "", o).unwrap_err(),
        format!(
            "run failed: module exceeded its execution deadline ({})",
            guestfn_engine::duration::format(limit)
        )
    );
    let took = started.elapsed();
    assert!(took >= limit, "ended after {took:?}, before the limit");
    assert!(
        took < limit + Duration::from_secs(7),
        "ended after {took:?}"
    );
}

#[test]
fn the_memory_limit_kills_a_growing_process() {
    let Some(t) = tools() else { return };
    let e = engine(&t, Config::default());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "grow", "200"],
            "app",
            &[],
        ))
        .unwrap();
    let mut o = opts();
    o.memory_limit = Some(96 << 20);
    let err = ask(&e, &m, "", o).unwrap_err();
    // The limit's own sentence and no other: a bare `process was killed by
    // signal 9` here means the init no longer recognises the kernel's OOM
    // kill (the cgroup, its kernel log check or the exit's oom field), and
    // a module's author would be told of a signal instead of the limit.
    assert!(
        err.starts_with("process was killed: the run's memory limit of 96Mi was exceeded"),
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
            &["/fixture", "--insecure", "echo"],
            "app",
            &[],
        ))
        .unwrap();
    for (i, request) in [
        "7".repeat(300_000),
        "again".to_string(),
        "and again".to_string(),
    ]
    .into_iter()
    .enumerate()
    {
        let started = std::time::Instant::now();
        let out = ask(&e, &m, &request, opts()).unwrap();
        // The first run boots and snapshots; the others restore.
        eprintln!("snapshots on: run {i} took {:?}", started.elapsed());
        assert_eq!(out, request);
    }
    let booting = engine(&t, Config::default());
    let booted = booting
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "echo"],
            "app",
            &[],
        ))
        .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(ask(&booting, &booted, "again", opts()).unwrap(), "again");
    eprintln!("snapshots off: a run took {:?}", started.elapsed());
    let m = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "exit", "3"],
            "app",
            &[],
        ))
        .unwrap();
    let err = ask(&e, &m, "", opts()).unwrap_err();
    assert!(err.starts_with("process exited with status 3"), "{err}");
}

/// A restored clone is as fresh as a boot: its random state is its own,
/// its clock is the host's, and nothing one clone wrote reaches the next.
#[test]
fn restored_clones_share_nothing() {
    let Some(t) = tools() else { return };
    let e = engine_with(&t, Config::default(), true);
    let random = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "random"],
            "app",
            &[],
        ))
        .unwrap();
    let draws: Vec<String> = (0..4)
        .map(|_| ask(&e, &random, "", opts()).unwrap())
        .collect();
    for (i, a) in draws.iter().enumerate() {
        assert_eq!(a.len(), 32, "{a:?}");
        for b in &draws[i + 1..] {
            assert_ne!(a, b, "two clones drew the same randomness");
        }
    }
    let clock = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "clock"],
            "app",
            &[],
        ))
        .unwrap();
    // The first run made the snapshot a moment ago; wait, so a clock left
    // at the snapshot's moment would show.
    let _ = ask(&e, &clock, "", opts()).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let guest: u64 = ask(&e, &clock, "", opts()).unwrap().parse().unwrap();
    let host = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        host.abs_diff(guest) <= 1,
        "guest clock {guest}, host {host}"
    );
    let leftover = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "leftover"],
            "app",
            &[],
        ))
        .unwrap();
    for _ in 0..3 {
        let seen = ask(&e, &leftover, "", opts()).unwrap();
        assert_eq!(
            seen, "tmp=absent root=absent",
            "a clone saw what another wrote"
        );
    }
}

/// A store for these tests: a directory with no key, which can be told to
/// refuse what it has, as a store does for another process's entries.
struct DirStore {
    dir: PathBuf,
    refuse: std::sync::atomic::AtomicBool,
    /// Files sealed: two for every snapshot made.
    sealed: std::sync::atomic::AtomicUsize,
}

impl DirStore {
    fn new(dir: &std::path::Path) -> std::sync::Arc<DirStore> {
        std::sync::Arc::new(DirStore {
            dir: dir.to_path_buf(),
            refuse: Default::default(),
            sealed: Default::default(),
        })
    }

    fn snapshots_made(&self) -> usize {
        self.sealed.load(std::sync::atomic::Ordering::Relaxed) / 2
    }
}

impl ArtifactStore for DirStore {
    fn begin(&self, name: &str) -> Result<PathBuf, String> {
        Ok(self.dir.join(format!("{name}.put-keep")))
    }

    fn seal(&self, name: &str) -> Result<PathBuf, String> {
        let entry = self.dir.join(name);
        std::fs::rename(self.dir.join(format!("{name}.put-keep")), &entry)
            .map_err(|e| e.to_string())?;
        self.sealed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(entry)
    }

    fn sealed(&self, name: &str) -> Option<PathBuf> {
        let entry = self.dir.join(name);
        if self.refuse.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = std::fs::remove_file(&entry);
            return None;
        }
        entry.is_file().then_some(entry)
    }

    fn used(&self, _: &std::path::Path) {}

    fn hold(&self, _: &std::path::Path) {}

    fn release(&self, _: &std::path::Path) {}
}

/// A stored image's snapshot is kept in the store beside it
/// (docs/one-pager-vm-snapshots.md): two entries under the root
/// filesystem's own name, the memory file sparse. A file the sweep took is
/// made again, an image loaded again takes the snapshot the store still
/// has, and one the store refuses is made again.
#[cfg(unix)]
#[test]
fn a_stored_image_keeps_its_snapshot_in_the_store() {
    use std::os::unix::fs::MetadataExt as _;
    let Some(t) = tools() else { return };
    let e = engine_with(&t, Config::default(), true);
    let dir = tempfile::tempdir().unwrap();
    let store = DirStore::new(dir.path());
    e.keep_in(std::sync::Arc::clone(&store) as std::sync::Arc<dyn ArtifactStore>);
    let compiled = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "echo"],
            "app",
            &[],
        ))
        .unwrap();
    let entry = dir.path().join("sha256-test");
    std::fs::write(&entry, e.serialize(&compiled).unwrap()).unwrap();
    let stored = e.stored(compiled, &entry);
    let kept = || {
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "sha256-test")
            .collect();
        names.sort();
        names
    };

    assert_eq!(ask(&e, &stored, "one", opts()).unwrap(), "one");
    let names = kept();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(
        names[0].starts_with("sha256-test.vm-") && names[0].ends_with(".mem"),
        "{names:?}"
    );
    assert!(
        names[1].starts_with("sha256-test.vm-") && names[1].ends_with(".state"),
        "{names:?}"
    );
    let mem = dir.path().join(&names[0]);
    let state = dir.path().join(&names[1]);
    let meta = std::fs::metadata(&mem).unwrap();
    assert!(
        meta.blocks() * 512 < meta.len() / 2,
        "the memory file is sparse: {} bytes long, {} on disk",
        meta.len(),
        meta.blocks() * 512
    );
    assert_eq!(ask(&e, &stored, "two", opts()).unwrap(), "two");

    // The sweep takes the memory file from under the loaded image.
    std::fs::remove_file(&mem).unwrap();
    assert_eq!(ask(&e, &stored, "three", opts()).unwrap(), "three");
    assert!(mem.is_file(), "made again");

    // The image loaded again: the store still has the snapshot.
    let made = std::fs::metadata(&state).unwrap().ino();
    let again = e.deserialize_file(&entry).unwrap();
    assert_eq!(ask(&e, &again, "four", opts()).unwrap(), "four");
    assert_eq!(
        std::fs::metadata(&state).unwrap().ino(),
        made,
        "restored from the kept snapshot"
    );

    // Loaded once more with a store that refuses what it has.
    store
        .refuse
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let refused = e.deserialize_file(&entry).unwrap();
    assert_eq!(ask(&e, &refused, "five", opts()).unwrap(), "five");
    assert_eq!(kept().len(), 2, "made again after the refusal");
    assert_eq!(ask(&e, &refused, "six", opts()).unwrap(), "six");
}

/// A stored image for the snapshot tests: the echo fixture compiled,
/// written into dir as the store would and continued from there.
fn stored_echo(t: &Tools, e: &Oci, dir: &std::path::Path) -> guestfn_engine_oci::Module {
    let compiled = e
        .compile(&fixture_image(
            &t.fixture,
            &["/fixture", "--insecure", "echo"],
            "app",
            &[],
        ))
        .unwrap();
    let entry = dir.join("sha256-test");
    std::fs::write(&entry, e.serialize(&compiled).unwrap()).unwrap();
    e.stored(compiled, &entry)
}

/// Warm-up takes the snapshot, so an image's first request restores it
/// and pays for no boot.
#[test]
fn the_warm_up_makes_the_snapshot() {
    let Some(t) = tools() else { return };
    let e = engine_with(&t, Config::default(), true);
    let dir = tempfile::tempdir().unwrap();
    let store = DirStore::new(dir.path());
    e.keep_in(std::sync::Arc::clone(&store) as std::sync::Arc<dyn ArtifactStore>);
    let stored = stored_echo(&t, &e, dir.path());
    e.warm(&stored).unwrap();
    assert_eq!(
        store.snapshots_made(),
        1,
        "the snapshot is there before a run"
    );
    let started = std::time::Instant::now();
    assert_eq!(ask(&e, &stored, "first", opts()).unwrap(), "first");
    eprintln!("the first run after warm-up took {:?}", started.elapsed());
    assert_eq!(store.snapshots_made(), 1, "the first run restored it");
    // A second warm-up of a loaded image finds the snapshot it has.
    e.warm(&stored).unwrap();
    assert_eq!(store.snapshots_made(), 1);
}

/// Concurrent first runs of one image don't wait for one another's
/// snapshot: one makes it and the others boot, since making a snapshot
/// takes longer than a boot. Every run answers, and one snapshot exists.
#[test]
fn concurrent_first_runs_make_one_snapshot_and_do_not_wait() {
    let Some(t) = tools() else { return };
    let e = std::sync::Arc::new(engine_with(&t, Config::default(), true));
    let dir = tempfile::tempdir().unwrap();
    let store = DirStore::new(dir.path());
    e.keep_in(std::sync::Arc::clone(&store) as std::sync::Arc<dyn ArtifactStore>);
    let stored = stored_echo(&t, &e, dir.path());
    let runs: Vec<_> = (0..3u8)
        .map(|i| {
            let e = std::sync::Arc::clone(&e);
            let m = std::sync::Arc::clone(&stored);
            std::thread::spawn(move || ask(&e, &m, &(b'a' + i).to_string(), opts()))
        })
        .collect();
    for (i, run) in runs.into_iter().enumerate() {
        assert_eq!(run.join().unwrap().unwrap(), (b'a' + i as u8).to_string());
    }
    assert_eq!(store.snapshots_made(), 1);
    assert_eq!(ask(&e, &stored, "after", opts()).unwrap(), "after");
    assert_eq!(store.snapshots_made(), 1);
}

/// Boot against restore, by hand (#255):
///
///   GUESTFN_VM_TIMING=1 cargo test -p guestfn-engine-oci --release --test vm -- \
///     --ignored --nocapture boot_against_restore
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
    let image = fixture_image(&t.fixture, &["/fixture", "--insecure", "echo"], "app", &[]);
    for (name, snapshots) in [("boot", false), ("restore", true)] {
        let e = engine_with(&t, Config::default(), snapshots);
        let m = e.compile(&image).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(ask(&e, &m, "warm", opts()).unwrap(), "warm");
        let first = started.elapsed();
        let mut millis: Vec<f64> = (0..runs)
            .map(|_| {
                let started = std::time::Instant::now();
                assert_eq!(ask(&e, &m, "request", opts()).unwrap(), "request");
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
