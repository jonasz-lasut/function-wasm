//! The wasmfn.log import: reads a JSON record from guest memory and forwards
//! it to the host's logger with the module reference and digest attached.
//! Malformed records are logged as-is rather than dropped or turned into
//! traps, so a guest can never lose its own diagnostics.

use serde::Deserialize;
use wasmtime::Caller;

use crate::run::check_bounds;
use crate::{Ctx, EXPORT_MEMORY};

/// The wasmfn.log level a guest sends for a debug record; any other level
/// (0, info) logs at info.
const LEVEL_DEBUG: i32 = 1;

/// The JSON payload of one wasmfn.log call: a message and alternating keys
/// and values, as a structured logger takes them.
#[derive(Deserialize)]
struct LogRecord {
    #[serde(default)]
    msg: String,
    #[serde(default)]
    kv: Vec<serde_json::Value>,
}

pub(crate) fn host_log(mut caller: Caller<'_, Ctx>, level: i32, ptr: i32, size: i32) {
    let Some(memory) = caller
        .get_export(EXPORT_MEMORY)
        .and_then(|e| e.into_memory())
    else {
        return;
    };
    let data = memory.data(&caller);
    let (p, n) = (ptr as u32, size as u32);
    if check_bounds(data.len(), p, n).is_err() {
        tracing::info!(ptr = p, len = n, "guest log record out of bounds");
        return;
    }
    let payload = data[p as usize..][..n as usize].to_vec();

    let call = &caller.data().call;
    let (module, digest) = (call.module.as_str(), call.digest.as_str());
    let mut rec: LogRecord = match serde_json::from_slice(&payload) {
        Ok(rec) => rec,
        Err(e) => {
            tracing::info!(module, digest, wasmfn_log_error = %e, "{}", String::from_utf8_lossy(&payload));
            return;
        }
    };
    if !rec.kv.len().is_multiple_of(2) {
        rec.kv
            .push(serde_json::Value::String("(missing value)".to_string()));
    }
    // tracing fields are static; the guest's keys and values travel as one
    // JSON-rendered field instead.
    let kv = serde_json::Value::Array(rec.kv).to_string();
    if level == LEVEL_DEBUG {
        tracing::debug!(module, digest, kv = %kv, "{}", rec.msg);
    } else {
        tracing::info!(module, digest, kv = %kv, "{}", rec.msg);
    }
}
