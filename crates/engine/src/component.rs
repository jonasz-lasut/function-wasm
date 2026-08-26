//! The ABI v2 host: components implementing the wasmfn:function world
//! (wit/wasmfn-function.wit, docs/abi-v2.md). Detection is the binary
//! format - a component is ABI v2, a core module is ABI v1 - and the world
//! typecheck at load is v2's checkABI: the one place the contract is
//! enforced, whose verdict inspect reports.

use std::time::{Duration, Instant};

use wasmtime::component::Accessor;
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::{
    ARGV0, CallState, Config, EPOCH_TICK, Engine, Error, HostTimer, RunOptions, first_line,
    run::deadline_ticks, sandbox,
};

/// The world a v2 guest implements, named in refusals.
pub const ABI_V2_WORLD: &str = "wasmfn:function@2.0.0-draft";

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "function",
});

/// The per-store data of a v2 run: the WASI context (p2 + p3 both linked -
/// a wasip3 toolchain's std may still import wasi 0.2 interfaces), the
/// memory limiter and the state the host imports reach.
pub(crate) struct CtxV2 {
    wasi: WasiCtx,
    table: ResourceTable,
    pub(crate) limits: crate::RunLimiter,
    pub(crate) call: CallState,
}

impl WasiView for CtxV2 {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// The typed log import (the world's `log`): v1's JSON payload and guest
/// memory read are gone, the canonical ABI hands the host native values.
impl FunctionImports for CtxV2 {
    fn log(&mut self, level: LogLevel, msg: String, kv: Vec<(String, String)>) {
        let kv = serde_json::to_string(&kv).unwrap_or_default();
        // The module identity attached the way hostlog does it; kv rendered
        // as one JSON field because tracing fields are static.
        match level {
            LogLevel::Debug => {
                tracing::debug!(module = %self.call.module, digest = %self.call.digest, kv = %kv, "{msg}");
            }
            LogLevel::Info => {
                tracing::info!(module = %self.call.module, digest = %self.call.digest, kv = %kv, "{msg}");
            }
        }
    }
}

/// A compiled, world-checked component with its imports resolved once, the
/// v2 counterpart of the core path's InstancePre.
#[derive(Clone)]
pub(crate) struct ComponentModule {
    pub(crate) inner: wasmtime::component::Component,
    pub(crate) pre: FunctionPre<CtxV2>,
}

/// True when the bytes are a component in the wasm binary format: the layer
/// field (bytes 6-7) is non-zero. The layer, not the version, is the durable
/// discriminator - the component binary version has bumped before.
pub(crate) fn is_component_binary(wasm: &[u8]) -> bool {
    wasm.len() >= 8 && wasm[0..4] == *b"\0asm" && wasm[6..8] != [0, 0]
}

impl Engine {
    /// Compiles component bytes; the caller has already detected the format.
    pub(crate) fn compiled_component(
        &self,
        wasm: &[u8],
    ) -> Result<wasmtime::component::Component, Error> {
        wasmtime::component::Component::from_binary(&self.inner, wasm).map_err(|e| {
            Error(format!(
                "cannot compile module: {}",
                first_line(&e.to_string())
            ))
        })
    }

    /// Resolves the component's imports and typechecks it against the
    /// wasmfn:function world - ABI v2's checkABI, at load, once.
    pub(crate) fn pre_component(
        &self,
        c: wasmtime::component::Component,
    ) -> Result<ComponentModule, Error> {
        let pre = self
            .clinker
            .instantiate_pre(&c)
            .and_then(FunctionPre::new)
            .map_err(|e| {
                Error(format!(
                    "component does not implement the {ABI_V2_WORLD} world: {}",
                    first_line(&e.to_string())
                ))
            })?;
        Ok(ComponentModule { inner: c, pre })
    }
}

/// Builds the v2 linker: WASI 0.3 and 0.2 (a guest's std may import either)
/// and the world's log import.
pub(crate) fn linker(
    engine: &wasmtime::Engine,
) -> Result<wasmtime::component::Linker<CtxV2>, Error> {
    let mut linker: wasmtime::component::Linker<CtxV2> = wasmtime::component::Linker::new(engine);
    wasmtime_wasi::p3::add_to_linker(&mut linker)
        .map_err(|e| Error(format!("cannot define WASI 0.3 imports: {e}")))?;
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)
        .map_err(|e| Error(format!("cannot define WASI 0.2 imports: {e}")))?;
    Function::add_to_linker::<CtxV2, wasmtime::component::HasSelf<CtxV2>>(&mut linker, |c| c)
        .map_err(|e| Error(format!("cannot define the log import: {e}")))?;
    Ok(linker)
}

/// One v2 run: fresh store, the same sandbox and epoch-deadline model as a
/// v1 run, the world's `run` export driven on the tokio runtime (ambient in
/// tests, the caller's when the engine runs off spawn_blocking).
pub(crate) fn execute(
    engine: &Engine,
    m: &ComponentModule,
    request: &[u8],
    opts: &RunOptions,
    limits: &Config,
    hold: Option<crate::PoolHold>,
    wait_deadline: Instant,
) -> Result<Vec<u8>, Error> {
    let tmp = sandbox::PrivateTmp::create(opts.private_tmp)?;

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

    let ctx = CtxV2 {
        wasi: wasi.build(),
        table: ResourceTable::new(),
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
    let mut store = wasmtime::Store::new(&engine.inner, ctx);
    store.limiter(|c| &mut c.limits);

    // The same deadline model as a v1 run: guest compute is metered in epoch
    // ticks, time blocked in host HTTP is credited back (http_host stays
    // zero until the wasi:http import exists), the request's gRPC deadline
    // is the hard cap.
    let hard = opts.deadline;
    let mut credited = Duration::ZERO;
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
            let cap = (h.duration_since(now).as_nanos() / EPOCH_TICK.as_nanos()).max(1) as u64;
            extend = extend.min(cap);
        }
        credited += Duration::from_nanos(EPOCH_TICK.as_nanos() as u64 * extend);
        Ok(wasmtime::UpdateDeadline::Continue(extend))
    });
    store.call_hook(move |mut cx, hook| {
        cx.data_mut().call.timer.transition(hook);
        Ok(())
    });

    let _running = engine.running();

    // What one v2 drive produced; the store travels back with it so the
    // hostcall split can be read off it whatever path it exits through.
    enum Outcome {
        Response(Vec<u8>),
        // The guest's own error string: v2's third channel (v1 guests encode
        // failures into the protobuf response). It becomes the request's
        // fatal result, naming the export that produced it.
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
            .run_concurrent(async |accessor: &Accessor<CtxV2>| {
                instance.call_run(accessor, req).await
            })
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
