//! The bounds around one run - the run slot, the memory reservation and
//! the run metric - the run's description for the init, and the
//! translation of how the VM ended into the fatal results operators read
//! (the runtime prefixes them with `module <desc> failed: `, as it does
//! the Wasmtime engine's; their wording is for the reader and is no
//! contract, docs/contract/compatibility.md).

use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::concurrency::{MemPool, format_bytes};
use crate::{Error, RunOptions, duration, metrics};

use crate::sandbox::{Ended, OUTPUT_CAP, VmSpec};
use crate::{Image, Oci, Rootfs, timeline};

/// What the VM gets beyond the process's limit: the guest kernel, the
/// init, the root filesystem's overlay and the squashfs page cache. The
/// process's own cap is the step's `limits.memory`, held by a cgroup the
/// init puts it in, and the overhead must leave that cgroup room to be
/// the enforcer: at 32 MiB the guest's own use passed it, and the kernel's
/// global OOM killer ended a growing process at 82 MiB of a 96Mi limit
/// (#272, the engine suite on 2026-11-08), short of what the module was
/// promised.
pub const VM_OVERHEAD_MIB: u64 = 64;
/// The least a process is capped at: below it the kernel's own needs
/// would end every run.
pub const MIN_PROCESS_MIB: u64 = 16;
/// The most of one line of the process's output that is logged or quoted
/// in a fatal result: 16 KiB, where the container runtimes split a log
/// line themselves, so a process that writes no newline makes no record
/// larger than the pod's log would carry whole.
const MAX_LINE: usize = 16 << 10;

/// Released on drop: what a run reserved from the pool.
struct Reservation {
    pool: Arc<MemPool>,
    n: u64,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.pool.release(self.n);
    }
}

pub(crate) fn run(
    engine: &Oci,
    image: &Image,
    request: &[u8],
    opts: RunOptions,
) -> Result<Vec<u8>, Error> {
    let limits = engine.effective(&opts);
    let mut wait_deadline = Instant::now() + limits.timeout;
    if let Some(d) = opts.deadline
        && d < wait_deadline
    {
        wait_deadline = d;
    }
    let _slot = match &engine.scheduler {
        Some(s) => Some(s.acquire(&opts.digest, wait_deadline).map_err(Error)?),
        None => None,
    };
    // A VM's memory is reserved whole before it boots: the guest kernel
    // may touch all of it, so the pool counts the ceiling, not a growth.
    let process_mib = limits.memory_limit.div_ceil(1 << 20).max(MIN_PROCESS_MIB);
    let memory_mib = process_mib + VM_OVERHEAD_MIB;
    let _reservation = match &engine.mem {
        Some(pool) => {
            pool.reserve(memory_mib << 20, wait_deadline)
                .map_err(Error)?;
            Some(Reservation {
                pool: Arc::clone(pool),
                n: memory_mib << 20,
            })
        }
        None => None,
    };
    metrics::RUNS_IN_FLIGHT.inc();
    let start = Instant::now();
    timeline::mark("admitted");
    let result = boot_and_run(
        engine,
        image,
        request,
        &opts,
        limits.timeout,
        memory_mib,
        process_mib,
    );
    metrics::RUNS_IN_FLIGHT.dec();
    metrics::RUN_DURATION
        .with_label_values(&[match &result {
            Ok(_) => metrics::OUTCOME_OK,
            Err(e) if e.0.contains("exceeded its execution deadline") => metrics::OUTCOME_TIMEOUT,
            Err(_) => metrics::OUTCOME_ERROR,
        }])
        .observe(start.elapsed().as_secs_f64());
    result
}

/// The store a stored image's files live in, with the root filesystem
/// marked used: every run reads that file from the store, and each one is
/// a use. The loaded image holds the file against the sweep; the mark is
/// what orders it among the others once the image has left memory.
fn store_of<'a>(engine: &'a Oci, image: &Image) -> Option<&'a dyn crate::ArtifactStore> {
    match &image.rootfs {
        Rootfs::Stored(file) => engine.store.get().map(|store| {
            store.used(file.path());
            store.as_ref()
        }),
        Rootfs::Scratch(_) => None,
    }
}

/// What a first run would otherwise make, at warm-up: the sandbox takes
/// its snapshot of the image for the machine a step with no limits of its
/// own gets. A step that sets `limits.memory` runs another machine, and
/// its first run makes that one. The VM that is frozen reserves its
/// memory as a run's does.
pub(crate) fn prepare(engine: &Oci, image: &Image) -> Result<(), Error> {
    let opts = RunOptions::default();
    let limits = engine.effective(&opts);
    let deadline = Instant::now() + limits.timeout;
    let process_mib = limits.memory_limit.div_ceil(1 << 20).max(MIN_PROCESS_MIB);
    let memory_mib = process_mib + VM_OVERHEAD_MIB;
    let _reservation = match &engine.mem {
        Some(pool) => {
            pool.reserve(memory_mib << 20, deadline).map_err(Error)?;
            Some(Reservation {
                pool: Arc::clone(pool),
                n: memory_mib << 20,
            })
        }
        None => None,
    };
    let description = image.config.describe(&opts);
    engine.sandbox.prepare(VmSpec {
        kernel: &engine.options.kernel,
        initrd: engine.initrd.as_ref(),
        rootfs: image.rootfs.path(),
        memory_mib,
        vcpus: engine.options.vcpus,
        description: &description,
        request: &[],
        deadline,
        scratch: engine.scratch.path(),
        snapshots: &image.snapshots,
        store: store_of(engine, image),
    })
}

fn boot_and_run(
    engine: &Oci,
    image: &Image,
    request: &[u8],
    opts: &RunOptions,
    budget: Duration,
    memory_mib: u64,
    process_mib: u64,
) -> Result<Vec<u8>, Error> {
    // The budget covers the boot: a VM that takes the whole budget to boot
    // never ran, and the deadline says so.
    let mut deadline = Instant::now() + budget;
    if let Some(d) = opts.deadline
        && d < deadline
    {
        deadline = d;
    }
    let mut description = image.config.describe(opts);
    description.memory_max = process_mib << 20;
    let outcome = engine.sandbox.run(VmSpec {
        kernel: &engine.options.kernel,
        initrd: engine.initrd.as_ref(),
        rootfs: image.rootfs.path(),
        memory_mib,
        vcpus: engine.options.vcpus,
        description: &description,
        request,
        deadline,
        scratch: engine.scratch.path(),
        snapshots: &image.snapshots,
        store: store_of(engine, image),
    })?;
    timeline::mark("sandbox-returned");
    let (output, dropped) = outcome.log.into_text();
    if dropped > 0 {
        tracing::warn!(
            module = %opts.module,
            digest = %opts.digest,
            "dropped the first {dropped} bytes of the process's output: a run's last {} are kept",
            format_bytes(OUTPUT_CAP as u64)
        );
    }
    log_lines(&output, opts);
    if !outcome.console.is_empty() {
        tracing::debug!(module = %opts.module, digest = %opts.digest, console = %outcome.console, "VM console");
    }
    let last_line = last_line(&output);
    timeline::mark("output-logged");
    match outcome.end {
        Ended::Deadline => Err(Error(format!(
            "run failed: module exceeded its execution deadline ({})",
            duration::format(budget)
        ))),
        Ended::Lost(why) => Err(Error(format!("run failed: {why}"))),
        Ended::Answered(response) => Ok(response),
        Ended::Refused { code, message } => Err(Error(format!(
            "run returned an error: {message} (gRPC status {code})"
        ))),
        Ended::Exit(exit) => {
            if !exit.error.is_empty() {
                return Err(Error(format!("cannot start the process: {}", exit.error)));
            }
            if exit.oom {
                return Err(Error(format!(
                    "process was killed: the run's memory limit of {} was exceeded",
                    format_bytes(process_mib << 20)
                )));
            }
            if let Some(signal) = exit.signal {
                return Err(Error(with_last(
                    format!("process was killed by signal {signal}"),
                    &last_line,
                )));
            }
            match exit.status {
                Some(n) => Err(Error(with_last(
                    format!("process exited with status {n} before answering the call"),
                    &last_line,
                ))),
                None => Err(Error("process ended without an exit status".to_string())),
            }
        }
    }
}

fn with_last(head: String, last: &str) -> String {
    if last.is_empty() {
        head
    } else {
        format!("{head}: {last}")
    }
}

fn last_line(output: &str) -> String {
    let line = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    bounded(line).into_owned()
}

/// A line no longer than MAX_LINE: a longer one is cut there, on a
/// character's boundary, and says how much of it is missing.
fn bounded(line: &str) -> Cow<'_, str> {
    if line.len() <= MAX_LINE {
        return Cow::Borrowed(line);
    }
    let mut cut = MAX_LINE;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    Cow::Owned(format!(
        "{} [truncated: {} more bytes]",
        &line[..cut],
        line.len() - cut
    ))
}

/// The process's output as the runtime's log: a line that parses as a
/// JSON object with `level` and `msg` is a typed line with its other keys
/// as fields, any other line is info, each with the module and its digest
/// attached as the typed `log` import's lines are. A line, a message or
/// the fields of one longer than MAX_LINE are cut to it.
fn log_lines(output: &str, opts: &RunOptions) {
    for line in output.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(line)
            && let (Some(level), Some(msg)) = (
                map.get("level").and_then(|v| v.as_str()),
                map.get("msg").and_then(|v| v.as_str()),
            )
        {
            let fields: Vec<String> = map
                .iter()
                .filter(|(k, _)| k.as_str() != "level" && k.as_str() != "msg")
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            let fields = fields.join(" ");
            let (msg, fields) = (bounded(msg), bounded(&fields));
            match level.to_ascii_lowercase().as_str() {
                "debug" | "trace" => {
                    tracing::debug!(module = %opts.module, digest = %opts.digest, fields = %fields, "{msg}")
                }
                "warn" | "warning" => {
                    tracing::warn!(module = %opts.module, digest = %opts.digest, fields = %fields, "{msg}")
                }
                "error" | "fatal" => {
                    tracing::error!(module = %opts.module, digest = %opts.digest, fields = %fields, "{msg}")
                }
                _ => {
                    tracing::info!(module = %opts.module, digest = %opts.digest, fields = %fields, "{msg}")
                }
            }
            continue;
        }
        let line = bounded(line);
        tracing::info!(module = %opts.module, digest = %opts.digest, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_line_is_the_last_non_empty_one() {
        assert_eq!(last_line("a\nb\n\n  \n"), "b");
        assert_eq!(last_line(""), "");
    }

    #[test]
    fn a_long_line_is_cut_and_says_so() {
        let cases: &[(&str, String, String)] = &[
            ("a short line", "short".to_string(), "short".to_string()),
            (
                "exactly the bound",
                "a".repeat(MAX_LINE),
                "a".repeat(MAX_LINE),
            ),
            (
                "one byte over",
                "a".repeat(MAX_LINE + 1),
                format!("{} [truncated: 1 more bytes]", "a".repeat(MAX_LINE)),
            ),
            (
                "the cut inside a character",
                format!("a{}", "\u{e9}".repeat(MAX_LINE)),
                format!(
                    "a{} [truncated: {} more bytes]",
                    "\u{e9}".repeat(MAX_LINE / 2 - 1),
                    MAX_LINE + 2
                ),
            ),
        ];
        for (name, line, want) in cases {
            assert_eq!(bounded(line), want.as_str(), "{name}");
        }
        let long = format!("first\n{}\n", "z".repeat(3 * MAX_LINE));
        assert_eq!(
            last_line(&long),
            format!(
                "{} [truncated: {} more bytes]",
                "z".repeat(MAX_LINE),
                2 * MAX_LINE
            )
        );
    }
}
