//! The bounds around one run - the run slot, the memory reservation and
//! the run metric - and the translation of wasmtime failures into the
//! messages the Go runtime produces. The run itself (store, WASI, deadline,
//! the world's export) is component::execute.

use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmtime::Trap;
use wasmtime_wasi::I32Exit;

use crate::{EPOCH_TICK, Engine, Error, Module, RunOptions, duration, first_line};

pub(crate) fn run(
    engine: &Engine,
    m: &Module,
    request: &[u8],
    opts: RunOptions,
) -> Result<Vec<u8>, Error> {
    let limits = engine.effective(&opts);
    // A run slot (round-robin by module key when --max-concurrent-runs
    // bounds them) and the memory reservation come first, waited for under
    // the run's own budget - or the request's deadline when that is
    // sooner: a wait cut short held and consumed nothing.
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
        .with_label_values(&[run_outcome(&result)])
        .observe(start.elapsed().as_secs_f64());
    result
}

fn run_outcome<T>(result: &Result<T, Error>) -> &'static str {
    match result {
        Ok(_) => crate::metrics::OUTCOME_OK,
        Err(e) if e.0.contains("exceeded its execution deadline") => {
            crate::metrics::OUTCOME_TIMEOUT
        }
        Err(_) => crate::metrics::OUTCOME_ERROR,
    }
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
pub(crate) fn guest_error(what: &str, err: wasmtime::Error, budget: Duration) -> Error {
    if let Some(exit) = err.downcast_ref::<I32Exit>() {
        return Error(format!("{what}: module exited with status {}", exit.0));
    }
    if let Some(trap) = err.downcast_ref::<Trap>() {
        if *trap == Trap::Interrupt {
            return Error(format!(
                "{what}: module exceeded its execution deadline ({})",
                duration::format(budget)
            ));
        }
        // The Debug format carries the wasm backtrace - file and line frames
        // when the engine's backtrace_details resolved the module's DWARF.
        tracing::debug!(trap = ?err, "Guest trapped");
        return Error(format!("{what}: {}", trap_text(*trap)));
    }
    Error(format!("{what}: {}", first_line(&err.to_string())))
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
