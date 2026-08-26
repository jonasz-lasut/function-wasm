//! The wasmfn.http import (docs/abi.md): reads a JSON wire::Request from
//! guest memory, has the run's HttpRequester perform it, allocates the JSON
//! wire::Response in the guest through its own wasmfn_alloc - called
//! re-entrantly, while wasmfn_run is on the stack - copies it there and
//! returns ptr<<32|len. Every request-level failure travels back inside the
//! Response; only what leaves the instance unusable - a trap inside
//! wasmfn_alloc - becomes a trap of the run.

use std::time::Instant;

use wasmtime::Caller;

use crate::run::check_bounds;
use crate::wire;
use crate::{Ctx, EXPORT_ALLOC, EXPORT_MEMORY, first_line};

/// What a wasmfn.http call gets on a run without a grant. (A grant the
/// operator policy does not permit never reaches a run: it is a fatal result
/// before the module runs.)
pub(crate) const NO_EGRESS: &str = "sandbox.egress: HTTP egress is not granted to this module: its manifest requires no egress (requires.egress.http)";

/// Answers the wasmfn.http import for one run: the host side of the module's
/// sandbox.egress grant. It never fails - whatever stops a request is a
/// Response with status 0 and an error - so a guest always gets a well-formed
/// answer and never a trap. The deadline is the run's: a request never
/// outlives it.
pub trait HttpRequester: Send + Sync {
    fn do_request(&self, req: &wire::Request, deadline: Instant) -> wire::Response;
}

pub(crate) fn host_http(mut caller: Caller<'_, Ctx>, ptr: i32, size: i32) -> wasmtime::Result<i64> {
    let Some(memory) = caller
        .get_export(EXPORT_MEMORY)
        .and_then(|e| e.into_memory())
    else {
        return Err(wasmtime::Error::msg(
            "wasmfn.http: the module exports no memory",
        ));
    };
    // The request is copied out before anything else runs in the guest:
    // wasmfn_alloc may grow the memory and move it.
    let data = memory.data(&caller);
    let (p, n) = (ptr as u32, size as u32);
    check_bounds(data.len(), p, n)
        .map_err(|b| wasmtime::Error::msg(format!("wasmfn.http: request buffer {b}")))?;
    let payload = data[p as usize..][..n as usize].to_vec();

    let out = encode_response(&serve(caller.data_mut(), &payload));

    // The response lives in a buffer the guest allocated, so a guest finds it
    // in its pinned buffers the way it finds the request.
    let Some(alloc) = caller.get_export(EXPORT_ALLOC).and_then(|e| e.into_func()) else {
        return Err(wasmtime::Error::msg(format!(
            "wasmfn.http: the module exports no {EXPORT_ALLOC}"
        )));
    };
    let alloc = alloc.typed::<i32, i32>(&caller).map_err(|e| {
        wasmtime::Error::msg(format!(
            "wasmfn.http: {EXPORT_ALLOC}: {}",
            first_line(&e.to_string())
        ))
    })?;
    // A trap inside the re-entered wasmfn_alloc propagates as the run's trap.
    let allocated = alloc.call(&mut caller, out.len() as i32)?;
    let out_ptr = allocated as u32;
    let data = memory.data_mut(&mut caller);
    check_bounds(data.len(), out_ptr, out.len() as u32).map_err(|b| {
        wasmtime::Error::msg(format!(
            "{EXPORT_ALLOC} returned an invalid buffer inside wasmfn.http: {b}"
        ))
    })?;
    data[out_ptr as usize..][..out.len()].copy_from_slice(&out);
    Ok(((u64::from(out_ptr) << 32) | out.len() as u64) as i64)
}

/// Renders the JSON a guest reads. A Response always encodes, so the
/// fallback is unreachable in practice; the size check keeps a 32-bit guest
/// addressable.
fn encode_response(rsp: &wire::Response) -> Vec<u8> {
    let out = match serde_json::to_vec(rsp) {
        Ok(out) => out,
        Err(e) => {
            return serde_json::to_vec(&wire::Response::refusal(format!(
                "sandbox.egress: cannot encode the response: {e}"
            )))
            .unwrap_or_default();
        }
    };
    if out.len() > i32::MAX as usize {
        return br#"{"status":0,"error":"sandbox.egress: the response exceeds what a 32-bit guest can address"}"#.to_vec();
    }
    out
}

/// Decodes one request and answers it: through the run's grant, bounded by
/// the run's remaining deadline, or with a refusal when the run has none.
fn serve(ctx: &mut Ctx, payload: &[u8]) -> wire::Response {
    let call = &mut ctx.call;
    let req: wire::Request = match serde_json::from_slice(payload) {
        Ok(req) => req,
        Err(e) => {
            let msg = format!("sandbox.egress: cannot decode the request: {e}");
            crate::metrics::HTTP_REQUESTS
                .with_label_values(&[crate::metrics::OUTCOME_ERROR])
                .inc();
            tracing::info!(module = %call.module, digest = %call.digest, outcome = "error", error = %msg, "Module HTTP request");
            return wire::Response::refusal(msg);
        }
    };
    let Some(http) = &call.http else {
        let (host, path) = host_and_path(&req.url);
        let method = if req.method.is_empty() {
            "GET"
        } else {
            &req.method
        };
        // A guest without a grant that keeps calling gets one info line, then
        // debug: it is the guest looping, not the host.
        if call.no_grant_logged {
            tracing::debug!(module = %call.module, digest = %call.digest, method, outcome = "refused", host, path, error = NO_EGRESS, "Module HTTP request");
        } else {
            tracing::info!(module = %call.module, digest = %call.digest, method, outcome = "refused", host, path, error = NO_EGRESS, "Module HTTP request");
        }
        call.no_grant_logged = true;
        crate::metrics::HTTP_REQUESTS
            .with_label_values(&["refused"])
            .inc();
        return wire::Response::refusal(NO_EGRESS);
    };
    http.do_request(&req, call.deadline)
}

/// A refusal's audit line names the host and path; enough of a URL parse for
/// a log field, without a URL dependency the egress policy will bring anyway.
fn host_and_path(url: &str) -> (&str, &str) {
    let rest = url.split_once("://").map_or("", |(_, rest)| rest);
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    (host, path.split('?').next().unwrap_or(path))
}
