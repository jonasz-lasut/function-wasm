//! The gRPC server metrics of the Go runtime, re-created over the raw-codec
//! transport: `grpc_server_started_total`, `grpc_server_handled_total`,
//! `grpc_server_msg_received_total` and `grpc_server_msg_sent_total`, with
//! the names, labels and help strings of function-sdk-go's grpc-prometheus
//! interceptor, so dashboards and alerts built on the Go runtime keep
//! working. Like that interceptor - which was unary-only - streaming
//! methods (reflection, health Watch) are not counted, and like its
//! InitializeMetrics, every method the server carries starts as a zero
//! series. No handling-time histogram: function-sdk-go never enabled one,
//! and `function_wasm_module_run_duration_seconds` covers latency.

use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll};

use function_wasm_engine::metrics::LabeledCounter;

const RUN_FUNCTION_SERVICE: &str = "apiextensions.fn.proto.v1.FunctionRunnerService";
const HEALTH_SERVICE: &str = "grpc.health.v1.Health";

static STARTED: LazyLock<LabeledCounter> = LazyLock::new(|| {
    LabeledCounter::new(
        "grpc_server_started",
        "Total number of RPCs started on the server.",
        &["grpc_type", "grpc_service", "grpc_method"],
    )
});

static HANDLED: LazyLock<LabeledCounter> = LazyLock::new(|| {
    LabeledCounter::new(
        "grpc_server_handled",
        "Total number of RPCs completed on the server, regardless of success or failure.",
        &["grpc_type", "grpc_service", "grpc_method", "grpc_code"],
    )
});

static MSG_RECEIVED: LazyLock<LabeledCounter> = LazyLock::new(|| {
    LabeledCounter::new(
        "grpc_server_msg_received",
        "Total number of RPC stream messages received on the server.",
        &["grpc_type", "grpc_service", "grpc_method"],
    )
});

static MSG_SENT: LazyLock<LabeledCounter> = LazyLock::new(|| {
    LabeledCounter::new(
        "grpc_server_msg_sent",
        "Total number of gRPC stream messages sent by the server.",
        &["grpc_type", "grpc_service", "grpc_method"],
    )
});

/// The gRPC status code names, indexed by their wire value - the strings
/// google.golang.org/grpc/codes renders, which is what the Go runtime's
/// grpc_code label carried.
const CODES: [&str; 17] = [
    "OK",
    "Canceled",
    "Unknown",
    "InvalidArgument",
    "DeadlineExceeded",
    "NotFound",
    "AlreadyExists",
    "PermissionDenied",
    "ResourceExhausted",
    "FailedPrecondition",
    "Aborted",
    "OutOfRange",
    "Unimplemented",
    "Internal",
    "Unavailable",
    "DataLoss",
    "Unauthenticated",
];

fn code_str(n: u32) -> &'static str {
    CODES.get(n as usize).copied().unwrap_or("Unknown")
}

/// The unary methods the server carries - the only calls the Go
/// interceptor counted (it installed no stream interceptor).
fn unary_method(path: &str) -> Option<(&'static str, &'static str)> {
    match path {
        "/apiextensions.fn.proto.v1.FunctionRunnerService/RunFunction" => {
            Some((RUN_FUNCTION_SERVICE, "RunFunction"))
        }
        "/grpc.health.v1.Health/Check" => Some((HEALTH_SERVICE, "Check")),
        _ => None,
    }
}

/// Pre-creates every series the server can emit at zero, the way
/// grpc-prometheus's InitializeMetrics did for the Go runtime: a scrape
/// sees each method of each registered service from the first request on,
/// streaming methods included (present and forever zero, exactly as under
/// the Go runtime's unary-only interceptor).
pub fn initialize() {
    let methods: [(&str, &str, &str); 5] = [
        ("unary", RUN_FUNCTION_SERVICE, "RunFunction"),
        ("unary", HEALTH_SERVICE, "Check"),
        ("server_stream", HEALTH_SERVICE, "Watch"),
        (
            "bidi_stream",
            "grpc.reflection.v1.ServerReflection",
            "ServerReflectionInfo",
        ),
        (
            "bidi_stream",
            "grpc.reflection.v1alpha.ServerReflection",
            "ServerReflectionInfo",
        ),
    ];
    for (kind, service, method) in methods {
        let labels = [kind, service, method];
        let _ = STARTED.with_label_values(&labels);
        let _ = MSG_RECEIVED.with_label_values(&labels);
        let _ = MSG_SENT.with_label_values(&labels);
        for code in CODES {
            let _ = HANDLED.with_label_values(&[kind, service, method, code]);
        }
    }
}

/// One unary call's bookkeeping: started and msg_received on creation,
/// handled (and msg_sent on OK) exactly once when the status is known. A
/// reporter dropped before any status - the caller went away mid-call - is
/// a Canceled, which is what the Go interceptor reported for a canceled
/// context.
struct Reporter {
    service: &'static str,
    method: &'static str,
    done: bool,
}

impl Reporter {
    fn start(service: &'static str, method: &'static str) -> Self {
        STARTED.with_label_values(&["unary", service, method]).inc();
        MSG_RECEIVED
            .with_label_values(&["unary", service, method])
            .inc();
        Reporter {
            service,
            method,
            done: false,
        }
    }

    fn report(&mut self, code: u32) {
        if self.done {
            return;
        }
        self.done = true;
        HANDLED
            .with_label_values(&["unary", self.service, self.method, code_str(code)])
            .inc();
        if code == 0 {
            MSG_SENT
                .with_label_values(&["unary", self.service, self.method])
                .inc();
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        if !self.done {
            self.done = true;
            HANDLED
                .with_label_values(&["unary", self.service, self.method, "Canceled"])
                .inc();
        }
    }
}

/// Reads a numeric grpc-status from headers or trailers; a gRPC response
/// with no grpc-status is out of spec, so the caller falls back to Unknown.
fn status_in(map: &http::HeaderMap) -> Option<u32> {
    map.get("grpc-status")?.to_str().ok()?.parse().ok()
}

/// The tower layer serve() wraps the whole router in, so the health service
/// is counted exactly as the Go runtime's server-wide interceptor counted
/// it, not only the routes this crate owns.
#[derive(Clone, Copy, Default)]
pub struct MetricsLayer;

impl<S> tower_layer::Layer<S> for MetricsLayer {
    type Service = Metrics<S>;

    fn layer(&self, inner: S) -> Metrics<S> {
        Metrics { inner }
    }
}

#[derive(Clone)]
pub struct Metrics<S> {
    inner: S,
}

impl<S, ReqBody> tower_service::Service<http::Request<ReqBody>> for Metrics<S>
where
    S: tower_service::Service<http::Request<ReqBody>, Response = http::Response<tonic::body::Body>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
{
    type Response = http::Response<MetricsBody>;
    type Error = S::Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let unary = unary_method(req.uri().path());
        // The readied service is the one that must take the call; the clone
        // parks in self (the tower Service contract).
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move {
            let mut reporter = unary.map(|(service, method)| Reporter::start(service, method));
            let rsp = inner.call(req).await?;
            let (parts, body) = rsp.into_parts();
            // A trailers-only response (tonic's immediate refusals) carries
            // grpc-status in the headers; report now and wrap nothing.
            if let (Some(r), Some(code)) = (reporter.as_mut(), status_in(&parts.headers)) {
                r.report(code);
                reporter = None;
            }
            Ok(http::Response::from_parts(
                parts,
                MetricsBody {
                    inner: body,
                    reporter,
                },
            ))
        })
    }
}

/// The response body with the call's reporter riding along: the status of a
/// normal unary response is in the trailers frame, so that is where handled
/// can be counted - and a body dropped before its trailers is the client
/// going away, the reporter's Canceled.
pub struct MetricsBody {
    inner: tonic::body::Body,
    reporter: Option<Reporter>,
}

impl http_body::Body for MetricsBody {
    type Data = bytes::Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match std::task::ready!(Pin::new(&mut this.inner).poll_frame(cx)) {
            Some(Ok(frame)) => {
                if let (Some(trailers), Some(r)) = (frame.trailers_ref(), this.reporter.as_mut()) {
                    r.report(status_in(trailers).unwrap_or(2));
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Err(e)) => {
                if let Some(r) = this.reporter.as_mut() {
                    r.report(2);
                }
                Poll::Ready(Some(Err(e)))
            }
            None => {
                // A stream that ends without trailers never carried a
                // status; out of spec, counted as Unknown like a broken
                // response would be.
                if let Some(r) = this.reporter.as_mut() {
                    r.report(2);
                }
                Poll::Ready(None)
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::Body::size_hint(&self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_names_match_grpc_go() {
        assert_eq!(code_str(0), "OK");
        assert_eq!(code_str(1), "Canceled");
        assert_eq!(code_str(12), "Unimplemented");
        assert_eq!(code_str(16), "Unauthenticated");
        assert_eq!(code_str(17), "Unknown");
    }

    #[test]
    fn only_unary_methods_are_counted() {
        assert_eq!(
            unary_method("/apiextensions.fn.proto.v1.FunctionRunnerService/RunFunction"),
            Some((RUN_FUNCTION_SERVICE, "RunFunction"))
        );
        assert_eq!(
            unary_method("/grpc.health.v1.Health/Check"),
            Some((HEALTH_SERVICE, "Check"))
        );
        // Streaming methods stay uncounted, as under the Go runtime's
        // unary-only interceptor.
        assert_eq!(unary_method("/grpc.health.v1.Health/Watch"), None);
        assert_eq!(
            unary_method("/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"),
            None
        );
    }

    #[test]
    fn a_dropped_reporter_is_a_canceled_call() {
        initialize();
        let canceled = || {
            function_wasm_engine::metrics::sample(
                "grpc_server_handled_total",
                &[
                    ("grpc_type", "unary"),
                    ("grpc_service", RUN_FUNCTION_SERVICE),
                    ("grpc_method", "RunFunction"),
                    ("grpc_code", "Canceled"),
                ],
            )
            .unwrap_or(0.0)
        };
        let before = canceled();
        drop(Reporter::start(RUN_FUNCTION_SERVICE, "RunFunction"));
        assert_eq!(canceled(), before + 1.0);
        // A reported call does not double-count on drop.
        let mut r = Reporter::start(RUN_FUNCTION_SERVICE, "RunFunction");
        r.report(0);
        drop(r);
        assert_eq!(canceled(), before + 1.0);
    }
}
