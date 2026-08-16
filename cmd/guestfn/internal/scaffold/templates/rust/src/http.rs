//! HTTP egress through the host (docs/abi.md, "HTTP egress"): the guest never
//! opens a socket; it hands the host a request and the host performs it within
//! the Composition's `sandbox.egress` grant and the operator's egress policy,
//! or answers with a refusal. The wire format is JSON both ways; the host
//! writes its response into a buffer allocated through this guest's own
//! `wasmfn_alloc`.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// One request for the host to perform.
#[derive(Debug, Clone, Default)]
pub struct Request {
    /// Method of the request; empty means GET.
    pub method: String,
    /// URL, http or https, absolute.
    pub url: String,
    /// Headers to send. Host, Content-Length and hop-by-hop headers are the
    /// host's to set and are dropped.
    pub headers: BTreeMap<String, Vec<String>>,
    /// Body bytes.
    pub body: Vec<u8>,
}

/// What the server answered: its status, whatever it is (a 503 is a
/// response, not an error), headers and body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, Vec<String>>,
    pub body: Vec<u8>,
}

/// The host's reason for not performing a request — refused by the grant or
/// the policy, over a budget, or failed — or a guest-side problem talking to
/// the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

// The JSON on the wire. `body` is base64 (standard alphabet, padded) both
// ways, as Go's encoding/json renders []byte.
#[derive(Serialize)]
struct WireRequest<'a> {
    #[serde(skip_serializing_if = "str::is_empty")]
    method: &'a str,
    url: &'a str,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    headers: &'a BTreeMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "String::is_empty")]
    body: String,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    status: u16,
    #[serde(default)]
    headers: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    error: String,
}

/// Performs a GET through the host.
pub fn get(url: &str) -> Result<Response, Error> {
    send(&Request {
        method: "GET".to_string(),
        url: url.to_string(),
        ..Default::default()
    })
}

/// Performs `req` through the host. A request the host does not perform is
/// an `Error` naming the reason; a response from the server, whatever its
/// status, is returned as is.
pub fn send(req: &Request) -> Result<Response, Error> {
    let payload = serde_json::to_vec(&WireRequest {
        method: &req.method,
        url: &req.url,
        headers: &req.headers,
        body: if req.body.is_empty() {
            String::new()
        } else {
            STANDARD.encode(&req.body)
        },
    })
    .map_err(|e| Error(format!("wasmfn: cannot encode the request: {e}")))?;
    let out = call(&payload)?;
    let rsp: WireResponse = serde_json::from_slice(&out).map_err(|e| {
        Error(format!(
            "wasmfn: cannot decode the host's HTTP response: {e}"
        ))
    })?;
    if !rsp.error.is_empty() {
        return Err(Error(rsp.error));
    }
    let body = if rsp.body.is_empty() {
        Vec::new()
    } else {
        STANDARD.decode(rsp.body).map_err(|e| {
            Error(format!(
                "wasmfn: the host's response body is not base64: {e}"
            ))
        })?
    };
    Ok(Response {
        status: rsp.status,
        headers: rsp.headers,
        body,
    })
}

/// Performs a GET and returns the body of a 200 as text, trimmed; any other
/// status is an error naming it.
pub fn get_text(url: &str) -> Result<String, Error> {
    let rsp = get(url)?;
    if rsp.status != 200 {
        return Err(Error(format!("GET {url}: status {}", rsp.status)));
    }
    Ok(String::from_utf8_lossy(&rsp.body).trim().to_string())
}

/// Hands a JSON request to the host and returns its JSON response. On wasi
/// that is the `wasmfn.http` import; elsewhere there is no host and the
/// request is refused — a native test installs its own with `set_host`.
#[cfg(target_os = "wasi")]
fn call(payload: &[u8]) -> Result<Vec<u8>, Error> {
    #[link(wasm_import_module = "wasmfn")]
    extern "C" {
        fn http(ptr: u32, len: u32) -> u64;
    }
    // SAFETY: the host reads len bytes at ptr during the call, then re-enters
    // wasmfn_alloc for its response — a buffer that lands in the pinned set
    // like any other — and returns its pointer and length.
    let packed = unsafe { http(payload.as_ptr() as u32, payload.len() as u32) };
    let (ptr, n) = ((packed >> 32) as u32, packed as u32 as usize);
    match crate::abi::take(ptr) {
        Some(mut buf) if buf.len() >= n => {
            buf.truncate(n);
            Ok(buf)
        }
        _ => Err(Error(
            "wasmfn: the host answered with a buffer this guest did not allocate".to_string(),
        )),
    }
}

/// A stand-in host for native builds: receives the JSON request, returns the
/// JSON response the runtime would.
#[cfg(not(target_os = "wasi"))]
pub type Host = Box<dyn Fn(&[u8]) -> Result<Vec<u8>, Error>>;

#[cfg(not(target_os = "wasi"))]
fn call(payload: &[u8]) -> Result<Vec<u8>, Error> {
    HOST.with(|h| match &*h.borrow() {
        Some(host) => host(payload),
        None => Err(Error(
            "wasmfn: no host HTTP in this build (not running under function-wasm)".to_string(),
        )),
    })
}

#[cfg(not(target_os = "wasi"))]
thread_local! {
    static HOST: std::cell::RefCell<Option<Host>> = const { std::cell::RefCell::new(None) };
}

/// Installs a fake host for native tests.
#[cfg(not(target_os = "wasi"))]
pub fn set_host(host: impl Fn(&[u8]) -> Result<Vec<u8>, Error> + 'static) {
    HOST.with(|h| *h.borrow_mut() = Some(Box::new(host)));
}
