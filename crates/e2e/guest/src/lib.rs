//! function-wasm's end-to-end test module (crates/e2e, docs/project/testing.md):
//! an ABI v2 guest whose composite resource's spec says which runtime feature
//! to exercise. It composes provider-nop `NopResource`s, the way Crossplane's
//! own end-to-end suite composes something that reconciles without a cloud,
//! and writes what it observed - the body an egress GET returned, the
//! environment it was given, the credentials it received, whether its private
//! /tmp is writable, the EnvironmentConfig Crossplane resolved for it - into
//! the composite resource's status, where a test reads it. Knobs that end a
//! run (a fatal result, a spin past the deadline, a memory growth past the
//! limit) show up as the condition Crossplane puts on the composite resource.
//!
//! `run_function` is ordinary async Rust over the protobuf messages prost
//! generated from the rust scaffold's vendored proto; `bindings` holds the
//! wit-bindgen world and only exists on the wasm target, so the crate also
//! builds and tests natively.

use std::collections::BTreeMap;
use std::future::Future;

use prost::Message;
use prost_types::value::Kind;
use prost_types::{Duration, ListValue, Struct, Value};
use serde::{Deserialize, Serialize};

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
    resource_selector, RequestMeta, Requirements, ResourceSelector, ResponseMeta,
    Result as FnResult, RunFunctionRequest, RunFunctionResponse, Severity, Target,
};

const TTL_SECONDS: i64 = 60;
/// The module's name, as `status.module` states it.
pub const MODULE: &str = "e2e-guest";
/// provider-nop's managed resource, namespaced in Crossplane v2.
pub const NOP_API_VERSION: &str = "nop.crossplane.io/v1alpha1";
pub const NOP_KIND: &str = "NopResource";
/// What the requirements knob asks Crossplane for.
pub const ENVIRONMENT_CONFIG_API_VERSION: &str = "apiextensions.crossplane.io/v1beta1";
pub const ENVIRONMENT_CONFIG_KIND: &str = "EnvironmentConfig";
/// The key the requirement travels under, in both directions.
pub const REQUIREMENT_KEY: &str = "environment";
/// The file the private /tmp knob writes and reads back.
pub const TMP_PROBE: &str = "/tmp/e2e-probe";

/// What a GET through the host returned.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub status: u16,
    pub body: String,
}

/// The knobs: the composite resource's spec, every field optional.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Spec {
    /// How many NopResources to compose (one when unset).
    #[serde(default = "one")]
    pub nop_resources: u32,
    /// When each NopResource turns Ready, through provider-nop's conditionAfter.
    pub ready_after_seconds: u32,
    /// GET this URL through wasi:http and report status and body.
    pub egress: Option<EgressSpec>,
    /// Report these environment variables.
    pub env: Vec<String>,
    /// Report the keys of these step credentials, as the request carries them.
    pub credentials: Vec<String>,
    /// Write and read back a file under /tmp.
    pub private_tmp: bool,
    /// Ask Crossplane for resources through the response's requirements.
    pub requirements: Option<RequirementsSpec>,
    /// Return a fatal result with this message.
    pub fatal: Option<String>,
    /// Spin until the runtime's deadline stops the run.
    pub spin: bool,
    /// Grow linear memory by this many MiB, reporting where the limit refused it.
    #[serde(rename = "growMiB")]
    pub grow_mib: u32,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct EgressSpec {
    pub url: String,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct RequirementsSpec {
    /// The EnvironmentConfig to request by name.
    pub environment_config: Option<String>,
}

/// What the module observed, written to the composite resource's status.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Status {
    pub module: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressStatus>,
    /// Each requested variable and its value, None when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, Option<String>>>,
    /// Each requested credential and the sorted keys the request carried for
    /// it; an empty list when the request did not carry it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<BTreeMap<String, Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmp: Option<TmpStatus>,
    /// The `data` of the EnvironmentConfig Crossplane resolved, once it has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_config: Option<serde_json::Value>,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct EgressStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The host's refusal or transport error, as the guest read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TmpStatus {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Runs the knobs the composite resource's spec sets and composes its
/// NopResources. `fetch` GETs a URL: the wasi:http client on the wasm target,
/// a test double natively.
pub async fn run_function<F, Fut>(
    req: &RunFunctionRequest,
    fetch: F,
) -> Result<RunFunctionResponse, String>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<Fetched, String>>,
{
    let tag = req
        .meta
        .as_ref()
        .map(|m: &RequestMeta| m.tag.clone())
        .unwrap_or_default();
    let xr = req
        .observed
        .as_ref()
        .and_then(|s| s.composite.as_ref())
        .and_then(|c| c.resource.as_ref())
        .map(struct_to_json)
        .ok_or("cannot get observed composite resource: none in request")?;
    let spec: Spec = serde_json::from_value(xr.get("spec").cloned().unwrap_or_default())
        .map_err(|e| format!("cannot read spec: {e}"))?;
    log::info(
        "Running the e2e guest",
        &[
            ("tag", &tag),
            ("nopResources", &spec.nop_resources.to_string()),
        ],
    );

    if let Some(message) = &spec.fatal {
        return Err(message.clone());
    }
    if spec.spin {
        spin();
    }
    if spec.grow_mib > 0 {
        grow(spec.grow_mib)?;
    }

    let mut status = Status {
        module: MODULE.to_string(),
        ..Default::default()
    };
    if let Some(egress) = &spec.egress {
        status.egress = Some(match fetch(egress.url.clone()).await {
            Ok(f) => EgressStatus {
                status: Some(f.status),
                body: Some(f.body.chars().take(512).collect()),
                error: None,
            },
            Err(e) => EgressStatus {
                error: Some(e),
                ..Default::default()
            },
        });
    }
    if !spec.env.is_empty() {
        status.env = Some(
            spec.env
                .iter()
                .map(|name| (name.clone(), std::env::var(name).ok()))
                .collect(),
        );
    }
    if !spec.credentials.is_empty() {
        status.credentials = Some(
            spec.credentials
                .iter()
                .map(|name| (name.clone(), credential_keys(req, name)))
                .collect(),
        );
    }
    if spec.private_tmp {
        status.tmp = Some(probe_tmp());
    }

    let mut requirements = None;
    if let Some(name) = spec
        .requirements
        .as_ref()
        .and_then(|r| r.environment_config.as_ref())
    {
        // Asked for on every call: Crossplane calls again with the resources
        // once it has them, and a response that stops asking would drop them.
        let mut resources = std::collections::HashMap::new();
        resources.insert(
            REQUIREMENT_KEY.to_string(),
            ResourceSelector {
                api_version: ENVIRONMENT_CONFIG_API_VERSION.to_string(),
                kind: ENVIRONMENT_CONFIG_KIND.to_string(),
                namespace: None,
                r#match: Some(resource_selector::Match::MatchName(name.clone())),
            },
        );
        requirements = Some(Requirements {
            resources,
            ..Default::default()
        });
        status.environment_config = req
            .required_resources
            .get(REQUIREMENT_KEY)
            .and_then(|r| r.items.first())
            .and_then(|r| r.resource.as_ref())
            .map(struct_to_json)
            .and_then(|v| v.get("data").cloned());
    }

    // In a function pipeline, Crossplane takes a composed resource for ready
    // only when a function says so: each NopResource is marked ready once
    // its observed counterpart carries Ready=True, what function-auto-ready
    // does for the compositions that end with it.
    let mut desired = req.desired.clone().unwrap_or_default();
    for i in 0..spec.nop_resources {
        let name = format!("nop-{i}");
        let ready = req
            .observed
            .as_ref()
            .and_then(|o| o.resources.get(&name))
            .and_then(|r| r.resource.as_ref())
            .map(struct_to_json)
            .is_some_and(|observed| {
                observed["status"]["conditions"]
                    .as_array()
                    .is_some_and(|cs| {
                        cs.iter()
                            .any(|c| c["type"] == "Ready" && c["status"] == "True")
                    })
            });
        desired.resources.insert(
            name,
            fnv1::Resource {
                resource: Some(json_to_struct(&nop_resource(i, spec.ready_after_seconds))),
                ready: if ready {
                    fnv1::Ready::True as i32
                } else {
                    fnv1::Ready::Unspecified as i32
                },
                ..Default::default()
            },
        );
    }
    set_status(&mut desired, &xr, &status)?;

    Ok(RunFunctionResponse {
        meta: Some(ResponseMeta {
            tag,
            ttl: Some(Duration {
                seconds: TTL_SECONDS,
                nanos: 0,
            }),
        }),
        desired: Some(desired),
        requirements,
        results: vec![FnResult {
            severity: Severity::Normal as i32,
            message: format!("composed {} nop resources", spec.nop_resources),
            target: Some(Target::Composite as i32),
            ..Default::default()
        }],
        ..Default::default()
    })
}

/// One composed NopResource: provider-nop turns it Ready after
/// ready_after_seconds, and records index in its fields.
pub fn nop_resource(index: u32, ready_after_seconds: u32) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": NOP_API_VERSION,
        "kind": NOP_KIND,
        "spec": {
            "forProvider": {
                "conditionAfter": [{
                    "time": format!("{ready_after_seconds}s"),
                    "conditionType": "Ready",
                    "conditionStatus": "True",
                }],
                "fields": {"index": index},
            },
        },
    })
}

/// The sorted keys of a step credential the request carries; empty when the
/// runtime withheld it.
fn credential_keys(req: &RunFunctionRequest, name: &str) -> Vec<String> {
    let mut keys: Vec<String> = req
        .credentials
        .get(name)
        .and_then(|c| c.source.as_ref())
        .map(|source| match source {
            fnv1::credentials::Source::CredentialData(d) => d.data.keys().cloned().collect(),
        })
        .unwrap_or_default();
    keys.sort();
    keys
}

/// Writes a file under /tmp and reads it back: what a granted private /tmp
/// makes possible, and what the sandbox refuses without the grant.
fn probe_tmp() -> TmpStatus {
    let probe = || -> Result<(), String> {
        std::fs::write(TMP_PROBE, b"ok").map_err(|e| format!("write {TMP_PROBE}: {e}"))?;
        let back =
            std::fs::read_to_string(TMP_PROBE).map_err(|e| format!("read {TMP_PROBE}: {e}"))?;
        if back != "ok" {
            return Err(format!("read {TMP_PROBE}: got {back:?}"));
        }
        Ok(())
    };
    match probe() {
        Ok(()) => TmpStatus {
            ok: true,
            error: None,
        },
        Err(e) => TmpStatus {
            ok: false,
            error: Some(e),
        },
    }
}

/// Burns compute until the runtime's deadline interrupts the run.
fn spin() -> ! {
    let mut n: u64 = 0;
    loop {
        n = n.wrapping_add(1);
        std::hint::black_box(n);
    }
}

/// Grows linear memory one MiB at a time up to mib, reporting the point at
/// which the runtime refused the growth (the per-run memory limit): a clean
/// error, not an allocation abort, so the fatal names the limit.
#[cfg(target_arch = "wasm32")]
fn grow(mib: u32) -> Result<(), String> {
    const PAGES_PER_MIB: usize = 16;
    for grown in 0..mib {
        if core::arch::wasm32::memory_grow(0, PAGES_PER_MIB) == usize::MAX {
            return Err(format!(
                "memory growth refused at {grown} MiB of the {mib} MiB asked for (limits.memory)"
            ));
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn grow(_mib: u32) -> Result<(), String> {
    Ok(())
}

/// Sets status on the desired composite resource (apiVersion and kind from
/// the observed one, earlier steps' status fields kept).
fn set_status(
    desired: &mut fnv1::State,
    observed: &serde_json::Value,
    status: &Status,
) -> Result<(), String> {
    let composite = desired
        .composite
        .get_or_insert_with(Default::default)
        .resource
        .get_or_insert_with(Default::default);
    let mut xr = struct_to_json(composite);
    if !xr.is_object() {
        xr = serde_json::json!({});
    }
    for key in ["apiVersion", "kind"] {
        if let Some(v) = observed.get(key) {
            xr[key] = v.clone();
        }
    }
    let written = serde_json::to_value(status).map_err(|e| format!("cannot encode status: {e}"))?;
    match xr.get_mut("status") {
        Some(serde_json::Value::Object(existing)) => {
            if let serde_json::Value::Object(fields) = written {
                existing.extend(fields);
            }
        }
        _ => xr["status"] = written,
    }
    *composite = json_to_struct(&xr);
    Ok(())
}

/// Decode, run, encode. Every failure becomes a fatal result so the host can
/// always decode the reply.
pub async fn handle<F, Fut>(input: &[u8], fetch: F) -> Vec<u8>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<Fetched, String>>,
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
                seconds: TTL_SECONDS,
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

/// A protobuf Struct as JSON, so the knobs read with serde.
pub fn struct_to_json(s: &Struct) -> serde_json::Value {
    serde_json::Value::Object(
        s.fields
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect(),
    )
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match &v.kind {
        None | Some(Kind::NullValue(_)) => serde_json::Value::Null,
        // A Struct has no integers: a whole number comes back as one, so a
        // knob declared u32 reads.
        Some(Kind::NumberValue(n)) if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 => {
            serde_json::Value::Number(serde_json::Number::from(*n as i64))
        }
        Some(Kind::NumberValue(n)) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Some(Kind::StringValue(s)) => serde_json::Value::String(s.clone()),
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(*b),
        Some(Kind::StructValue(s)) => struct_to_json(s),
        Some(Kind::ListValue(l)) => {
            serde_json::Value::Array(l.values.iter().map(value_to_json).collect())
        }
    }
}

/// JSON as a protobuf Struct; a non-object becomes an empty Struct.
pub fn json_to_struct(v: &serde_json::Value) -> Struct {
    match v {
        serde_json::Value::Object(map) => Struct {
            fields: map
                .iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        },
        _ => Struct::default(),
    }
}

fn json_to_value(v: &serde_json::Value) -> Value {
    let kind = match v {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(*b),
        serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or_default()),
        serde_json::Value::String(s) => Kind::StringValue(s.clone()),
        serde_json::Value::Array(items) => Kind::ListValue(ListValue {
            values: items.iter().map(json_to_value).collect(),
        }),
        serde_json::Value::Object(_) => Kind::StructValue(json_to_struct(v)),
    };
    Value { kind: Some(kind) }
}

/// Structured logging through the host: the world's typed `log` import on
/// the wasm target, stderr elsewhere.
pub mod log {
    #[derive(Clone, Copy, Debug)]
    enum Level {
        Info,
    }

    pub fn info(msg: &str, kv: &[(&str, &str)]) {
        emit(Level::Info, msg, kv);
    }

    #[cfg(target_arch = "wasm32")]
    fn emit(level: Level, msg: &str, kv: &[(&str, &str)]) {
        use crate::bindings::LogLevel;
        let level = match level {
            Level::Info => LogLevel::Info,
        };
        let kv: Vec<(String, String)> = kv
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        crate::bindings::log(level, msg, &kv);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn emit(level: Level, msg: &str, kv: &[(&str, &str)]) {
        eprintln!("{level:?} {msg} {kv:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fake_fetch(url: String) -> Result<Fetched, String> {
        if url == "https://registry.e2e.svc:5000/v2/" {
            Ok(Fetched {
                status: 401,
                body: r#"{"errors":[{"code":"UNAUTHORIZED"}]}"#.to_string(),
            })
        } else {
            Err(format!(
                "internal-error: sandbox.egress: no rule admits host {url:?}"
            ))
        }
    }

    fn request(spec: serde_json::Value) -> RunFunctionRequest {
        RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".into(),
                ..Default::default()
            }),
            observed: Some(fnv1::State {
                composite: Some(fnv1::Resource {
                    resource: Some(json_to_struct(&serde_json::json!({
                        "apiVersion": "e2e.function-wasm.io/v1alpha1",
                        "kind": "Guest",
                        "metadata": {"name": "probe", "namespace": "e2e"},
                        "spec": spec,
                    }))),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn run(req: &RunFunctionRequest) -> Result<RunFunctionResponse, String> {
        pollster::block_on(run_function(req, fake_fetch))
    }

    fn status_of(rsp: &RunFunctionResponse) -> Status {
        let xr = struct_to_json(
            rsp.desired
                .as_ref()
                .unwrap()
                .composite
                .as_ref()
                .unwrap()
                .resource
                .as_ref()
                .unwrap(),
        );
        serde_json::from_value(xr["status"].clone()).unwrap()
    }

    #[test]
    fn composes_one_nop_resource_by_default() {
        let rsp = run(&request(serde_json::json!({}))).unwrap();
        let desired = rsp.desired.as_ref().unwrap();
        assert_eq!(desired.resources.len(), 1);
        let nop = struct_to_json(desired.resources["nop-0"].resource.as_ref().unwrap());
        assert_eq!(nop["kind"], "NopResource");
        assert_eq!(
            nop["spec"]["forProvider"]["conditionAfter"][0]["time"],
            "0s"
        );
        assert_eq!(status_of(&rsp).module, MODULE);
        assert_eq!(rsp.results[0].message, "composed 1 nop resources");
    }

    #[test]
    fn knobs_drive_the_status() {
        let rsp = run(&request(serde_json::json!({
            "nopResources": 3,
            "readyAfterSeconds": 2,
            "egress": {"url": "https://registry.e2e.svc:5000/v2/"},
            "env": ["E2E_TOKEN"],
            "credentials": ["e2e"],
            "growMiB": 1,
        })))
        .unwrap();
        assert_eq!(rsp.desired.as_ref().unwrap().resources.len(), 3);
        let status = status_of(&rsp);
        assert_eq!(status.egress.as_ref().unwrap().status, Some(401));
        assert_eq!(status.env.as_ref().unwrap()["E2E_TOKEN"], None);
        assert_eq!(
            status.credentials.as_ref().unwrap()["e2e"],
            Vec::<String>::new()
        );
    }

    #[test]
    fn requirements_are_asked_for_and_echoed() {
        let mut req = request(serde_json::json!({
            "requirements": {"environmentConfig": "team-a"},
        }));
        let rsp = run(&req).unwrap();
        let selector = &rsp.requirements.as_ref().unwrap().resources[REQUIREMENT_KEY];
        assert_eq!(selector.kind, ENVIRONMENT_CONFIG_KIND);
        assert_eq!(status_of(&rsp).environment_config, None);

        req.required_resources.insert(
            REQUIREMENT_KEY.to_string(),
            fnv1::Resources {
                items: vec![fnv1::Resource {
                    resource: Some(json_to_struct(&serde_json::json!({
                        "apiVersion": ENVIRONMENT_CONFIG_API_VERSION,
                        "kind": ENVIRONMENT_CONFIG_KIND,
                        "data": {"region": "eu-central-1"},
                    }))),
                    ..Default::default()
                }],
            },
        );
        let rsp = run(&req).unwrap();
        assert_eq!(
            status_of(&rsp).environment_config,
            Some(serde_json::json!({"region": "eu-central-1"}))
        );
    }

    #[test]
    fn a_ready_observed_resource_is_marked_ready() {
        let mut req = request(serde_json::json!({"nopResources": 2}));
        let observed = req.observed.as_mut().unwrap();
        observed.resources.insert(
            "nop-1".to_string(),
            fnv1::Resource {
                resource: Some(json_to_struct(&serde_json::json!({
                    "status": {"conditions": [{"type": "Ready", "status": "True"}]},
                }))),
                ..Default::default()
            },
        );
        let rsp = run(&req).unwrap();
        let desired = rsp.desired.unwrap();
        assert_eq!(
            desired.resources["nop-0"].ready,
            fnv1::Ready::Unspecified as i32
        );
        assert_eq!(desired.resources["nop-1"].ready, fnv1::Ready::True as i32);
    }

    #[test]
    fn the_spec_reads_every_knob() {
        let spec: Spec = serde_json::from_value(serde_json::json!({
            "nopResources": 2, "readyAfterSeconds": 3, "egress": {"url": "u"}, "env": ["A"],
            "credentials": ["c"], "privateTmp": true, "requirements": {"environmentConfig": "e"},
            "fatal": "f", "spin": true, "growMiB": 64,
        }))
        .unwrap();
        assert_eq!(spec.grow_mib, 64);
        assert_eq!(spec.ready_after_seconds, 3);
        assert!(spec.private_tmp && spec.spin);
        assert_eq!(Spec::default().nop_resources, 0);
        assert_eq!(
            serde_json::from_value::<Spec>(serde_json::json!({}))
                .unwrap()
                .nop_resources,
            1
        );
    }

    #[test]
    fn fatal_knob_is_an_error() {
        assert_eq!(
            run(&request(serde_json::json!({"fatal": "asked to fail"}))).unwrap_err(),
            "asked to fail"
        );
    }

    #[test]
    fn handle_reports_a_missing_composite_as_fatal() {
        let rsp = RunFunctionResponse::decode(
            pollster::block_on(handle(
                &RunFunctionRequest::default().encode_to_vec(),
                fake_fetch,
            ))
            .as_slice(),
        )
        .unwrap();
        assert_eq!(rsp.results[0].severity, Severity::Fatal as i32);
    }
}
