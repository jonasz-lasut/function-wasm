//! The mechanics of one run: a fresh store, the WASI config, the epoch
//! deadline and memory limiter, the ABI calls, and the translation of
//! wasmtime failures into the messages the Go runtime produces.

use std::time::{Duration, Instant};

use wasmtime::{Store, StoreLimitsBuilder, Trap};
use wasmtime_wasi::I32Exit;
use wasmtime_wasi::WasiCtxBuilder;

use crate::{
    ARGV0, CallState, Ctx, EPOCH_TICK, EXPORT_ALLOC, EXPORT_INITIALIZE, EXPORT_MEMORY, EXPORT_RUN,
    Engine, Error, Module, RunOptions, duration, first_line, sandbox,
};

pub(crate) fn run(
    engine: &Engine,
    m: &Module,
    request: &[u8],
    opts: RunOptions,
) -> Result<Vec<u8>, Error> {
    if request.len() > i32::MAX as usize {
        return Err(Error(format!(
            "request of {} bytes exceeds what a 32-bit guest can address",
            request.len()
        )));
    }

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
    let _mem = match &engine.mem {
        Some(m) => Some(
            m.reserve(limits.memory_limit, wait_deadline)
                .map_err(Error)?,
        ),
        None => None,
    };

    // The private /tmp outlives the store (declared first, so it drops last):
    // the guest's descriptors into it are closed before it is removed.
    let tmp = sandbox::PrivateTmp::create(opts.private_tmp)?;

    // The run budget is the effective timeout, capped by what remains of
    // the request's deadline.
    let mut timeout = limits.timeout;
    if let Some(d) = opts.deadline {
        timeout = timeout.min(d.saturating_duration_since(Instant::now()));
    }
    let (ticks, budget) = deadline_ticks(timeout);

    let mut wasi = WasiCtxBuilder::new();
    wasi.args(&[ARGV0]);
    wasi.inherit_stdout();
    wasi.inherit_stderr();
    sandbox::configure(&mut wasi, &opts, tmp.path())?;

    let ctx = Ctx {
        wasi: wasi.build_p1(),
        limits: StoreLimitsBuilder::new()
            .memory_size(limits.memory_limit as usize)
            .build(),
        call: CallState {
            module: opts.module.clone(),
            digest: opts.digest.clone(),
            http: opts.http.clone(),
            deadline: Instant::now() + budget,
            no_grant_logged: false,
        },
    };
    let mut store = Store::new(&engine.inner, ctx);
    store.limiter(|c| &mut c.limits);
    store.set_epoch_deadline(ticks);

    let _running = engine.running();

    let instance = engine
        .linker
        .instantiate(&mut store, &m.0)
        .map_err(|e| guest_error("cannot instantiate module", e, budget))?;
    if let Some(init) = instance.get_func(&mut store, EXPORT_INITIALIZE) {
        let init = init
            .typed::<(), ()>(&store)
            .map_err(|e| guest_error(&format!("{EXPORT_INITIALIZE} failed"), e, budget))?;
        init.call(&mut store, ())
            .map_err(|e| guest_error(&format!("{EXPORT_INITIALIZE} failed"), e, budget))?;
    }

    // The ABI check guarantees the exports exist with these types.
    let memory = instance
        .get_memory(&mut store, EXPORT_MEMORY)
        .ok_or_else(|| {
            Error(format!(
                "module does not export a memory named {EXPORT_MEMORY:?}"
            ))
        })?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut store, EXPORT_ALLOC)
        .map_err(|e| Error(format!("{EXPORT_ALLOC}: {}", first_line(&e.to_string()))))?;
    let run = instance
        .get_typed_func::<(i32, i32), i64>(&mut store, EXPORT_RUN)
        .map_err(|e| Error(format!("{EXPORT_RUN}: {}", first_line(&e.to_string()))))?;

    // wasm i32 and i64 values carry the ABI's unsigned pointers and lengths;
    // the conversions below reinterpret bits, they do not change values.
    let size = request.len() as i32;
    let allocated = alloc
        .call(&mut store, size)
        .map_err(|e| guest_error(&format!("{EXPORT_ALLOC} failed"), e, budget))?;
    let ptr = allocated as u32;
    check_bounds(memory.data_size(&store), ptr, size as u32)
        .map_err(|b| Error(format!("{EXPORT_ALLOC} returned an invalid buffer: {b}")))?;
    memory.data_mut(&mut store)[ptr as usize..][..request.len()].copy_from_slice(request);

    let packed =
        run.call(&mut store, (ptr as i32, size))
            .map_err(|e| guest_error(&format!("{EXPORT_RUN} failed"), e, budget))? as u64;
    let (out_ptr, out_len) = ((packed >> 32) as u32, packed as u32);
    check_bounds(memory.data_size(&store), out_ptr, out_len).map_err(|b| {
        Error(format!(
            "{EXPORT_RUN} returned an invalid response buffer: {b}"
        ))
    })?;
    // The store dies with this call, so the response is copied out.
    Ok(memory.data(&store)[out_ptr as usize..][..out_len as usize].to_vec())
}

/// Converts a run's budget into epoch ticks, at least one.
fn deadline_ticks(timeout: Duration) -> (u64, Duration) {
    let ticks = timeout.as_nanos().div_ceil(EPOCH_TICK.as_nanos()).max(1);
    (ticks as u64, timeout)
}

pub(crate) fn check_bounds(size: usize, ptr: u32, n: u32) -> Result<(), String> {
    let end = u64::from(ptr) + u64::from(n);
    if end > size as u64 {
        return Err(format!(
            "[{ptr:#x}, {end:#x}) exceeds the {size} bytes of guest memory"
        ));
    }
    Ok(())
}

/// Turns wasmtime's failure into something an operator can act on from an XR
/// condition: a deadline interrupt becomes the timeout message, a WASI exit
/// reports its status and a trap is named by its code. wasmtime's backtrace
/// only helps next to the guest's own stderr, so it goes to the debug log.
fn guest_error(what: &str, err: wasmtime::Error, budget: Duration) -> Error {
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
        tracing::debug!(trap = %err, "Guest trapped");
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
