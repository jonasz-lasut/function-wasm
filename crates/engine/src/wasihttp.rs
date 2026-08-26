//! The wasi:http@0.3 host of ABI v2 (docs/abi-v2.md "HTTP egress"): the
//! `client.send` import bridged onto the same egress seam as v1's
//! wasmfn.http - the HttpRequester behind it applies the grant, the SSRF
//! judgment, the budgets, the rate limit, the audit line and the metric.
//! The crate builds wasmtime-wasi-http without default-send-request, so
//! this implementation is compulsory: a run can never fall through to an
//! unpoliced default client.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use base64::Engine as _;
use http_body_util::BodyExt;
use wasmtime_wasi_http::{Error as HttpError, RequestOptions, WasiBody, WasiHttpHooks};

use crate::hosthttp::NO_EGRESS;
use crate::wire;

/// The per-run state behind the guest's wasi:http imports. Split from the
/// store data because wasmtime-wasi-http borrows the hooks and the resource
/// table from it at once.
pub(crate) struct EgressHooks {
    /// The run's grant; None refuses every send, never a trap.
    requester: Option<Arc<dyn crate::HttpRequester>>,
    /// The run's deadline - each request is capped by what remains of it.
    deadline: Instant,
    module: String,
    digest: String,
    /// One info line for a guest that keeps sending without a grant, then
    /// debug: it is the guest looping, not the host.
    no_grant_logged: bool,
    /// Nanoseconds this run spent blocked in send - credited back to the
    /// epoch deadline so limits.timeout means guest compute, shared with
    /// the run's deadline callback.
    http_host: Arc<AtomicU64>,
}

impl EgressHooks {
    pub(crate) fn new(
        requester: Option<Arc<dyn crate::HttpRequester>>,
        deadline: Instant,
        module: String,
        digest: String,
        http_host: Arc<AtomicU64>,
    ) -> Self {
        EgressHooks {
            requester,
            deadline,
            module,
            digest,
            no_grant_logged: false,
            http_host,
        }
    }
}

type SendResult = wasmtime_wasi_http::Result<(
    http::Response<WasiBody>,
    Box<dyn Future<Output = wasmtime_wasi_http::Result<()>> + Send>,
)>;

impl WasiHttpHooks for EgressHooks {
    fn send_request(
        &mut self,
        request: http::Request<WasiBody>,
        _options: Option<RequestOptions>,
        _fut: Box<dyn Future<Output = wasmtime_wasi_http::Result<()>> + Send>,
    ) -> Box<dyn Future<Output = SendResult> + Send> {
        let Some(requester) = self.requester.clone() else {
            let method = request.method().as_str().to_string();
            let host = request.uri().host().unwrap_or_default();
            let path = request.uri().path();
            if self.no_grant_logged {
                tracing::debug!(module = %self.module, digest = %self.digest, method, outcome = "refused", host, path, error = NO_EGRESS, "Module HTTP request");
            } else {
                tracing::info!(module = %self.module, digest = %self.digest, method, outcome = "refused", host, path, error = NO_EGRESS, "Module HTTP request");
            }
            self.no_grant_logged = true;
            crate::metrics::HTTP_REQUESTS
                .with_label_values(&["refused"])
                .inc();
            // The reason travels to the guest: wasi:http's payload-carrying
            // code is internal-error, and v1's refusal string is contract.
            return Box::new(async { Err(HttpError::InternalError(Some(NO_EGRESS.to_string()))) });
        };

        let deadline = self.deadline;
        let http_host = Arc::clone(&self.http_host);
        Box::new(async move {
            let started = Instant::now();
            let result = send(requester, request, deadline).await;
            // Time blocked here is host time, not guest compute; the epoch
            // deadline callback credits it back.
            http_host.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            result
        })
    }
}

/// One granted send: buffer the outgoing body, hand the request to the
/// policy client on a blocking thread (it resolves, judges, budgets, audits
/// and counts), and map its answer back. Bodies are complete on both sides,
/// exactly as under v1: the budget acts on whole responses.
async fn send(
    requester: Arc<dyn crate::HttpRequester>,
    request: http::Request<WasiBody>,
    deadline: Instant,
) -> SendResult {
    let (parts, body) = request.into_parts();
    let collected = body
        .collect()
        .await
        .map_err(|e| HttpError::InternalError(Some(format!("cannot read the request body: {e}"))))?
        .to_bytes();

    let mut headers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in &parts.headers {
        headers
            .entry(name.as_str().to_string())
            .or_default()
            .push(String::from_utf8_lossy(value.as_bytes()).into_owned());
    }
    let wreq = wire::Request {
        method: parts.method.as_str().to_string(),
        url: parts.uri.to_string(),
        headers,
        body: base64::engine::general_purpose::STANDARD.encode(&collected),
    };

    // The policy client is blocking (v1's whole egress stack); the guest
    // task is suspended meanwhile, not spinning.
    let rsp =
        wasmtime_wasi::runtime::spawn_blocking(move || requester.do_request(&wreq, deadline)).await;

    if !rsp.error.is_empty() {
        // What v1 told the guest in-band travels as the error's payload -
        // the refusal wording is contract, and no other wasi:http code
        // carries a reason.
        return Err(HttpError::InternalError(Some(rsp.error)));
    }

    let body = base64::engine::general_purpose::STANDARD
        .decode(&rsp.body)
        .map_err(|e| {
            HttpError::InternalError(Some(format!("cannot decode the response body: {e}")))
        })?;
    let mut builder = http::Response::builder().status(rsp.status as u16);
    for (name, values) in &rsp.headers {
        for value in values {
            builder = builder.header(name, value);
        }
    }
    let response = builder
        .body(
            http_body_util::Full::new(bytes::Bytes::from(body))
                .map_err(|e| match e {})
                .boxed_unsync(),
        )
        .map_err(|e| HttpError::InternalError(Some(format!("cannot build the response: {e}"))))?;
    Ok((
        response,
        Box::new(async { Ok(()) })
            as Box<dyn Future<Output = wasmtime_wasi_http::Result<()>> + Send>,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A requester recording what it was asked and answering from a script.
    struct Fake {
        asked: Mutex<Vec<wire::Request>>,
        answer: wire::Response,
    }

    impl crate::HttpRequester for Fake {
        fn do_request(&self, req: &wire::Request, _deadline: Instant) -> wire::Response {
            self.asked.lock().expect("lock").push(wire::Request {
                method: req.method.clone(),
                url: req.url.clone(),
                headers: req.headers.clone(),
                body: req.body.clone(),
            });
            wire::Response {
                status: self.answer.status,
                headers: self.answer.headers.clone(),
                body: self.answer.body.clone(),
                error: self.answer.error.clone(),
            }
        }
    }

    fn request(body: &[u8]) -> http::Request<WasiBody> {
        http::Request::builder()
            .method("POST")
            .uri("https://api.example.com/greet?x=1")
            .header("x-a", "1")
            .body(
                http_body_util::Full::new(bytes::Bytes::copy_from_slice(body))
                    .map_err(|e| match e {})
                    .boxed_unsync(),
            )
            .expect("request")
    }

    fn drive(hooks: &mut EgressHooks, req: http::Request<WasiBody>) -> SendResult {
        wasmtime_wasi::runtime::in_tokio(Box::into_pin(hooks.send_request(
            req,
            None,
            Box::new(async { Ok(()) }),
        )))
    }

    #[test]
    fn no_grant_refuses_with_the_v1_wording() {
        let mut hooks = EgressHooks::new(
            None,
            Instant::now() + std::time::Duration::from_secs(1),
            "module file fn.wasm".to_string(),
            "sha256:abc".to_string(),
            Arc::new(AtomicU64::new(0)),
        );
        match drive(&mut hooks, request(b"")) {
            Ok(_) => panic!("should be refused"),
            Err(HttpError::InternalError(Some(msg))) => assert_eq!(msg, NO_EGRESS),
            Err(other) => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_granted_send_round_trips_through_the_requester() {
        let fake = Arc::new(Fake {
            asked: Mutex::new(Vec::new()),
            answer: wire::Response {
                status: 200,
                headers: BTreeMap::from([("x-b".to_string(), vec!["2".to_string()])]),
                body: base64::engine::general_purpose::STANDARD.encode(b"howdy"),
                error: String::new(),
            },
        });
        let counted = Arc::new(AtomicU64::new(0));
        let mut hooks = EgressHooks::new(
            Some(fake.clone() as Arc<dyn crate::HttpRequester>),
            Instant::now() + std::time::Duration::from_secs(1),
            "m".to_string(),
            "d".to_string(),
            Arc::clone(&counted),
        );
        let (rsp, _io) = drive(&mut hooks, request(b"hello")).expect("send");
        assert_eq!(rsp.status(), 200);
        assert_eq!(rsp.headers().get("x-b").expect("header"), "2");
        let body = wasmtime_wasi::runtime::in_tokio(async {
            rsp.into_body().collect().await.expect("body").to_bytes()
        });
        assert_eq!(&body[..], b"howdy");

        let asked = fake.asked.lock().expect("lock");
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].method, "POST");
        assert_eq!(asked[0].url, "https://api.example.com/greet?x=1");
        assert_eq!(asked[0].headers["x-a"], vec!["1".to_string()]);
        assert_eq!(
            asked[0].body,
            base64::engine::general_purpose::STANDARD.encode(b"hello")
        );
        assert!(counted.load(Ordering::Relaxed) > 0, "the wait was counted");
    }

    #[test]
    fn an_in_band_error_travels_as_internal_error() {
        let fake = Arc::new(Fake {
            asked: Mutex::new(Vec::new()),
            answer: wire::Response::refusal(
                "sandbox.egress: api.example.com resolves to an address the egress policy blocks",
            ),
        });
        let mut hooks = EgressHooks::new(
            Some(fake as Arc<dyn crate::HttpRequester>),
            Instant::now() + std::time::Duration::from_secs(1),
            "m".to_string(),
            "d".to_string(),
            Arc::new(AtomicU64::new(0)),
        );
        match drive(&mut hooks, request(b"")) {
            Ok(_) => panic!("should be blocked"),
            Err(HttpError::InternalError(Some(msg))) => assert_eq!(
                msg,
                "sandbox.egress: api.example.com resolves to an address the egress policy blocks"
            ),
            Err(other) => panic!("unexpected: {other:?}"),
        }
    }
}
