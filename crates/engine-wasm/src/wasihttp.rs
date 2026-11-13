//! The wasi:http host (docs/abi-v2.md "HTTP egress"): 0.3's `client.send`
//! and 0.2's `outgoing-handler.handle` bridged onto the egress seam
//! (wire.rs) - the HttpRequester behind it applies the grant, the SSRF
//! judgment, the budgets, the rate limit, the audit line and the metric.
//! The crate builds wasmtime-wasi-http without default-send-request, so
//! this implementation is compulsory: a run can never fall through to an
//! unpoliced default client.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Limited};
use wasmtime_wasi_http::{Error as HttpError, RequestOptions, WasiBody, WasiHttpHooks};

use crate::wire::{self, HttpRequester};

/// What a send gets on a run without a grant. (A grant the operator policy
/// does not permit never reaches a run: it is a fatal result before the
/// module runs.)
pub(crate) const NO_EGRESS: &str = "sandbox.egress: HTTP egress is not granted to this module: its manifest requires no egress (requires.egress.http)";

/// The most a request's body may be, the response budget's size: the host
/// buffers a body whole before the policy client sees the request, outside
/// the run's memory limit.
pub(crate) const MAX_REQUEST_BYTES: usize = 4 << 20;

/// How long a run has been blocked in the policy client, counted once
/// however many sends overlap: the time a run's deadline does not charge.
/// A send still in flight counts up to now, so a reader never sees a run
/// that waits on a server as one that merely waits.
#[derive(Default)]
pub(crate) struct HttpWait {
    state: Mutex<WaitState>,
}

#[derive(Default)]
struct WaitState {
    in_flight: u32,
    since: Option<Instant>,
    total: Duration,
}

impl HttpWait {
    pub(crate) fn credited(&self) -> Duration {
        let s = self.state.lock().expect("poisoned");
        s.total + s.since.map(|t| t.elapsed()).unwrap_or_default()
    }

    /// Counts from now until the guard drops, a cancelled send included.
    fn enter(self: &Arc<Self>) -> Waiting {
        let mut s = self.state.lock().expect("poisoned");
        if s.in_flight == 0 {
            s.since = Some(Instant::now());
        }
        s.in_flight += 1;
        Waiting(Arc::clone(self))
    }
}

struct Waiting(Arc<HttpWait>);

impl Drop for Waiting {
    fn drop(&mut self) {
        let mut s = self.0.state.lock().expect("poisoned");
        s.in_flight -= 1;
        if s.in_flight == 0
            && let Some(since) = s.since.take()
        {
            s.total += since.elapsed();
        }
    }
}

/// The per-run state behind the guest's wasi:http imports. Split from the
/// store data because wasmtime-wasi-http borrows the hooks and the resource
/// table from it at once.
pub(crate) struct EgressHooks {
    /// The run's grant; None refuses every send, never a trap.
    requester: Option<Arc<dyn HttpRequester>>,
    /// The run's deadline - each request is capped by what remains of it.
    deadline: Instant,
    module: String,
    digest: String,
    /// One info line for a guest that keeps sending without a grant, then
    /// debug: it is the guest looping, not the host.
    no_grant_logged: bool,
    /// The time this run spent blocked in the policy client - credited
    /// back to the deadline so limits.timeout means guest compute, shared
    /// with the run's deadline.
    http_host: Arc<HttpWait>,
}

impl EgressHooks {
    pub(crate) fn new(
        requester: Option<Arc<dyn HttpRequester>>,
        deadline: Instant,
        module: String,
        digest: String,
        http_host: Arc<HttpWait>,
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
            // code is internal-error, and the refusal string is contract.
            return Box::new(async { Err(HttpError::InternalError(Some(NO_EGRESS.to_string()))) });
        };

        let deadline = self.deadline;
        let http_host = Arc::clone(&self.http_host);
        Box::new(send(requester, request, deadline, http_host))
    }
}

/// One granted send: buffer the outgoing body, hand the request to the
/// policy client on a blocking thread (it resolves, judges, budgets, audits
/// and counts), and map its answer back. Bodies are complete on both sides:
/// the budget acts on whole responses, and a request's body is bounded as
/// it is read.
async fn send(
    requester: Arc<dyn HttpRequester>,
    request: http::Request<WasiBody>,
    deadline: Instant,
    http_host: Arc<HttpWait>,
) -> SendResult {
    let (parts, body) = request.into_parts();
    let collected = Limited::new(body, MAX_REQUEST_BYTES)
        .collect()
        .await
        .map_err(|e| {
            HttpError::InternalError(Some(
                if e.downcast_ref::<http_body_util::LengthLimitError>().is_some() {
                    format!(
                        "sandbox.egress: the request body exceeds {MAX_REQUEST_BYTES} bytes (maxRequestBytes)"
                    )
                } else {
                    format!("cannot read the request body: {e}")
                },
            ))
        })?
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
        body: collected.to_vec(),
    };

    // The policy client is blocking (the whole egress stack is); the guest
    // task is suspended meanwhile, not spinning. Only this wait is credited
    // back to the run's deadline: the guest paces the body it sends, and
    // that time is its own.
    let rsp = {
        let _waiting = http_host.enter();
        wasmtime_wasi::runtime::spawn_blocking(move || requester.do_request(&wreq, deadline)).await
    };

    if !rsp.error.is_empty() {
        // The policy client's reason travels as the error's payload - the
        // refusal wording is contract, and no other wasi:http code carries
        // a reason.
        return Err(HttpError::InternalError(Some(rsp.error)));
    }

    let mut builder = http::Response::builder().status(rsp.status as u16);
    for (name, values) in &rsp.headers {
        for value in values {
            builder = builder.header(name, value);
        }
    }
    let response = builder
        .body(
            http_body_util::Full::new(bytes::Bytes::from(rsp.body))
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

    /// A requester recording what it was asked and answering from a script.
    struct Fake {
        asked: Mutex<Vec<wire::Request>>,
        answer: wire::Response,
    }

    impl HttpRequester for Fake {
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
    fn no_grant_refuses_with_the_contract_wording() {
        let mut hooks = EgressHooks::new(
            None,
            Instant::now() + std::time::Duration::from_secs(1),
            "module file fn.wasm".to_string(),
            "sha256:abc".to_string(),
            Arc::new(HttpWait::default()),
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
                body: b"howdy".to_vec(),
                error: String::new(),
            },
        });
        let counted = Arc::new(HttpWait::default());
        let mut hooks = EgressHooks::new(
            Some(fake.clone() as Arc<dyn HttpRequester>),
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
        assert_eq!(asked[0].body, b"hello");
        assert!(counted.credited() > Duration::ZERO, "the wait was counted");
    }

    #[test]
    fn overlapping_sends_are_credited_once() {
        let wait = Arc::new(HttpWait::default());
        assert_eq!(wait.credited(), Duration::ZERO);
        let first = wait.enter();
        let second = wait.enter();
        std::thread::sleep(Duration::from_millis(40));
        // A send in flight counts up to now.
        let during = wait.credited();
        assert!(during >= Duration::from_millis(40), "{during:?}");
        drop(first);
        drop(second);
        let after = wait.credited();
        assert!(
            after >= during && after < Duration::from_millis(40) * 2,
            "{after:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(wait.credited(), after);
    }

    /// The body is bounded as the host reads it, before the policy client
    /// sees the request: nothing is asked, and the wait credits nothing.
    #[test]
    fn a_request_body_over_the_bound_is_refused() {
        let fake = Arc::new(Fake {
            asked: Mutex::new(Vec::new()),
            answer: wire::Response {
                status: 204,
                ..Default::default()
            },
        });
        let counted = Arc::new(HttpWait::default());
        let mut hooks = EgressHooks::new(
            Some(fake.clone() as Arc<dyn HttpRequester>),
            Instant::now() + Duration::from_secs(5),
            "m".to_string(),
            "d".to_string(),
            Arc::clone(&counted),
        );
        match drive(&mut hooks, request(&vec![0u8; MAX_REQUEST_BYTES + 1])) {
            Ok(_) => panic!("should be refused"),
            Err(HttpError::InternalError(Some(msg))) => assert_eq!(
                msg,
                "sandbox.egress: the request body exceeds 4194304 bytes (maxRequestBytes)"
            ),
            Err(other) => panic!("unexpected: {other:?}"),
        }
        assert!(fake.asked.lock().expect("lock").is_empty());
        assert_eq!(counted.credited(), Duration::ZERO);

        // A body of exactly the bound is sent.
        let _sent = drive(&mut hooks, request(&vec![0u8; MAX_REQUEST_BYTES])).expect("send");
        assert_eq!(fake.asked.lock().expect("lock").len(), 1);
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
            Some(fake as Arc<dyn HttpRequester>),
            Instant::now() + std::time::Duration::from_secs(1),
            "m".to_string(),
            "d".to_string(),
            Arc::new(HttpWait::default()),
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
