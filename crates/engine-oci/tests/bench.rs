//! Where a run's time goes, by hand: the engine over real Firecracker VMs,
//! as tests/vm.rs runs it, measured. Built only with the `bench` feature
//! and never run by `cargo test` unasked:
//!
//!   cargo test -p guestfn-engine-oci --release --features bench --test bench -- \
//!     --ignored --nocapture --test-threads=1
//!
//! with the VM suite's GUESTFN_FIRECRACKER, GUESTFN_KERNEL, GUESTFN_INIT
//! and GUESTFN_FIXTURE set (`make oci.tools` and `make oci.init`). Every
//! table is Markdown on standard output.
//!
//! `a_run_by_phase`, `a_run_by_memory` and `a_run_by_root_filesystem_packing`
//! are the three that run VMs; name one to run it alone. `packing_by_level`
//! needs no VM: it times the root filesystem's writing at each level, over
//! the binaries GUESTFN_FIXTURE and GUESTFN_BENCH_BINARY name.
//!
//!   GUESTFN_BENCH_RUNS     measured runs per row (30)
//!   GUESTFN_BENCH_ROUNDS   timed writes per row of `packing_by_level` (3)
//!   GUESTFN_BENCH_PAD_MIB  the size of the file the padded image's process
//!                          reads before it serves (32)
//!   GUESTFN_BENCH_BINARY   another function's static binary, measured
//!                          beside the fixture: started as `<it> --insecure`
//!   GUESTFN_BENCH_LABEL    a word for the tables' headings

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Instant;

use function_sdk_rust::proto::v1::{
    RequestMeta, Resource, RunFunctionRequest, RunFunctionResponse, State,
};
use guestfn_engine::{Config, Engine, RunOptions};
use guestfn_engine_oci::rootfs::ZSTD_LEVEL;
use guestfn_engine_oci::testing::{fixture_image, fixture_image_with, tar_layer};
use guestfn_engine_oci::timeline::{self, Mark};
use guestfn_engine_oci::{Module, Oci, Options};
use prost::Message as _;

struct Tools {
    firecracker: PathBuf,
    kernel: PathBuf,
    init: PathBuf,
    fixture: Vec<u8>,
}

/// The tools, or None (a skip, said aloud) when the environment names
/// none and does not insist.
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
            println!(
                "SKIPPED: the VM tools are not named (GUESTFN_FIRECRACKER, GUESTFN_KERNEL, GUESTFN_INIT, GUESTFN_FIXTURE)"
            );
            None
        }
    }
}

fn number(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn label() -> String {
    std::env::var("GUESTFN_BENCH_LABEL")
        .map(|l| format!(" ({l})"))
        .unwrap_or_default()
}

fn engine(t: &Tools, snapshots: bool) -> Oci {
    Oci::new(
        Config::default(),
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
        module: "oci bench".to_string(),
        digest: "sha256:bench".to_string(),
        ..Default::default()
    }
}

/// The two ways a run gets its VM, and whether the engine snapshots.
const PATHS: [(&str, bool); 2] = [("boot", false), ("restore", true)];

const TAG: &str = "bench";

/// A request as a composition step sends one: a composite resource and
/// eight composed ones, observed and desired, a few kilobytes in all.
fn request() -> Vec<u8> {
    let object = |kind: &str, name: &str| {
        let json = serde_json::json!({
            "apiVersion": "bench.example.org/v1alpha1",
            "kind": kind,
            "metadata": {
                "name": name,
                "namespace": "team-a",
                "labels": {
                    "crossplane.io/composite": "bench-abc12",
                    "app.kubernetes.io/managed-by": "crossplane",
                },
                "annotations": {
                    "crossplane.io/composition-resource-name": name,
                    "crossplane.io/external-name": format!("{name}-7f3a9c"),
                },
            },
            "spec": {
                "forProvider": {
                    "region": "eu-central-1",
                    "tags": {"team": "a", "cost-center": "1234", "environment": "production"},
                    "versioning": [{"enabled": true}],
                },
                "providerConfigRef": {"name": "default"},
            },
            "status": {
                "atProvider": {"arn": format!("arn:aws:s3:::{name}-7f3a9c"), "id": name},
                "conditions": [
                    {"type": "Ready", "status": "True", "reason": "Available"},
                    {"type": "Synced", "status": "True", "reason": "ReconcileSuccess"},
                ],
            },
        });
        Resource {
            resource: Some(function_sdk_rust::resource::json_to_struct(
                json.as_object().expect("an object"),
            )),
            ..Default::default()
        }
    };
    let state = || State {
        composite: Some(object("XBucket", "bench-abc12")),
        resources: (0..8)
            .map(|i| {
                let name = format!("bucket-{i}");
                (name.clone(), object("Bucket", &name))
            })
            .collect(),
    };
    RunFunctionRequest {
        meta: Some(RequestMeta {
            tag: TAG.to_string(),
            ..Default::default()
        }),
        observed: Some(state()),
        desired: Some(state()),
        ..Default::default()
    }
    .encode_to_vec()
}

/// One measured run: when it started, the marks it passed, when the
/// caller had its answer and when the run's VM was gone. The caller of an
/// answered run does not wait for the last: until the engine returned a
/// response only once the VM was stopped, it was what the caller waited
/// for, so the two give the wait before and after in one run.
struct Run {
    started: Instant,
    marks: Vec<Mark>,
    resolved: Instant,
    gone: Instant,
}

impl Run {
    /// What the caller waited.
    fn total_ms(&self) -> f64 {
        ms(self.started, self.resolved)
    }

    /// The run to the end of its VM.
    fn gone_ms(&self) -> f64 {
        ms(self.started, self.gone)
    }

    fn at(&self, name: &str) -> Option<Instant> {
        self.marks.iter().find(|m| m.name == name).map(|m| m.at)
    }

    /// The milliseconds between two marks, `resolved` being the caller's
    /// and `gone` the end of the VM.
    fn between(&self, from: &str, to: &str) -> Option<f64> {
        let to = match to {
            "resolved" => self.resolved,
            "gone" => self.gone,
            name => self.at(name)?,
        };
        Some(ms(self.at(from)?, to))
    }
}

fn ms(from: Instant, to: Instant) -> f64 {
    to.saturating_duration_since(from).as_secs_f64() * 1e3
}

/// Runs the module once with its timeline kept; the answer must echo the
/// request's tag.
fn timed(e: &Oci, m: &Module, request: &[u8], opts: RunOptions) -> Result<Run, String> {
    timeline::start();
    let started = Instant::now();
    let result = e.run(m, request, opts);
    let resolved = Instant::now();
    // The VM is stopped on a thread of its own, which marks this timeline
    // too: the next run starts once it is gone, as the runs before it did.
    e.settle();
    let gone = Instant::now();
    let marks = timeline::take();
    let response = result.map_err(|e| e.to_string())?;
    let tag = RunFunctionResponse::decode(response.as_slice())
        .map_err(|e| format!("cannot decode the response: {e}"))?
        .meta
        .map(|m| m.tag)
        .unwrap_or_default();
    if tag != TAG {
        return Err(format!("the response's tag is {tag:?}, not {TAG:?}"));
    }
    Ok(Run {
        started,
        marks,
        resolved,
        gone,
    })
}

/// The median, the ninth decile and the largest of some milliseconds.
#[derive(Clone, Copy)]
struct Spread {
    p50: f64,
    p90: f64,
    max: f64,
}

impl Spread {
    fn of(mut values: Vec<f64>) -> Spread {
        assert!(!values.is_empty(), "nothing was measured");
        values.sort_by(|a, b| a.partial_cmp(b).expect("a number"));
        let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
        Spread {
            p50: at(0.5),
            p90: at(0.9),
            max: at(1.0),
        }
    }

    fn cells(&self) -> String {
        format!("{:.1} | {:.1} | {:.1}", self.p50, self.p90, self.max)
    }
}

/// `runs` measured runs after one that is not: the first run of a module
/// makes its snapshot where the engine snapshots, and reads the root
/// filesystem into the host's page cache either way.
fn measure(e: &Oci, m: &Module, request: &[u8], runs: usize) -> Result<Vec<Run>, String> {
    e.warm(m).map_err(|e| e.0)?;
    measure_with(e, m, request, runs, opts)
}

/// `measure` with every run's options from `opts`, and no warm-up: the
/// run that is not measured makes the snapshot of the machine they name.
fn measure_with(
    e: &Oci,
    m: &Module,
    request: &[u8],
    runs: usize,
    opts: impl Fn() -> RunOptions,
) -> Result<Vec<Run>, String> {
    timed(e, m, request, opts())?;
    (0..runs).map(|_| timed(e, m, request, opts())).collect()
}

/// A run's timeline, cold and restored: every mark the engine and the
/// sandbox make, as the time since the mark before it and since the start,
/// then what follows the response's bytes being in hand: how long the
/// caller still waits, and how long the VM takes to go, which the caller
/// waited for too before a response was returned ahead of it.
#[test]
#[ignore = "a hand-run benchmark"]
fn a_run_by_phase() {
    let Some(t) = tools() else { return };
    let runs = number("GUESTFN_BENCH_RUNS", 30);
    let request = request();
    let image = fixture_image(&t.fixture, &["/fixture", "--insecure", "echo"], "app", &[]);
    for (path, snapshots) in PATHS {
        let e = engine(&t, snapshots);
        let m = e.compile(&image).expect("the fixture image");
        let measured = measure(&e, &m, &request, runs).expect("a run");
        let names: Vec<&str> = measured[0].marks.iter().map(|m| m.name).collect();
        for run in &measured {
            let these: Vec<&str> = run.marks.iter().map(|m| m.name).collect();
            assert_eq!(these, names, "every run passes the same marks");
        }
        println!(
            "\n### A run by phase: {path}{}\n\n{runs} runs, a request of {} bytes, milliseconds.\n",
            label(),
            request.len()
        );
        println!(
            "| reached | since the mark before: p50 | p90 | max | since the start: p50 | looks: p50 | max |"
        );
        println!("|---|---|---|---|---|---|---|");
        let row = |name: &str, step: Vec<f64>, since: Vec<f64>, looks: Vec<f64>| {
            let looks = if looks.iter().all(|l| *l == 0.0) {
                " | ".to_string()
            } else {
                let looks = Spread::of(looks);
                format!("{:.0} | {:.0}", looks.p50, looks.max)
            };
            println!(
                "| {name} | {} | {:.1} | {looks} |",
                Spread::of(step).cells(),
                Spread::of(since).p50,
            );
        };
        for (i, name) in names.iter().enumerate() {
            let before = |run: &Run| match i {
                0 => run.started,
                _ => run.marks[i - 1].at,
            };
            row(
                name,
                measured
                    .iter()
                    .map(|r| ms(before(r), r.marks[i].at))
                    .collect(),
                measured
                    .iter()
                    .map(|r| ms(r.started, r.marks[i].at))
                    .collect(),
                measured.iter().map(|r| r.marks[i].count as f64).collect(),
            );
        }
        println!(
            "\nThe caller has the response at {:.1} (p50), and the VM is gone at {:.1}.",
            Spread::of(measured.iter().map(Run::total_ms).collect()).p50,
            Spread::of(measured.iter().map(Run::gone_ms).collect()).p50,
        );
        println!("\n| span | p50 | p90 | max | share of the run: p50 |");
        println!("|---|---|---|---|---|");
        for (what, from, to) in [
            (
                "the response in hand until the caller has it",
                "response-received",
                "resolved",
            ),
            (
                "the response in hand until the VM is gone (the caller's wait before)",
                "response-received",
                "gone",
            ),
            (
                "of it: the Finish and the init's Exit",
                "response-received",
                "exit-read",
            ),
            (
                "of it: the VMM killed and reaped",
                "exit-read",
                "vmm-reaped",
            ),
            (
                "of it: the console drained and the output logged",
                "vmm-reaped",
                "output-logged",
            ),
            (
                "the run sent until the function serves",
                "run-sent",
                "function-serving",
            ),
            (
                "the function serving until its response",
                "function-serving",
                "response-received",
            ),
        ] {
            let spans: Vec<f64> = measured
                .iter()
                .map(|r| r.between(from, to).expect("both marks"))
                .collect();
            let shares: Vec<f64> = measured
                .iter()
                .zip(&spans)
                .map(|(r, span)| 100.0 * span / r.total_ms())
                .collect();
            println!(
                "| {what} | {} | {:.1} % |",
                Spread::of(spans).cells(),
                Spread::of(shares).p50
            );
        }
        println!(
            "| the whole run for its caller | {} | 100 % |",
            Spread::of(measured.iter().map(Run::total_ms).collect()).cells()
        );
        println!(
            "| the whole run until the VM is gone (the caller's before) | {} | |",
            Spread::of(measured.iter().map(Run::gone_ms).collect()).cells()
        );
    }
}

/// The VM's memory against a run's latency, cold and restored: the step's
/// `limits.memory` sizes the VM, and the whole of it is what the VMM maps
/// and gives back when it is stopped.
#[test]
#[ignore = "a hand-run benchmark"]
fn a_run_by_memory() {
    let Some(t) = tools() else { return };
    let runs = number("GUESTFN_BENCH_RUNS", 30);
    let request = request();
    let image = fixture_image(&t.fixture, &["/fixture", "--insecure", "echo"], "app", &[]);
    println!(
        "\n### A run by memory{}\n\n{runs} runs per row, milliseconds, p50; the VM has the limit and 64 MiB more.\n",
        label()
    );
    println!(
        "| path | limits.memory MiB | the whole run for its caller | p90 | until the guest connects | the run sent until the function serves | the VMM killed and reaped | the response in hand until the caller has it | until the VM is gone |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    for (path, snapshots) in PATHS {
        let e = engine(&t, snapshots);
        let m = e.compile(&image).expect("the fixture image");
        for limit in [64u64, 256, 512] {
            let measured = measure_with(&e, &m, &request, runs, || RunOptions {
                memory_limit: Some(limit << 20),
                ..opts()
            })
            .expect("a run");
            let p50 = |from: &str, to: &str| {
                Spread::of(
                    measured
                        .iter()
                        .map(|r| r.between(from, to).expect("both marks"))
                        .collect(),
                )
                .p50
            };
            let total = Spread::of(measured.iter().map(Run::total_ms).collect());
            println!(
                "| {path} | {limit} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
                total.p50,
                total.p90,
                p50("vmm-spawned", "guest-connected"),
                p50("run-sent", "function-serving"),
                p50("exit-read", "vmm-reaped"),
                p50("response-received", "resolved"),
                p50("response-received", "gone"),
            );
        }
    }
}

/// How a root filesystem's blocks are packed.
#[derive(Clone, Copy)]
enum Packing {
    /// As the engine writes it: zstd at `rootfs::ZSTD_LEVEL`.
    Engine,
    /// As the engine wrote it before: zlib's default level.
    Gzip,
    /// zstd at a level.
    Zstd(u32),
    Lz4,
    /// No block compressed.
    Stored,
}

impl Packing {
    /// The packings a run is measured over.
    const ALL: [Packing; 6] = [
        Packing::Gzip,
        Packing::Engine,
        Packing::Zstd(1),
        Packing::Zstd(19),
        Packing::Lz4,
        Packing::Stored,
    ];

    /// The packings whose writing is timed.
    const LEVELS: [Packing; 9] = [
        Packing::Gzip,
        Packing::Zstd(1),
        Packing::Zstd(3),
        Packing::Zstd(6),
        Packing::Zstd(9),
        Packing::Zstd(12),
        Packing::Zstd(15),
        Packing::Zstd(19),
        Packing::Lz4,
    ];

    fn name(self) -> String {
        match self {
            Packing::Engine => format!("zstd {ZSTD_LEVEL} (the engine's)"),
            Packing::Gzip => "gzip (the engine's before)".to_string(),
            Packing::Zstd(level) => format!("zstd {level}"),
            Packing::Lz4 => "lz4".to_string(),
            Packing::Stored => "stored".to_string(),
        }
    }
}

/// The engine's squashfs written again with another packing, every node as
/// it was.
fn repack(squashfs: &[u8], packing: Packing) -> Vec<u8> {
    use backhand::compression::{CompressionOptions, Compressor, Gzip, Lz4, Zstd};
    use backhand::{FilesystemCompressor, FilesystemReader, FilesystemWriter};
    let (compressor, options) = match packing {
        Packing::Engine => return squashfs.to_vec(),
        Packing::Gzip => (Compressor::Gzip, None),
        Packing::Zstd(compression_level) => (
            Compressor::Zstd,
            Some(CompressionOptions::Zstd(Zstd { compression_level })),
        ),
        // The kernel reads an lz4 squashfs only with its options present,
        // and only the legacy format, version 1.
        Packing::Lz4 => (
            Compressor::Lz4,
            Some(CompressionOptions::Lz4(Lz4 {
                version: 1,
                flags: 0,
            })),
        ),
        // A squashfs names a compressor whatever it holds: level 0 of
        // zlib makes every block larger than it was, and a block that did
        // not shrink is stored as it is.
        Packing::Stored => (
            Compressor::Gzip,
            Some(CompressionOptions::Gzip(Gzip {
                compression_level: 0,
                window_size: 15,
                strategies: 0,
            })),
        ),
    };
    let reader = FilesystemReader::from_reader(Cursor::new(squashfs)).expect("the squashfs");
    let mut writer = FilesystemWriter::from_fs_reader(&reader).expect("a writer");
    writer.set_compressor(FilesystemCompressor::new(compressor, options).expect("a compressor"));
    // As the engine writes it, with no options block; the stored packing's
    // would name a level the kernel refuses.
    if matches!(packing, Packing::Stored | Packing::Zstd(_)) {
        writer.set_emit_compression_options(false);
    }
    let mut out = Cursor::new(Vec::new());
    writer.write(&mut out).expect("the repacked squashfs");
    out.into_inner()
}

/// A file of `mib` MiB that compresses as a program does: the fixture's
/// own bytes, over and over.
fn padding(program: &[u8], mib: usize) -> Vec<u8> {
    program.iter().copied().cycle().take(mib << 20).collect()
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1u64 << 20) as f64
}

/// The root filesystem's packing against a run's latency, cold and
/// restored: the fixture alone, the fixture reading a padding file before
/// it serves (a larger program paging itself in), and the binary
/// GUESTFN_BENCH_BINARY names. Every packing is the engine's own squashfs
/// written again, loaded as a stored root filesystem is.
#[test]
#[ignore = "a hand-run benchmark"]
fn a_run_by_root_filesystem_packing() {
    let Some(t) = tools() else { return };
    let runs = number("GUESTFN_BENCH_RUNS", 30);
    let pad_mib = number("GUESTFN_BENCH_PAD_MIB", 32);
    let request = request();
    let pad = tar_layer(&[("pad.bin", Some(&padding(&t.fixture, pad_mib)), 0o644)]);
    let mut images: Vec<(String, usize, Vec<u8>)> = vec![
        (
            "the fixture".to_string(),
            t.fixture.len(),
            fixture_image(&t.fixture, &["/fixture", "--insecure", "echo"], "app", &[]),
        ),
        (
            format!("the fixture reading {pad_mib} MiB first"),
            t.fixture.len(),
            fixture_image_with(
                &t.fixture,
                &["/fixture", "--insecure", "preload", "/pad.bin"],
                "app",
                &[],
                &[&pad],
            ),
        ),
    ];
    if let Some(path) = std::env::var_os("GUESTFN_BENCH_BINARY") {
        let binary = std::fs::read(&path).expect("GUESTFN_BENCH_BINARY");
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        images.push((
            name,
            binary.len(),
            fixture_image(&binary, &["/fixture", "--insecure"], "app", &[]),
        ));
    }
    let engines: Vec<(&str, Oci)> = PATHS
        .iter()
        .map(|(path, snapshots)| (*path, engine(&t, *snapshots)))
        .collect();
    let dir = tempfile::tempdir().expect("tempdir");
    println!(
        "\n### A run by root filesystem packing{}\n\n{runs} runs per cell, milliseconds, what the caller waited; `serving` is the run sent until the function serves, `VM gone` the run until its VM is stopped (the caller's wait before a response was returned ahead of it).\n",
        label()
    );
    println!(
        "| image | program MiB | packing | squashfs MiB | boot: p50 | p90 | max | serving p50 | VM gone p50 | restore: p50 | p90 | max | serving p50 | VM gone p50 |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for (n, (image, program, layout)) in images.iter().enumerate() {
        let written = engines[0].1.compile(layout).expect("the image");
        let squashfs = engines[0].1.serialize(&written).expect("its squashfs");
        for (p, packing) in Packing::ALL.into_iter().enumerate() {
            let packed = repack(&squashfs, packing);
            let entry = dir.path().join(format!("sha256-bench-{n}-{p}"));
            std::fs::write(&entry, &packed).expect("the root filesystem");
            let mut cells = BTreeMap::new();
            for (path, e) in &engines {
                let cell = e
                    .deserialize_file(&entry)
                    .map_err(|e| e.0)
                    .and_then(|m| measure(e, &m, &request, runs))
                    .map(|measured| {
                        let serving = measured
                            .iter()
                            .map(|r| r.between("run-sent", "function-serving").expect("marks"))
                            .collect();
                        format!(
                            "{} | {:.1} | {:.1}",
                            Spread::of(measured.iter().map(Run::total_ms).collect()).cells(),
                            Spread::of(serving).p50,
                            Spread::of(measured.iter().map(Run::gone_ms).collect()).p50
                        )
                    })
                    .unwrap_or_else(|e| format!("failed: {e} | | | | "));
                cells.insert(*path, cell);
            }
            println!(
                "| {image} | {:.1} | {} | {:.2} | {} | {} |",
                mib(*program),
                packing.name(),
                mib(packed.len()),
                cells["boot"],
                cells["restore"],
            );
            let _ = std::fs::remove_file(&entry);
        }
    }
}

/// What a packing costs where it is paid, at an image's first load, and
/// what it leaves on disk: the engine's squashfs of each image written
/// again at every level, timed. Runs without a VM, over the binaries
/// GUESTFN_FIXTURE and GUESTFN_BENCH_BINARY name. The time is the whole
/// rewrite, reading the engine's squashfs included, which is the same for
/// every row of an image; the engine's own compile, layers unpacked and
/// all, is the row above them.
#[test]
#[ignore = "a hand-run benchmark"]
fn packing_by_level() {
    use guestfn_engine_oci::sandbox::VmSpec;
    use guestfn_engine_oci::testing::FakeSandbox;
    let binaries: Vec<PathBuf> = ["GUESTFN_FIXTURE", "GUESTFN_BENCH_BINARY"]
        .iter()
        .filter_map(|name| std::env::var_os(name).map(PathBuf::from))
        .collect();
    if binaries.is_empty() {
        println!("SKIPPED: neither GUESTFN_FIXTURE nor GUESTFN_BENCH_BINARY names a binary");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let init = dir.path().join("init");
    std::fs::write(&init, b"").expect("init");
    let e = Oci::with_sandbox(
        Config::default(),
        Options {
            vmm: dir.path().join("firecracker"),
            kernel: dir.path().join("vmlinux"),
            init,
            vcpus: 1,
            snapshots: false,
        },
        Box::new(FakeSandbox::new(Default::default(), |_: &VmSpec<'_>| {
            unreachable!("nothing runs")
        })),
    )
    .expect("engine");
    let rounds = number("GUESTFN_BENCH_ROUNDS", 3);
    let timed = |work: &dyn Fn() -> Vec<u8>| {
        let mut size = 0;
        let spent: Vec<f64> = (0..rounds)
            .map(|_| {
                let started = Instant::now();
                size = work().len();
                ms(started, Instant::now())
            })
            .collect();
        (Spread::of(spent).p50, size)
    };
    println!(
        "\n### Packing by level{}\n\nThe median of {rounds} writes, milliseconds, one thread.\n",
        label()
    );
    println!("| image | program MiB | packing | squashfs MiB | written in ms |");
    println!("|---|---|---|---|---|");
    for path in binaries {
        let binary = std::fs::read(&path).expect("the binary");
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let layout = fixture_image(&binary, &["/fixture", "--insecure"], "app", &[]);
        let row = |packing: &str, (spent, size): (f64, usize)| {
            println!(
                "| {name} | {:.1} | {packing} | {:.2} | {spent:.0} |",
                mib(binary.len()),
                mib(size),
            );
        };
        row(
            &format!("the engine's compile, zstd {ZSTD_LEVEL}"),
            timed(&|| {
                let m = e.compile(&layout).expect("the image");
                e.serialize(&m).expect("its squashfs")
            }),
        );
        let squashfs = e
            .serialize(&e.compile(&layout).expect("the image"))
            .expect("its squashfs");
        for packing in Packing::LEVELS {
            row(&packing.name(), timed(&|| repack(&squashfs, packing)));
        }
    }
}

/// Every packing holds the files the engine's squashfs held, and the
/// stored one compresses nothing. Runs without a VM.
#[test]
fn every_packing_keeps_the_files() {
    use backhand::compression::Compressor;
    use backhand::{
        FilesystemCompressor, FilesystemReader, FilesystemWriter, InnerNode, NodeHeader,
    };
    let program: Vec<u8> = (0..600_000u32)
        .flat_map(|i| (i / 7).to_le_bytes())
        .collect();
    let mut fs = FilesystemWriter::default();
    fs.set_compressor(FilesystemCompressor::new(Compressor::Gzip, None).expect("gzip"));
    fs.push_file(
        Cursor::new(&program[..]),
        "/fixture",
        NodeHeader::new(0o755, 0, 0, 0),
    )
    .expect("a file");
    let mut written = Cursor::new(Vec::new());
    fs.write(&mut written).expect("a squashfs");
    let written = written.into_inner();
    for packing in Packing::ALL {
        let packed = repack(&written, packing);
        let reader = FilesystemReader::from_reader(Cursor::new(&packed[..])).expect("a squashfs");
        let read: Vec<Vec<u8>> = reader
            .files()
            .filter_map(|node| match &node.inner {
                InnerNode::File(file) => {
                    let mut data = Vec::new();
                    std::io::Read::read_to_end(&mut reader.file(file).reader(), &mut data)
                        .expect("the file");
                    Some(data)
                }
                _ => None,
            })
            .collect();
        assert_eq!(read, std::slice::from_ref(&program), "{}", packing.name());
        if matches!(packing, Packing::Stored) {
            assert!(packed.len() > program.len(), "nothing is compressed");
        } else {
            assert!(packed.len() < program.len(), "{}", packing.name());
        }
    }
}
