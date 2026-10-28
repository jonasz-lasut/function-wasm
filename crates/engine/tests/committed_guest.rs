//! The end-to-end suite's committed test module (crates/e2e/guest/fn.wasm,
//! an ABI v2 component on wasi:http@0.3) as an engine fixture: a real guest
//! that makes an HTTP request through the host, driven here with a fake
//! requester, so the egress seam is covered on every push with no toolchain
//! (issue #150). The guest GETs the URL its composite resource's spec.egress
//! names and writes the status and body, or the host's refusal, into the
//! composite's status.

use std::sync::Arc;
use std::time::{Duration, Instant};

use function_sdk_rust::proto::v1::{Resource, RunFunctionRequest, RunFunctionResponse, State};
use function_wasm_engine::wire::{HttpRequester, Request, Response};
use function_wasm_engine::{Config, Engine, Module, RunOptions};
use prost::Message;

const GUEST: &[u8] = include_bytes!("../../e2e/guest/fn.wasm");

fn engine() -> Engine {
    Engine::new(Config::default()).expect("engine")
}

fn guest(e: &Engine) -> Module {
    e.compile(GUEST).expect("the committed guest compiles")
}

/// A request whose composite resource asks the guest to GET url.
fn request(url: &str) -> Vec<u8> {
    let xr = serde_json::json!({
        "apiVersion": "e2e.function-wasm.io/v1alpha1",
        "kind": "Guest",
        "metadata": {"name": "probe", "namespace": "e2e"},
        "spec": {"nopResources": 0, "egress": {"url": url}},
    });
    RunFunctionRequest {
        observed: Some(State {
            composite: Some(Resource {
                resource: Some(serde_json::from_value(xr).expect("a Struct")),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec()
}

/// What the guest wrote under status.egress.
fn egress_status(response: &[u8]) -> serde_json::Value {
    let rsp = RunFunctionResponse::decode(response).expect("a RunFunctionResponse");
    let fatal: Vec<_> = rsp.results.iter().filter(|r| r.severity == 1).collect();
    assert!(
        fatal.is_empty(),
        "the guest returned a fatal result: {fatal:?}"
    );
    let xr = rsp
        .desired
        .and_then(|d| d.composite)
        .and_then(|c| c.resource)
        .expect("desired composite");
    serde_json::to_value(&xr).expect("json")["status"]["egress"].clone()
}

/// A requester answering after a delay, with what it is given.
struct Slow {
    delay: Duration,
    answer: fn(&Request) -> Response,
}

impl HttpRequester for Slow {
    fn do_request(&self, req: &Request, _deadline: Instant) -> Response {
        std::thread::sleep(self.delay);
        (self.answer)(req)
    }
}

fn ok_body(req: &Request) -> Response {
    assert_eq!(req.method, "GET");
    Response {
        status: 200,
        body: format!("answered {}", req.url).into_bytes(),
        ..Default::default()
    }
}

/// No grant: the guest's send gets the in-band refusal with the contract
/// wording, as an error it reads, never a trap, and the run completes.
#[test]
fn without_a_grant_the_refusal_reaches_the_guest_in_band() {
    let e = engine();
    let m = guest(&e);
    let out = e
        .run(
            &m,
            &request("https://api.example.com/v1/x"),
            RunOptions::default(),
        )
        .expect("the run completes");
    let egress = egress_status(&out);
    let error = egress["error"].as_str().expect("an in-band error");
    assert!(
        error.contains("sandbox.egress: HTTP egress is not granted to this module"),
        "{error}"
    );
    assert!(egress["status"].is_null(), "{egress}");
}

/// The policy client's refusal travels as the error's payload, word for word.
#[test]
fn a_refusal_from_the_requester_reaches_the_guest_in_band() {
    let e = engine();
    let m = guest(&e);
    let opts = RunOptions {
        http: Some(Arc::new(Slow {
            delay: Duration::ZERO,
            answer: |req| {
                Response::refusal(format!("sandbox.egress: no rule admits host {:?}", req.url))
            },
        })),
        ..Default::default()
    };
    let out = e
        .run(&m, &request("https://api.example.com/v1/x"), opts)
        .expect("the run completes");
    let egress = egress_status(&out);
    let error = egress["error"].as_str().expect("an in-band error");
    assert!(
        error.contains("sandbox.egress: no rule admits host"),
        "{error}"
    );
}

/// A granted send: the requester's answer, status and body, reaches the guest.
#[test]
fn a_granted_send_returns_the_answer() {
    let e = engine();
    let m = guest(&e);
    let opts = RunOptions {
        http: Some(Arc::new(Slow {
            delay: Duration::ZERO,
            answer: ok_body,
        })),
        ..Default::default()
    };
    let out = e
        .run(&m, &request("https://api.example.com/v1/x"), opts)
        .expect("the run completes");
    let egress = egress_status(&out);
    assert_eq!(egress["status"].as_f64(), Some(200.0), "{egress}");
    assert_eq!(
        egress["body"], "answered https://api.example.com/v1/x",
        "{egress}"
    );
}

/// Time blocked in wasi:http is host time, credited back to the compute
/// deadline: a run with a 100 ms budget survives a 400 ms answer.
#[test]
fn an_http_wait_does_not_consume_the_deadline() {
    let e = engine();
    let m = guest(&e);
    let opts = RunOptions {
        timeout: Some(Duration::from_millis(100)),
        http: Some(Arc::new(Slow {
            delay: Duration::from_millis(400),
            answer: ok_body,
        })),
        ..Default::default()
    };
    let out = e
        .run(&m, &request("https://api.example.com/v1/slow"), opts)
        .expect("the wait is credited back, the run completes");
    assert_eq!(egress_status(&out)["status"].as_f64(), Some(200.0));
}
