//! The mechanics of one run: a fresh store, the WASI config, the epoch
//! deadline and memory limiter, the ABI calls, and the translation of
//! wasmtime failures into the messages the Go runtime produces.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wasmtime::{Store, Trap};
use wasmtime_wasi::I32Exit;
use wasmtime_wasi::WasiCtxBuilder;

use crate::{
    ARGV0, CallState, Ctx, EPOCH_TICK, EXPORT_ALLOC, EXPORT_INITIALIZE, EXPORT_MEMORY, EXPORT_RUN,
    Engine, Error, HostTimer, Module, RunOptions, duration, first_line, sandbox,
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
    // The pre-run reservation covers the module's initial memory - what
    // instantiation will claim; growth beyond it is reserved as the guest
    // grows (RunLimiter), so a run's pool footprint is what it actually
    // uses, not the worst-case ceiling.
    let hold = match &engine.mem {
        Some(pool) => {
            let initial = m.initial_memory_bytes().min(limits.memory_limit);
            pool.reserve(initial, wait_deadline).map_err(Error)?;
            Some(crate::PoolHold::new(Arc::clone(pool), initial))
        }
        None => None,
    };

    // The run is timed from here - the slot and memory waits above are not
    // part of run_duration_seconds, and a wait cut short never ran.
    let start = Instant::now();
    let result = execute(engine, m, request, opts, limits, hold, wait_deadline);
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

fn execute(
    engine: &Engine,
    m: &Module,
    request: &[u8],
    opts: RunOptions,
    limits: crate::Config,
    hold: Option<crate::PoolHold>,
    wait_deadline: Instant,
) -> Result<Vec<u8>, Error> {
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
        limits: crate::RunLimiter::new(limits.memory_limit, hold, wait_deadline),
        call: CallState {
            module: opts.module.clone(),
            digest: opts.digest.clone(),
            http: opts.http.clone(),
            deadline: Instant::now() + budget,
            no_grant_logged: false,
            timer: HostTimer::new(),
            http_host: Duration::ZERO,
        },
    };
    let mut store = Store::new(&engine.inner, ctx);
    store.limiter(|c| &mut c.limits);

    // A profiled run (--profile-guests) samples the guest every epoch tick;
    // the profiler lives beside the store so the epoch and call hooks reach
    // it without borrowing through the store. A profiler that cannot be
    // built is logged, never the run's failure.
    let profiler: Option<Arc<Mutex<Option<wasmtime::GuestProfiler>>>> =
        opts.profile_dir.as_ref().and_then(|_| {
            let name = if opts.module.is_empty() {
                "module"
            } else {
                opts.module.as_str()
            };
            match wasmtime::GuestProfiler::new(
                &engine.inner,
                name,
                EPOCH_TICK,
                [(name.to_string(), m.inner.clone())],
            ) {
                Ok(p) => Some(Arc::new(Mutex::new(Some(p)))),
                Err(e) => {
                    tracing::info!(module = %opts.module, error = %e, "Cannot profile the run");
                    None
                }
            }
        });

    // The deadline meters guest compute: time the run was blocked in
    // wasmfn.http is credited back tick for tick when the deadline fires,
    // so limits.timeout is not consumed by a slow server. The request's own
    // gRPC deadline stays the hard wall-clock cap; without one the credit
    // is still bounded, because every http request is capped by the run
    // deadline set above. A fired deadline with nothing to credit is the
    // timeout trap.
    let hard = opts.deadline;
    let mut credited = Duration::ZERO;
    match &profiler {
        None => {
            store.set_epoch_deadline(ticks);
            store.epoch_deadline_callback(move |cx| {
                let now = Instant::now();
                if let Some(h) = hard
                    && now >= h
                {
                    return Ok(wasmtime::UpdateDeadline::Interrupt);
                }
                let credit = cx.data().call.http_host.saturating_sub(credited);
                let mut extend = (credit.as_nanos() / EPOCH_TICK.as_nanos()) as u64;
                if extend == 0 {
                    return Ok(wasmtime::UpdateDeadline::Interrupt);
                }
                if let Some(h) = hard {
                    // Never extend past the hard deadline; at least one tick
                    // keeps the callback re-firing to enforce it.
                    let cap =
                        (h.duration_since(now).as_nanos() / EPOCH_TICK.as_nanos()).max(1) as u64;
                    extend = extend.min(cap);
                }
                credited += Duration::from_nanos(EPOCH_TICK.as_nanos() as u64 * extend);
                Ok(wasmtime::UpdateDeadline::Continue(extend))
            });
        }
        Some(p) => {
            // Sampling needs the callback every tick, so the budget is
            // bookkept here - one tick spent per fire, http credit added -
            // rather than in the deadline itself.
            let p = Arc::clone(p);
            let mut left = ticks;
            store.set_epoch_deadline(1);
            store.epoch_deadline_callback(move |cx| {
                if let Ok(mut guard) = p.lock()
                    && let Some(prof) = guard.as_mut()
                {
                    prof.sample(&cx, EPOCH_TICK);
                }
                let now = Instant::now();
                if let Some(h) = hard
                    && now >= h
                {
                    return Ok(wasmtime::UpdateDeadline::Interrupt);
                }
                let credit = cx.data().call.http_host.saturating_sub(credited);
                let extend = (credit.as_nanos() / EPOCH_TICK.as_nanos()) as u64;
                if extend > 0 {
                    credited += Duration::from_nanos(EPOCH_TICK.as_nanos() as u64 * extend);
                    left += extend;
                }
                if left == 0 {
                    return Ok(wasmtime::UpdateDeadline::Interrupt);
                }
                left -= 1;
                Ok(wasmtime::UpdateDeadline::Continue(1))
            });
        }
    }
    // Every host<->wasm transition feeds the guest/host time split (and the
    // profile's host markers when this run is profiled).
    let ph = profiler.clone();
    store.call_hook(move |mut cx, hook| {
        cx.data_mut().call.timer.transition(hook);
        if let Some(p) = &ph
            && let Ok(mut guard) = p.lock()
            && let Some(prof) = guard.as_mut()
        {
            prof.call_hook(&cx, hook);
        }
        Ok(())
    });

    let _running = engine.running();

    let result = drive(&mut store, m, request, budget);
    crate::metrics::HOSTCALL_DURATION.observe(store.data().call.timer.host_total().as_secs_f64());
    if let (Some(dir), Some(p)) = (&opts.profile_dir, profiler) {
        write_profile(dir, &opts.digest, &p);
    }
    result
}

/// Writes a finished run's profile as Firefox-profiler JSON named by the
/// module digest and a timestamp. A write failure is logged; the run's
/// outcome is the guest's.
fn write_profile(
    dir: &std::path::Path,
    digest: &str,
    profiler: &Mutex<Option<wasmtime::GuestProfiler>>,
) {
    let Some(p) = profiler.lock().ok().and_then(|mut guard| guard.take()) else {
        return;
    };
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = if digest.is_empty() {
        format!("module-{millis}")
    } else {
        format!("{}-{millis}", digest.replace(':', "-"))
    };
    let path = dir.join(format!("{name}.json"));
    let written = std::fs::File::create(&path)
        .map_err(|e| e.to_string())
        .and_then(|f| {
            p.finish(std::io::BufWriter::new(f))
                .map_err(|e| e.to_string())
        });
    match written {
        Ok(()) => tracing::info!(profile = %path.display(), "Wrote a guest profile"),
        Err(e) => {
            tracing::info!(error = %e, profile = %path.display(), "Cannot write the guest profile");
        }
    }
}

/// Instantiates the module in the prepared store and drives the ABI calls;
/// split from execute so the run's timing splits can be read off the store
/// whatever path it exits through.
fn drive(
    store: &mut Store<Ctx>,
    m: &Module,
    request: &[u8],
    budget: Duration,
) -> Result<Vec<u8>, Error> {
    let instance = m
        .pre
        .instantiate(&mut *store)
        .map_err(|e| guest_error("cannot instantiate module", e, budget))?;
    if let Some(init) = instance.get_func(&mut *store, EXPORT_INITIALIZE) {
        let init = init
            .typed::<(), ()>(&*store)
            .map_err(|e| guest_error(&format!("{EXPORT_INITIALIZE} failed"), e, budget))?;
        init.call(&mut *store, ())
            .map_err(|e| guest_error(&format!("{EXPORT_INITIALIZE} failed"), e, budget))?;
    }

    // The ABI check guarantees the exports exist with these types.
    let memory = instance
        .get_memory(&mut *store, EXPORT_MEMORY)
        .ok_or_else(|| {
            Error(format!(
                "module does not export a memory named {EXPORT_MEMORY:?}"
            ))
        })?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut *store, EXPORT_ALLOC)
        .map_err(|e| Error(format!("{EXPORT_ALLOC}: {}", first_line(&e.to_string()))))?;
    let run = instance
        .get_typed_func::<(i32, i32), i64>(&mut *store, EXPORT_RUN)
        .map_err(|e| Error(format!("{EXPORT_RUN}: {}", first_line(&e.to_string()))))?;

    // wasm i32 and i64 values carry the ABI's unsigned pointers and lengths;
    // the conversions below reinterpret bits, they do not change values.
    let size = request.len() as i32;
    let allocated = alloc
        .call(&mut *store, size)
        .map_err(|e| guest_error(&format!("{EXPORT_ALLOC} failed"), e, budget))?;
    let ptr = allocated as u32;
    check_bounds(memory.data_size(&*store), ptr, size as u32)
        .map_err(|b| Error(format!("{EXPORT_ALLOC} returned an invalid buffer: {b}")))?;
    memory.data_mut(&mut *store)[ptr as usize..][..request.len()].copy_from_slice(request);

    let packed =
        run.call(&mut *store, (ptr as i32, size))
            .map_err(|e| guest_error(&format!("{EXPORT_RUN} failed"), e, budget))? as u64;
    let (out_ptr, out_len) = ((packed >> 32) as u32, packed as u32);
    check_bounds(memory.data_size(&*store), out_ptr, out_len).map_err(|b| {
        Error(format!(
            "{EXPORT_RUN} returned an invalid response buffer: {b}"
        ))
    })?;
    // The store dies with this call, so the response is copied out.
    Ok(memory.data(&*store)[out_ptr as usize..][..out_len as usize].to_vec())
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
