//! The ABI v2 world: wit-bindgen generates the component bindings for the
//! guest's world (wit/guest.wit - the vendored wasmfn:function contract plus
//! the wasi:http client), and this module implements the async `run` export
//! and the greeting fetch over `wasi:http/client@0.3.0`. wasm-only: the
//! canonical ABI has no native analogue, so native tests drive
//! `run_function` with a fetch double instead.

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
    generate_all,
});

use wasi::http::client;
use wasi::http::types::{Fields, Request, Response, Scheme};

struct Guest2;

impl Guest for Guest2 {
    async fn run(request: Vec<u8>) -> Result<Vec<u8>, String> {
        Ok(crate::handle(&request, fetch_text).await)
    }
}

/// GETs a URL through the host's wasi:http client and returns the trimmed
/// body, the v2 counterpart of an ABI v1 guest's get_text helper.
async fn fetch_text(url: String) -> Result<String, String> {
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (Scheme::Http, rest)
    } else {
        return Err(format!("GET {url}: only http and https URLs work"));
    };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));

    let headers = Fields::new();
    let (trailers_tx, trailers_rx) = wit_future::new(|| Ok(None));
    let (req, _transmit) = Request::new(headers, None, trailers_rx, None);
    // Dropping the writer resolves the trailers future to its default: the
    // zero-length request body is complete. Holding it would hang the send.
    drop(trailers_tx);
    req.set_scheme(Some(&scheme))
        .map_err(|()| format!("GET {url}: invalid scheme"))?;
    req.set_authority(Some(authority))
        .map_err(|()| format!("GET {url}: invalid authority"))?;
    req.set_path_with_query(Some(&format!("/{path}")))
        .map_err(|()| format!("GET {url}: invalid path"))?;

    let rsp = client::send(req)
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = rsp.get_status_code();
    let (res_tx, res_rx) = wit_future::new(|| Ok(()));
    let (stream, _trailers) = Response::consume_body(rsp, res_rx);
    drop(res_tx);
    let body = stream.collect().await;
    if status != 200 {
        return Err(format!("GET {url}: status {status}"));
    }
    Ok(String::from_utf8_lossy(&body).trim().to_string())
}

export!(Guest2);
