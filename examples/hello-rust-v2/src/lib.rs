//! The hello-rust-v2 guest: a Crossplane composition function compiled to a
//! WebAssembly component implementing ABI v2 (docs/abi-v2.md).
//! `run_function` is ordinary async Rust over the protobuf messages prost
//! generated from the vendored crossplane proto; `bindings` holds the
//! wit-bindgen world (the `run` export, the typed `log` import and the
//! async `wasi:http/client` fetch) and only exists on the wasm target, so
//! the crate also builds and tests natively. There is no hand-written ABI
//! glue: the canonical ABI owns what an ABI v1 guest carries by hand.

use std::collections::BTreeMap;
use std::future::Future;

use prost::Message;
use prost_types::value::Kind;
use prost_types::{Duration, Struct, Value};

#[cfg(target_arch = "wasm32")]
pub mod bindings;

/// The crossplane `RunFunction` messages.
pub mod fnv1 {
    // prost copies the proto comments verbatim; their list formatting is not
    // rustdoc's, which is nothing to fix here.
    #![allow(clippy::doc_lazy_continuation)]
    // `fn` is a Rust keyword, so prost escapes that segment of the proto package.
    include!(concat!(env!("OUT_DIR"), "/apiextensions.r#fn.proto.v1.rs"));
}

use fnv1::{
    Condition, RequestMeta, ResponseMeta, Result as FnResult, RunFunctionRequest,
    RunFunctionResponse, Severity, Status, Target,
};

const DEFAULT_TTL_SECONDS: i64 = 60;

/// Adds a ConfigMap greeting the composite resource to the desired state.
/// `fetch` resolves config.greetingUrl - the async wasi:http client on the
/// wasm target, a test double natively.
pub async fn run_function<F, Fut>(
    req: &RunFunctionRequest,
    fetch: F,
) -> Result<RunFunctionResponse, String>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let tag = req
        .meta
        .as_ref()
        .map(|m: &RequestMeta| m.tag.clone())
        .unwrap_or_default();
    log::info("Running function", &[("tag", &tag)]);

    let mut greeting = match config_string(req, "greeting") {
        Ok(Some(g)) => g,
        Ok(None) => "hello".to_string(),
        Err(e) => return Err(format!("cannot read config: {e}")),
    };
    // greetingUrl fetches the greeting through the host instead — the
    // requires.egress grant of the module's manifest decides whether it may.
    match config_string(req, "greetingUrl") {
        Ok(Some(url)) => {
            greeting = fetch(url)
                .await
                .map_err(|e| format!("cannot fetch greeting: {e}"))?;
        }
        Ok(None) => {}
        Err(e) => return Err(format!("cannot read config: {e}")),
    }

    let name = req
        .observed
        .as_ref()
        .and_then(|s| s.composite.as_ref())
        .and_then(|c| c.resource.as_ref())
        .ok_or("cannot get observed composite resource: none in request")?
        .fields
        .get("metadata")
        .and_then(struct_value)
        .and_then(|m| m.fields.get("name"))
        .and_then(string_value)
        .unwrap_or_default();

    let cm = fields(vec![
        ("apiVersion", string("v1")),
        ("kind", string("ConfigMap")),
        (
            "data",
            object(vec![("greeting", string(&format!("{greeting} {name}")))]),
        ),
    ]);

    let mut desired = req.desired.clone().unwrap_or_default();
    desired.resources.insert(
        "greeting".to_string(),
        fnv1::Resource {
            resource: Some(cm),
            ..Default::default()
        },
    );

    Ok(RunFunctionResponse {
        meta: Some(ResponseMeta {
            tag,
            ttl: Some(Duration {
                seconds: DEFAULT_TTL_SECONDS,
                nanos: 0,
            }),
        }),
        desired: Some(desired),
        results: vec![FnResult {
            severity: Severity::Normal as i32,
            message: format!("greeted {name}"),
            target: Some(Target::Composite as i32),
            ..Default::default()
        }],
        conditions: vec![Condition {
            r#type: "FunctionSuccess".to_string(),
            status: Status::ConditionTrue as i32,
            reason: "Success".to_string(),
            target: Some(Target::CompositeAndClaim as i32),
            ..Default::default()
        }],
        ..Default::default()
    })
}

/// Reads a string field of the Input's `config` block.
fn config_string(req: &RunFunctionRequest, key: &str) -> Result<Option<String>, String> {
    let Some(cfg) = req
        .input
        .as_ref()
        .and_then(|i| i.fields.get("config"))
        .and_then(struct_value)
    else {
        return Ok(None);
    };
    match cfg.fields.get(key) {
        None => Ok(None),
        Some(v) => string_value(v)
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| format!("{key} must be a string")),
    }
}

fn struct_value(v: &Value) -> Option<&Struct> {
    match &v.kind {
        Some(Kind::StructValue(s)) => Some(s),
        _ => None,
    }
}

fn string_value(v: &Value) -> Option<&str> {
    match &v.kind {
        Some(Kind::StringValue(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn string(s: &str) -> Value {
    Value {
        kind: Some(Kind::StringValue(s.to_string())),
    }
}

fn fields(fields: Vec<(&str, Value)>) -> Struct {
    Struct {
        fields: fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn object(entries: Vec<(&str, Value)>) -> Value {
    Value {
        kind: Some(Kind::StructValue(fields(entries))),
    }
}

/// Decode, run, encode. Every failure becomes a fatal result so the host can
/// always decode the reply - the world's `err(string)` channel stays for
/// failures that happen before a response can be built, which this guest
/// never has.
pub async fn handle<F, Fut>(input: &[u8], fetch: F) -> Vec<u8>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let req = match RunFunctionRequest::decode(input) {
        Ok(req) => req,
        Err(e) => {
            return fatal(None, &format!("cannot decode RunFunctionRequest: {e}")).encode_to_vec();
        }
    };
    match run_function(&req, fetch).await {
        Ok(rsp) => rsp.encode_to_vec(),
        Err(e) => fatal(Some(&req), &e).encode_to_vec(),
    }
}

fn fatal(req: Option<&RunFunctionRequest>, msg: &str) -> RunFunctionResponse {
    RunFunctionResponse {
        meta: req.map(|r| ResponseMeta {
            tag: r.meta.as_ref().map(|m| m.tag.clone()).unwrap_or_default(),
            ttl: Some(Duration {
                seconds: DEFAULT_TTL_SECONDS,
                nanos: 0,
            }),
        }),
        results: vec![FnResult {
            severity: Severity::Fatal as i32,
            message: msg.to_string(),
            target: Some(Target::Composite as i32),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Structured logging through the host: the world's typed `log` import on
/// the wasm target, stderr elsewhere.
pub mod log {
    pub fn info(msg: &str, kv: &[(&str, &str)]) {
        emit(false, msg, kv);
    }

    #[cfg(target_arch = "wasm32")]
    fn emit(debug: bool, msg: &str, kv: &[(&str, &str)]) {
        let level = if debug {
            crate::bindings::LogLevel::Debug
        } else {
            crate::bindings::LogLevel::Info
        };
        let kv: Vec<(String, String)> = kv
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        crate::bindings::log(level, msg, &kv);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn emit(_debug: bool, msg: &str, kv: &[(&str, &str)]) {
        eprintln!("{msg} {kv:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The native fetch double: greetings.example.com answers, anything else
    /// is refused the way the runtime words a policy refusal.
    async fn fake_fetch(url: String) -> Result<String, String> {
        if url == "https://greetings.example.com/en" {
            Ok("howdy".to_string())
        } else {
            Err(format!(
                "internal-error: sandbox.egress: no rule admits host {:?}",
                url
            ))
        }
    }

    fn run(req: &RunFunctionRequest) -> Result<RunFunctionResponse, String> {
        pollster::block_on(run_function(req, fake_fetch))
    }

    fn xr(name: &str) -> fnv1::State {
        fnv1::State {
            composite: Some(fnv1::Resource {
                resource: Some(fields(vec![
                    ("apiVersion", string("example.org/v1")),
                    ("kind", string("XR")),
                    ("metadata", object(vec![("name", string(name))])),
                ])),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn input(config: Option<Value>) -> Struct {
        let mut entries = vec![
            ("apiVersion", string("wasm.fn.crossplane.io/v1beta1")),
            ("kind", string("Input")),
        ];
        if let Some(c) = config {
            entries.push(("config", c));
        }
        fields(entries)
    }

    fn greeting_of(rsp: &RunFunctionResponse) -> String {
        let cm = rsp.desired.as_ref().unwrap().resources["greeting"]
            .resource
            .as_ref()
            .unwrap();
        let data = struct_value(&cm.fields["data"]).unwrap();
        string_value(&data.fields["greeting"]).unwrap().to_string()
    }

    #[test]
    fn default_greeting() {
        let req = RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "hello".into(),
                ..Default::default()
            }),
            observed: Some(xr("my-xr")),
            ..Default::default()
        };
        let rsp = run(&req).unwrap();
        assert_eq!(greeting_of(&rsp), "hello my-xr");
        assert_eq!(rsp.meta.as_ref().unwrap().tag, "hello");
        assert_eq!(rsp.results[0].message, "greeted my-xr");
        assert_eq!(rsp.conditions[0].r#type, "FunctionSuccess");
    }

    #[test]
    fn configured_greeting_keeps_desired() {
        let mut desired = fnv1::State::default();
        desired
            .resources
            .insert("other".into(), fnv1::Resource::default());
        let req = RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "hello".into(),
                ..Default::default()
            }),
            input: Some(input(Some(object(vec![("greeting", string("hi"))])))),
            observed: Some(xr("my-xr")),
            desired: Some(desired),
            ..Default::default()
        };
        let rsp = run(&req).unwrap();
        assert_eq!(greeting_of(&rsp), "hi my-xr");
        assert!(rsp
            .desired
            .as_ref()
            .unwrap()
            .resources
            .contains_key("other"));
    }

    #[test]
    fn bad_config_is_an_error() {
        let req = RunFunctionRequest {
            input: Some(input(Some(object(vec![(
                "greeting",
                Value {
                    kind: Some(Kind::NumberValue(7.0)),
                },
            )])))),
            observed: Some(xr("my-xr")),
            ..Default::default()
        };
        assert_eq!(
            run(&req).unwrap_err(),
            "cannot read config: greeting must be a string"
        );
    }

    #[test]
    fn greeting_from_url_through_the_fetcher() {
        let req = |url: &str| RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "hello".into(),
                ..Default::default()
            }),
            input: Some(input(Some(object(vec![("greetingUrl", string(url))])))),
            observed: Some(xr("my-xr")),
            ..Default::default()
        };
        let rsp = run(&req("https://greetings.example.com/en")).unwrap();
        assert_eq!(greeting_of(&rsp), "howdy my-xr");
        assert_eq!(
            run(&req("https://evil.example.com/en")).unwrap_err(),
            "cannot fetch greeting: internal-error: sandbox.egress: no rule admits host \"https://evil.example.com/en\""
        );
    }

    #[test]
    fn handle_round_trip_reports_fatal() {
        let req = RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let rsp = RunFunctionResponse::decode(
            pollster::block_on(handle(&req.encode_to_vec(), fake_fetch)).as_slice(),
        )
        .unwrap();
        assert_eq!(rsp.meta.as_ref().unwrap().tag, "t");
        assert_eq!(rsp.results[0].severity, Severity::Fatal as i32);
        assert_eq!(
            rsp.results[0].message,
            "cannot get observed composite resource: none in request"
        );
    }
}
