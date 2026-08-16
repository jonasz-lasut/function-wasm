//! The my-fn guest: a Crossplane composition function compiled to a
//! wasip1 reactor and run by function-wasm. It composes a ConfigMap greeting
//! the composite resource in a ~150 KB module.
//!
//! `run_function` is ordinary Rust over the protobuf messages prost generated
//! from the vendored crossplane proto; `abi` implements the function-wasm ABI
//! (`wasmfn_alloc`, `wasmfn_run`, the `wasmfn.log` import) and only exists on
//! the wasi target so the crate also builds and tests natively; `http` is
//! egress through the host (`wasmfn.http`), granted per Composition.

pub mod http;

use std::collections::BTreeMap;

use prost::Message;
use prost_types::value::Kind;
use prost_types::{Duration, Struct, Value};

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
pub fn run_function(req: &RunFunctionRequest) -> Result<RunFunctionResponse, String> {
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
    // sandbox.egress grant of the Composition decides whether it may.
    match config_string(req, "greetingUrl") {
        Ok(Some(url)) => {
            greeting = http::get_text(&url).map_err(|e| format!("cannot fetch greeting: {e}"))?;
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

/// The guest half of ABI v1: decode, run, encode. Every failure becomes a
/// fatal result so the host can always decode the reply.
pub fn handle(input: &[u8]) -> Vec<u8> {
    let req = match RunFunctionRequest::decode(input) {
        Ok(req) => req,
        Err(e) => {
            return fatal(None, &format!("cannot decode RunFunctionRequest: {e}")).encode_to_vec()
        }
    };
    match run_function(&req) {
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

/// Structured logging through the host (`wasmfn.log`); prints to stderr when
/// not running under function-wasm.
pub mod log {
    pub fn info(msg: &str, kv: &[(&str, &str)]) {
        let mut payload = format!("{{\"msg\":{},\"kv\":[", json_string(msg));
        for (i, (k, v)) in kv.iter().enumerate() {
            if i > 0 {
                payload.push(',');
            }
            payload.push_str(&format!("{},{}", json_string(k), json_string(v)));
        }
        payload.push_str("]}");
        emit(0, payload.as_bytes());
    }

    fn json_string(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    #[cfg(target_os = "wasi")]
    fn emit(level: u32, payload: &[u8]) {
        #[link(wasm_import_module = "wasmfn")]
        extern "C" {
            fn log(level: u32, ptr: u32, len: u32);
        }
        // SAFETY: the host reads len bytes at ptr during the call and never
        // keeps the pointer.
        unsafe { log(level, payload.as_ptr() as u32, payload.len() as u32) }
    }

    #[cfg(not(target_os = "wasi"))]
    fn emit(_level: u32, payload: &[u8]) {
        eprintln!("{}", String::from_utf8_lossy(payload));
    }
}

/// The function-wasm ABI v1 exports, only on the wasi target.
#[cfg(target_os = "wasi")]
pub(crate) mod abi {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        // Buffers the host reads or writes through raw pointers: inputs handed
        // out by wasmfn_alloc until wasmfn_run consumes them, responses the
        // host wrote through wasmfn_alloc inside a wasmfn.http call until the
        // guest takes them, and the last response until the next call.
        static BUFFERS: RefCell<HashMap<u32, Vec<u8>>> = RefCell::new(HashMap::new());
    }

    fn pin(buf: Vec<u8>) -> u32 {
        let ptr = buf.as_ptr() as u32;
        BUFFERS.with(|b| b.borrow_mut().insert(ptr, buf));
        ptr
    }

    /// Takes a buffer the host filled through wasmfn_alloc (a wasmfn.http
    /// response) out of the pinned set.
    pub(crate) fn take(ptr: u32) -> Option<Vec<u8>> {
        BUFFERS.with(|b| b.borrow_mut().remove(&ptr))
    }

    #[no_mangle]
    pub extern "C" fn wasmfn_alloc(size: u32) -> u32 {
        pin(vec![0u8; size as usize])
    }

    #[no_mangle]
    pub extern "C" fn wasmfn_run(ptr: u32, len: u32) -> u64 {
        let input = BUFFERS.with(|b| b.borrow_mut().remove(&ptr));
        let out = match input {
            Some(buf) if buf.len() >= len as usize => super::handle(&buf[..len as usize]),
            _ => super::fatal(
                None,
                "wasmfn_run: input was not allocated with wasmfn_alloc",
            )
            .encode_to_vec(),
        };
        BUFFERS.with(|b| b.borrow_mut().clear());
        let n = out.len() as u32;
        ((pin(out) as u64) << 32) | n as u64
    }

    use prost::Message;
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let rsp = run_function(&req).unwrap();
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
        let rsp = run_function(&req).unwrap();
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
            run_function(&req).unwrap_err(),
            "cannot read config: greeting must be a string"
        );
    }

    #[test]
    fn greeting_from_url_through_the_host() {
        // A fake host for wasmfn.http: greetings.example.com answers, anything
        // else is refused the way the runtime words it.
        http::set_host(|payload| {
            let req: serde_json::Value = serde_json::from_slice(payload).unwrap();
            let get = req["method"].as_str().is_none_or(|m| m == "GET");
            if get && req["url"] == "https://greetings.example.com/en" {
                return Ok(br#"{"status":200,"headers":{"Content-Type":["text/plain"]},"body":"aG93ZHkK"}"#.to_vec());
            }
            Ok(br#"{"status":0,"error":"sandbox.egress: no rule admits host \"evil.example.com\""}"#.to_vec())
        });
        let req = |url: &str| RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "hello".into(),
                ..Default::default()
            }),
            input: Some(input(Some(object(vec![("greetingUrl", string(url))])))),
            observed: Some(xr("my-xr")),
            ..Default::default()
        };
        let rsp = run_function(&req("https://greetings.example.com/en")).unwrap();
        assert_eq!(greeting_of(&rsp), "howdy my-xr");
        assert_eq!(
            run_function(&req("https://evil.example.com/en")).unwrap_err(),
            "cannot fetch greeting: sandbox.egress: no rule admits host \"evil.example.com\""
        );
    }

    #[test]
    fn http_codec_round_trip() {
        http::set_host(|payload| {
            let req: serde_json::Value = serde_json::from_slice(payload).unwrap();
            assert_eq!(req["method"], "POST");
            assert_eq!(req["headers"]["Accept"][0], "application/json");
            assert_eq!(req["body"], "eyJuYW1lIjoieCJ9"); // {"name":"x"}
            Ok(br#"{"status":503,"body":"bm8="}"#.to_vec())
        });
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Accept".to_string(), vec!["application/json".to_string()]);
        let rsp = http::send(&http::Request {
            method: "POST".into(),
            url: "https://api.example.com/v1/items".into(),
            headers,
            body: br#"{"name":"x"}"#.to_vec(),
        })
        .unwrap();
        assert_eq!(rsp.status, 503);
        assert_eq!(rsp.body, b"no");
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
        let rsp = RunFunctionResponse::decode(handle(&req.encode_to_vec()).as_slice()).unwrap();
        assert_eq!(rsp.meta.as_ref().unwrap().tag, "t");
        assert_eq!(rsp.results[0].severity, Severity::Fatal as i32);
        assert_eq!(
            rsp.results[0].message,
            "cannot get observed composite resource: none in request"
        );
    }
}
