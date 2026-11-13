//! What an answered run leaves to the engine after its response is back
//! with the caller: the VM still to stop, and with it the run's slot, its
//! memory, its image, its metrics and its log. The sandbox is the fake,
//! whose `stopping` hook stands where Firecracker stops a VM, so a test
//! holds a VM up for as long as it looks. tests/vm.rs has what only a
//! real VM shows.
//!
//! The metrics and the log capture are the process's, so the tests take
//! turns.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use guestfn_engine::{ArtifactStore, Config, Engine, RunFailure, RunOptions, metrics};
use guestfn_engine_oci::sandbox::{Ended, OUTPUT_CAP, Outcome, Output, VmSpec};
use guestfn_engine_oci::testing::{FakeSandbox, Logs, fixture_image};
use guestfn_engine_oci::{Module, Oci, Options};
use guestfn_oci_protocol::Exit;

fn serial() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(|e| e.into_inner())
}

/// Closed until a test opens it: where a fake VM's stopping waits.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.opened.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

fn engine(config: Config, sandbox: FakeSandbox) -> Oci {
    let dir = tempfile::tempdir().unwrap();
    let init = dir.path().join("init");
    std::fs::write(&init, b"#!/bin/sh\n").unwrap();
    let options = Options {
        vmm: dir.path().join("firecracker"),
        kernel: dir.path().join("vmlinux"),
        init,
        vcpus: 1,
        snapshots: false,
    };
    // The engine has read the init by the time the directory goes.
    Oci::with_sandbox(config, options, Box::new(sandbox)).unwrap()
}

fn image(e: &Oci) -> Module {
    e.compile(&fixture_image(b"x", &["/x"], "app", &[]))
        .unwrap()
}

/// A sandbox whose every run is answered with its request, the VM stopped
/// when `stopping` returns.
fn answering(stopping: impl Fn() + Send + Sync + 'static) -> FakeSandbox {
    FakeSandbox::new(Default::default(), |spec: &VmSpec<'_>| Outcome {
        log: Output::default(),
        end: Ended::Answered(spec.request.to_vec()),
        console: String::new(),
    })
    .stopping(stopping)
}

fn opts(module: &str) -> RunOptions {
    RunOptions {
        module: module.to_string(),
        digest: format!("sha256:{module}"),
        ..Default::default()
    }
}

/// A run that gives up waiting for its bounds after a moment.
fn impatient(module: &str) -> RunOptions {
    RunOptions {
        deadline: Some(Instant::now() + Duration::from_millis(150)),
        ..opts(module)
    }
}

fn in_flight() -> f64 {
    metrics::sample("guestfn_module_runs_in_flight", &[]).unwrap_or(0.0)
}

fn runs(outcome: &str) -> f64 {
    metrics::sample(
        "guestfn_module_run_duration_seconds",
        &[("outcome", outcome)],
    )
    .unwrap_or(0.0)
}

/// The memory one VM of the default machine reserves.
fn one_vm() -> u64 {
    Config::default().memory_limit + (64 << 20)
}

/// The response is the caller's while the VM is still up, and the run's
/// slot stays the VM's: the next run waits for it until the VM is gone,
/// and the run is counted in flight until then. Its duration is observed
/// at the response, once: the series is what a caller waits.
#[test]
fn an_answered_run_keeps_its_slot_until_its_vm_is_stopped() {
    let _turn = serial();
    let gate = Arc::new(Gate::default());
    let held = Arc::clone(&gate);
    let e = engine(
        Config {
            max_concurrent_runs: 1,
            ..Config::default()
        },
        answering(move || held.wait()),
    );
    let m = image(&e);
    let (flying, observed) = (in_flight(), runs("ok"));

    // Returned here, with the fake VM held up: the caller did not wait.
    let out = e.run(&m, b"one", opts("slot")).unwrap();
    assert_eq!(out, b"one");
    assert_eq!(in_flight(), flying + 1.0, "the run is still in flight");
    assert_eq!(runs("ok"), observed + 1.0, "and observed at its response");
    let err = e.run(&m, b"two", impatient("slot")).unwrap_err();
    assert_eq!(
        (err.kind(), err.message()),
        (
            RunFailure::Waiting,
            "waiting for a run slot: deadline exceeded"
        )
    );

    gate.open();
    e.settle();
    assert_eq!(in_flight(), flying);
    assert_eq!(runs("ok"), observed + 1.0, "observed once");
    assert_eq!(e.run(&m, b"three", impatient("slot")).unwrap(), b"three");
}

/// The same for the VM's memory: reserved until the VMM is gone, so no
/// second VM is up beside one that is being stopped.
#[test]
fn an_answered_run_keeps_its_memory_until_its_vm_is_stopped() {
    let _turn = serial();
    let gate = Arc::new(Gate::default());
    let held = Arc::clone(&gate);
    let e = engine(
        Config {
            max_total_run_memory: one_vm(),
            ..Config::default()
        },
        answering(move || held.wait()),
    );
    let m = image(&e);
    assert_eq!(e.run(&m, b"one", opts("memory")).unwrap(), b"one");
    let err = e.run(&m, b"two", impatient("memory")).unwrap_err();
    assert_eq!(err.kind(), RunFailure::Waiting);
    let err = err.message();
    assert!(
        err.starts_with("waiting for ") && err.contains(" of run memory (--max-total-run-memory "),
        "{err}"
    );
    gate.open();
    e.settle();
    assert_eq!(e.run(&m, b"three", impatient("memory")).unwrap(), b"three");
}

/// What the process wrote and the VM's console are logged once the VM is
/// stopped, as they were before the response was returned: the lines with
/// their levels, the warning for output past what a run keeps, and the
/// console with the init's report of the exit, an out-of-memory kill
/// after the answer among them. Each carries the module and its digest.
#[test]
fn what_an_answered_run_left_is_logged_once_its_vm_is_stopped() {
    let _turn = serial();
    let logs = Logs::capture();
    let gate = Arc::new(Gate::default());
    let held = Arc::clone(&gate);
    let fake = FakeSandbox::new(Default::default(), |spec: &VmSpec<'_>| {
        let mut log = Output::default();
        let line = [b"filling the log ".repeat(60), b"\n".to_vec()].concat();
        let mut written = 0;
        while written < 2 * OUTPUT_CAP {
            log.push(&line);
            written += line.len();
        }
        log.push(b"a last plain line\n{\"level\":\"warn\",\"msg\":\"a typed line\",\"k\":1}\n");
        Outcome {
            log,
            end: Ended::Answered(spec.request.to_vec()),
            console: "guestfn-init: exited status=None signal=Some(9) oom=true\n".to_string(),
        }
    })
    .stopping(move || held.wait());
    let e = engine(Config::default(), fake);
    let m = image(&e);
    let mine = ["module=late lines", "digest=sha256:late lines"];
    let with = |more: &[&str]| logs.with(&[&mine[..], more].concat());

    assert_eq!(e.run(&m, b"r", opts("late lines")).unwrap(), b"r");
    assert_eq!(
        with(&[]),
        [] as [&str; 0],
        "nothing before the VM is stopped"
    );

    gate.open();
    e.settle();
    assert_eq!(with(&["INFO", "message=a last plain line"]).len(), 1);
    assert_eq!(
        with(&["WARN", "message=a typed line", "fields=k=1"]).len(),
        1
    );
    let dropped = with(&[
        "WARN",
        "bytes of the process's output: a run's last 256Ki are kept",
    ]);
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    let console = with(&["DEBUG", "message=VM console", "oom=true"]);
    assert_eq!(console.len(), 1, "{console:?}");
}

/// A run that was not answered is over when it returns, as before: its
/// fatal result is made of how the VM ended and what the process wrote
/// last, its output is logged and it is observed by then, and nothing of
/// it is left to stop.
#[test]
fn a_run_that_is_not_answered_is_over_when_it_returns() {
    let _turn = serial();
    let logs = Logs::capture();
    let stopped = Arc::new(AtomicBool::new(false));
    let cases: Vec<(&str, Ended, &str, RunFailure, &str)> = vec![
        (
            "an exit",
            Ended::Exit(Exit {
                status: Some(3),
                ..Default::default()
            }),
            "process exited with status 3 before answering the call: out of ideas",
            RunFailure::Stopped,
            "error",
        ),
        (
            "the deadline",
            Ended::Deadline,
            "run failed: module exceeded its execution deadline (30s)",
            RunFailure::Deadline,
            "timeout",
        ),
        (
            "an error from the function",
            Ended::Refused {
                code: "FailedPrecondition".into(),
                message: "not this time".into(),
            },
            "run returned an error: not this time (gRPC status FailedPrecondition)",
            RunFailure::Returned,
            "error",
        ),
    ];
    for (name, end, want, kind, outcome) in cases {
        let end = Mutex::new(Some(end));
        let saw = Arc::clone(&stopped);
        let fake = FakeSandbox::new(Default::default(), move |_: &VmSpec<'_>| Outcome {
            log: Output::from(b"warming up\nout of ideas\n".as_slice()),
            end: end.lock().unwrap().take().unwrap(),
            console: String::new(),
        })
        .stopping(move || saw.store(true, Ordering::SeqCst));
        let e = engine(
            Config {
                max_concurrent_runs: 1,
                ..Config::default()
            },
            fake,
        );
        let m = image(&e);
        let (flying, observed) = (in_flight(), runs(outcome));
        let module = format!("unanswered {name}");
        let err = e.run(&m, b"r", opts(&module)).unwrap_err();
        assert_eq!((err.kind(), err.message()), (kind, want), "{name}");
        let logged = logs.with(&[&format!("module={module}"), "message=out of ideas"]);
        assert_eq!(logged.len(), 1, "{name}: {logged:?}");
        assert_eq!(in_flight(), flying, "{name}");
        assert_eq!(runs(outcome), observed + 1.0, "{name}");
    }
    assert!(!stopped.load(Ordering::SeqCst), "nothing was left to stop");
}

/// A sandbox that panics while it stops a VM takes nothing with it: the
/// process goes on, the run gives back its slot and its memory and is
/// observed, and the log says what was lost.
#[test]
fn a_teardown_that_panics_gives_back_what_the_run_held() {
    let _turn = serial();
    let logs = Logs::capture();
    let e = engine(
        Config {
            max_concurrent_runs: 1,
            max_total_run_memory: one_vm(),
            ..Config::default()
        },
        answering(|| panic!("the fake sandbox panics as it stops the VM")),
    );
    let m = image(&e);
    let (flying, observed) = (in_flight(), runs("ok"));
    for request in [b"one", b"two"] {
        let out = e.run(&m, request, impatient("panicking")).unwrap();
        assert_eq!(out, request);
        e.settle();
    }
    assert_eq!(in_flight(), flying);
    assert_eq!(runs("ok"), observed + 2.0);
    let said = logs.with(&[
        "ERROR",
        "module=panicking",
        "digest=sha256:panicking",
        "stopping the VM of an answered run panicked",
    ]);
    assert_eq!(said.len(), 2, "{said:?}");
}

/// An engine that is dropped waits for the VMs it has not stopped yet: the
/// runtime drops its engine when it shuts down, and a process that exited
/// under a teardown would leave the VMM running.
#[test]
fn a_dropped_engine_waits_for_its_vms() {
    let _turn = serial();
    let stopped = Arc::new(AtomicBool::new(false));
    let saw = Arc::clone(&stopped);
    let e = engine(
        Config::default(),
        answering(move || {
            std::thread::sleep(Duration::from_millis(300));
            saw.store(true, Ordering::SeqCst);
        }),
    );
    let m = image(&e);
    assert_eq!(e.run(&m, b"r", opts("shutdown")).unwrap(), b"r");
    assert!(!stopped.load(Ordering::SeqCst), "the VM is still up");
    drop(e);
    assert!(stopped.load(Ordering::SeqCst), "the VM was stopped first");
}

/// A store that records what is held in it.
#[derive(Default)]
struct HoldingStore {
    held: Mutex<Vec<PathBuf>>,
}

impl ArtifactStore for HoldingStore {
    fn begin(&self, name: &str) -> Result<PathBuf, String> {
        Err(format!("no file for {name} in this test"))
    }
    fn seal(&self, name: &str) -> Result<PathBuf, String> {
        Err(format!("no file for {name} in this test"))
    }
    fn sealed(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn used(&self, _: &Path) {}
    fn hold(&self, path: &Path) {
        self.held.lock().unwrap().push(path.to_path_buf());
    }
    fn release(&self, path: &Path) {
        self.held.lock().unwrap().retain(|p| p != path);
    }
}

/// A stored root filesystem stays held in the store for as long as a VM
/// has it open, which is past the response: the image the cache lets go
/// of meanwhile is kept by the run until its VM is stopped.
#[test]
fn a_stored_image_is_held_until_its_vm_is_stopped() {
    let _turn = serial();
    let gate = Arc::new(Gate::default());
    let held = Arc::clone(&gate);
    let e = engine(Config::default(), answering(move || held.wait()));
    let store = Arc::new(HoldingStore::default());
    e.keep_in(Arc::clone(&store) as Arc<dyn ArtifactStore>);
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("sha256-digest");
    let compiled = image(&e);
    std::fs::write(&entry, e.serialize(&compiled).unwrap()).unwrap();
    let stored = e.stored(compiled, &entry);
    assert_eq!(e.run(&stored, b"r", opts("held")).unwrap(), b"r");
    // The cache lets go of the image while its VM is being stopped.
    drop(stored);
    assert_eq!(*store.held.lock().unwrap(), vec![entry.clone()]);
    gate.open();
    e.settle();
    assert!(store.held.lock().unwrap().is_empty());
}
