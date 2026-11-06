//! The bounds around one run - the run slot, the memory reservation and
//! the run metric - the run's description for the init, and the
//! translation of how the VM ended into the fatal results operators read
//! (their wording is contract: the runtime prefixes them with `module
//! <desc> failed: `, as it does the Wasmtime engine's).

use std::sync::Arc;
use std::time::{Duration, Instant};

use function_wasm_engine_api::concurrency::{MemPool, format_bytes};
use function_wasm_engine_api::{Error, RunOptions, duration, metrics};

use crate::sandbox::{Ended, VmSpec};
use crate::{Image, Oci};

/// The least memory a VM gets: the kernel and the init need it before
/// the process runs, whatever the step's limit.
pub const MIN_VM_MIB: u64 = 64;

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
    let memory_mib = limits.memory_limit.div_ceil(1 << 20).max(MIN_VM_MIB);
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
    let result = boot_and_run(engine, image, request, &opts, limits.timeout, memory_mib);
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

fn boot_and_run(
    engine: &Oci,
    image: &Image,
    request: &[u8],
    opts: &RunOptions,
    budget: Duration,
    memory_mib: u64,
) -> Result<Vec<u8>, Error> {
    // The budget covers the boot: a VM that takes the whole budget to boot
    // never ran, and the deadline says so.
    let mut deadline = Instant::now() + budget;
    if let Some(d) = opts.deadline
        && d < deadline
    {
        deadline = d;
    }
    let description = image.config.describe(opts);
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
    })?;
    log_stderr(&outcome.stderr, opts);
    if !outcome.console.is_empty() {
        tracing::debug!(module = %opts.module, digest = %opts.digest, console = %outcome.console, "VM console");
    }
    let last_line = last_line(&outcome.stderr);
    match outcome.end {
        Ended::Deadline => Err(Error(format!(
            "run failed: module exceeded its execution deadline ({})",
            duration::format(budget)
        ))),
        Ended::Lost(why) => Err(Error(format!("run failed: {why}"))),
        Ended::Exit(exit) => {
            if !exit.error.is_empty() {
                return Err(Error(format!("cannot start the process: {}", exit.error)));
            }
            if exit.oom {
                return Err(Error(format!(
                    "process was killed: the run's memory limit of {} was exceeded",
                    format_bytes(memory_mib << 20)
                )));
            }
            if let Some(signal) = exit.signal {
                return Err(Error(with_last(
                    format!("process was killed by signal {signal}"),
                    &last_line,
                )));
            }
            match exit.status {
                Some(0) => {}
                Some(n) => {
                    return Err(Error(with_last(
                        format!("process exited with status {n}"),
                        &last_line,
                    )));
                }
                None => return Err(Error("process ended without an exit status".to_string())),
            }
            if outcome.stdout.is_empty() {
                return Err(Error(
                    "process exited with status 0 and wrote no response to its standard output"
                        .to_string(),
                ));
            }
            Ok(outcome.stdout)
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

fn last_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// The process's stderr as the runtime's log: a line that parses as a
/// JSON object with `level` and `msg` is a typed line with its other keys
/// as fields, any other line is info, each with the module and its digest
/// attached as the typed `log` import's lines are.
fn log_stderr(stderr: &[u8], opts: &RunOptions) {
    for line in String::from_utf8_lossy(stderr).lines() {
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
        tracing::info!(module = %opts.module, digest = %opts.digest, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_line_is_the_last_non_empty_one() {
        assert_eq!(last_line(b"a\nb\n\n  \n"), "b");
        assert_eq!(last_line(b""), "");
    }
}
