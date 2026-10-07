//! The host of the wasmfn:function world (wit/wasmfn-function.wit,
//! docs/abi-v2.md): the bindgen, the store data of one run, the linker
//! every component is resolved against, and the run itself. The world
//! typecheck at load is the ABI check: the one place the contract is
//! enforced, whose verdict inspect reports.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use wasmtime::component::Accessor;
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

use crate::{
    ARGV0, CallState, Config, EPOCH_TICK, Engine, Error, HostTimer, RunOptions,
    run::deadline_ticks, sandbox,
};

/// The world a guest implements, named in refusals.
pub const ABI_V2_WORLD: &str = "wasmfn:function@2.0.0";

/// The world's one host import beyond WASI, as the component names it.
pub(crate) const LOG_IMPORT: &str = "log";

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "function",
});

/// The per-store data of a run: the WASI context (p2 + p3 both linked - a
/// wasip3 toolchain's std may still import wasi 0.2 interfaces), the memory
/// limiter and the state the host imports reach.
pub(crate) struct Ctx {
    wasi: WasiCtx,
    table: ResourceTable,
    http: WasiHttpCtx,
    hooks: crate::wasihttp::EgressHooks,
    pub(crate) limits: crate::RunLimiter,
    pub(crate) call: CallState,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for Ctx {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.hooks,
        }
    }
}

/// The typed log import (the world's `log`): the canonical ABI hands the
/// host native values, and the host attaches the module's identity.
impl FunctionImports for Ctx {
    fn log(&mut self, level: LogLevel, msg: String, kv: Vec<(String, String)>) {
        let (module, digest) = (self.call.module.as_str(), self.call.digest.as_str());
        // tracing fields are static, so the guest's keys and values travel
        // as one JSON-rendered field: a flat alternating array, the shape a
        // structured logger takes them in.
        let flat: Vec<serde_json::Value> = kv
            .into_iter()
            .flat_map(|(k, v)| [serde_json::Value::String(k), serde_json::Value::String(v)])
            .collect();
        let kv = serde_json::Value::Array(flat).to_string();
        match level {
            LogLevel::Debug => {
                tracing::debug!(module, digest, kv = %kv, "{msg}");
            }
            LogLevel::Info => {
                tracing::info!(module, digest, kv = %kv, "{msg}");
            }
            LogLevel::Warn => {
                tracing::warn!(module, digest, kv = %kv, "{msg}");
            }
            LogLevel::Error => {
                tracing::error!(module, digest, kv = %kv, "{msg}");
            }
        }
    }
}

/// True when the bytes are a core module in the wasm binary format: the
/// layer field (bytes 6-7) is zero. The layer, not the version, is the
/// durable discriminator - the component binary version has bumped before.
pub(crate) fn is_core_module_binary(wasm: &[u8]) -> bool {
    wasm.len() >= 8 && wasm[0..4] == *b"\0asm" && wasm[6..8] == [0, 0]
}

/// Builds the linker: WASI 0.3 and 0.2 (a guest's std may import either),
/// both wasi:http generations and the world's log import.
pub(crate) fn linker(engine: &wasmtime::Engine) -> Result<wasmtime::component::Linker<Ctx>, Error> {
    let mut linker: wasmtime::component::Linker<Ctx> = wasmtime::component::Linker::new(engine);
    wasmtime_wasi::p3::add_to_linker(&mut linker)
        .map_err(|e| Error(format!("cannot define WASI 0.3 imports: {e}")))?;
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| Error(format!("cannot define WASI 0.2 imports: {e}")))?;
    // The import always exists; the run's grant decides what answers it.
    // Both wasi:http generations serve the same hooks bridge: 0.3 for async
    // guests, 0.2 for toolchains that reach fetch through it
    // (componentize-js) - one egress policy either way.
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)
        .map_err(|e| Error(format!("cannot define wasi:http imports: {e}")))?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
        .map_err(|e| Error(format!("cannot define wasi:http 0.2 imports: {e}")))?;
    Function::add_to_linker::<Ctx, wasmtime::component::HasSelf<Ctx>>(&mut linker, |c| c)
        .map_err(|e| Error(format!("cannot define the log import: {e}")))?;
    Ok(linker)
}

/// One run: fresh store, the sandbox and the epoch-deadline model, the
/// world's `run` export driven on the tokio runtime (ambient in tests, the
/// caller's when the engine runs off spawn_blocking).
pub(crate) fn execute(
    engine: &Engine,
    m: &crate::Module,
    request: &[u8],
    opts: &RunOptions,
    limits: &Config,
    hold: Option<crate::PoolHold>,
    wait_deadline: Instant,
) -> Result<Vec<u8>, Error> {
    // The private /tmp outlives the store (declared first, so it drops
    // last): the guest's descriptors into it are closed before it is
    // removed.
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
    sandbox::configure(&mut wasi, opts, tmp.path())?;

    // Time blocked in wasi:http is written here by the hooks and read by
    // the deadline callback - shared because wasmtime-wasi-http borrows the
    // hooks separately from the rest of the store data.
    let http_host = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + budget;
    let ctx = Ctx {
        wasi: wasi.build(),
        table: ResourceTable::new(),
        http: WasiHttpCtx::new(),
        hooks: crate::wasihttp::EgressHooks::new(
            opts.http.clone(),
            deadline,
            opts.module.clone(),
            opts.digest.clone(),
            Arc::clone(&http_host),
        ),
        limits: crate::RunLimiter::new(limits.memory_limit, hold, wait_deadline),
        call: CallState {
            module: opts.module.clone(),
            digest: opts.digest.clone(),
            timer: HostTimer::new(),
        },
    };
    let mut store = wasmtime::Store::new(&engine.inner, ctx);
    store.limiter(|c| &mut c.limits);

    // The deadline meters guest compute: time the run was blocked in
    // wasi:http is credited back tick for tick when the deadline fires (the
    // hooks count it into the shared counter), so limits.timeout is not
    // consumed by a slow server. The request's own gRPC deadline stays the
    // hard wall-clock cap; without one the credit is still bounded, because
    // every http request is capped by the run deadline set above. A fired
    // deadline with nothing to credit is the timeout trap.
    let hard = opts.deadline;
    let mut credited = std::time::Duration::ZERO;
    let credit_source = Arc::clone(&http_host);
    store.set_epoch_deadline(ticks);
    store.epoch_deadline_callback(move |_cx| {
        let now = Instant::now();
        if let Some(h) = hard
            && now >= h
        {
            return Ok(wasmtime::UpdateDeadline::Interrupt);
        }
        let credit = std::time::Duration::from_nanos(credit_source.load(Ordering::Relaxed))
            .saturating_sub(credited);
        let mut extend = (credit.as_nanos() / EPOCH_TICK.as_nanos()) as u64;
        if extend == 0 {
            return Ok(wasmtime::UpdateDeadline::Interrupt);
        }
        if let Some(h) = hard {
            // Never extend past the hard deadline; at least one tick keeps
            // the callback re-firing to enforce it.
            let cap = (h.duration_since(now).as_nanos() / EPOCH_TICK.as_nanos()).max(1) as u64;
            extend = extend.min(cap);
        }
        credited += std::time::Duration::from_nanos(EPOCH_TICK.as_nanos() as u64 * extend);
        Ok(wasmtime::UpdateDeadline::Continue(extend))
    });
    // Every host<->wasm transition feeds the guest/host time split.
    store.call_hook(move |mut cx, hook| {
        cx.data_mut().call.timer.transition(hook);
        Ok(())
    });

    let _running = engine.running();

    // What one drive produced; the store travels back with it so the
    // hostcall split can be read off it whatever path it exits through.
    enum Outcome {
        Response(Vec<u8>),
        // The guest's own error string, the world's third channel beside a
        // response and a trap. It becomes the request's fatal result, naming
        // the export that produced it.
        GuestError(String),
        Failed(&'static str, wasmtime::Error),
    }

    let pre = m.pre.clone();
    let req = request.to_vec();
    let (store, outcome) = wasmtime_wasi::runtime::in_tokio(async move {
        let instance = match pre.instantiate_async(&mut store).await {
            Ok(i) => i,
            Err(e) => return (store, Outcome::Failed("cannot instantiate module", e)),
        };
        let out = store
            .run_concurrent(async |accessor: &Accessor<Ctx>| instance.call_run(accessor, req).await)
            .await;
        let outcome = match out {
            Ok(Ok(Ok(response))) => Outcome::Response(response),
            Ok(Ok(Err(msg))) => Outcome::GuestError(msg),
            Ok(Err(e)) | Err(e) => Outcome::Failed("run failed", e),
        };
        (store, outcome)
    });

    crate::metrics::HOSTCALL_DURATION.observe(store.data().call.timer.host_total().as_secs_f64());
    match outcome {
        Outcome::Response(response) => Ok(response),
        Outcome::GuestError(msg) => Err(Error(format!("run returned an error: {msg}"))),
        Outcome::Failed(what, e) => Err(crate::run::guest_error(what, e, budget)),
    }
}
