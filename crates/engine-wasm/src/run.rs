//! The bounds around one run - the run slot, the memory reservation and
//! the run metric - and the translation of wasmtime failures into the
//! messages operators read (their wording is contract). The run itself (store, WASI, deadline,
//! the world's export) is component::execute.

use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmtime::Trap;
use wasmtime_wasi::I32Exit;

use crate::{EPOCH_TICK, Module, RunError, RunFailure, RunOptions, Wasmtime, first_line};

pub(crate) fn run(
    engine: &Wasmtime,
    m: &Module,
    request: &[u8],
    opts: RunOptions,
) -> Result<Vec<u8>, RunError> {
    let limits = engine.config().narrowed_by(&opts);
    // A run slot (round-robin by module key when --max-concurrent-runs
    // bounds them) and the memory reservation come first, waited for under
    // the run's own budget - or the request's deadline when that is
    // sooner: a wait cut short held and consumed nothing.
    let wait_deadline = opts.deadline_within(limits.timeout);
    let _slot = match &engine.scheduler {
        Some(s) => Some(
            s.acquire(&opts.digest, wait_deadline)
                .map_err(RunError::waiting)?,
        ),
        None => None,
    };
    // A component exports no top-level memory, so nothing is reserved
    // before the run: every growth, the initial memory included, is reserved
    // as the guest claims it (RunLimiter), so a run's pool footprint is what
    // it actually uses, not the worst-case ceiling.
    let hold = engine
        .mem
        .as_ref()
        .map(|pool| crate::PoolHold::new(Arc::clone(pool), 0));

    // The run is timed from here - the slot wait above is not part of
    // run_duration_seconds, and a wait cut short never ran.
    let start = Instant::now();
    let result = crate::component::execute(engine, m, request, &opts, &limits, hold, wait_deadline);
    crate::metrics::RUN_DURATION
        .with_label_values(&[crate::metrics::run_outcome(&result)])
        .observe(start.elapsed().as_secs_f64());
    result
}

/// Converts a run's budget into epoch ticks, at least one.
pub(crate) fn deadline_ticks(timeout: Duration) -> (u64, Duration) {
    let ticks = timeout.as_nanos().div_ceil(EPOCH_TICK.as_nanos()).max(1);
    (ticks as u64, timeout)
}

/// Turns wasmtime's failure into something an operator can act on from an XR
/// condition: a deadline interrupt becomes the timeout message, a WASI exit
/// reports its status and a trap is named by its code. wasmtime's backtrace
/// only helps next to the guest's own stderr, so it goes to the debug log.
pub(crate) fn guest_error(what: &str, err: wasmtime::Error, budget: Duration) -> RunError {
    let stopped = |message: String| RunError::new(RunFailure::Stopped, message);
    if let Some(exit) = err.downcast_ref::<I32Exit>() {
        return stopped(format!("{what}: module exited with status {}", exit.0));
    }
    if let Some(trap) = err.downcast_ref::<Trap>() {
        if *trap == Trap::Interrupt {
            return RunError::deadline(what, budget);
        }
        // The Debug format carries the wasm backtrace - file and line frames
        // when the engine's backtrace_details resolved the module's DWARF.
        tracing::debug!(trap = ?err, "Guest trapped");
        return stopped(format!("{what}: {}", trap_text(*trap)));
    }
    stopped(format!("{what}: {}", first_line(&err.to_string())))
}

/// Names a trap without wasmtime's backtrace.
fn trap_text(trap: Trap) -> String {
    match trap {
        Trap::StackOverflow => "trap: call stack exhausted".to_string(),
        Trap::MemoryOutOfBounds => "trap: out-of-bounds memory access".to_string(),
        Trap::UnreachableCodeReached => {
            "trap: unreachable code reached (a Go guest prints the panic to stderr)".to_string()
        }
        other => format!("trap: {other}"),
    }
}
