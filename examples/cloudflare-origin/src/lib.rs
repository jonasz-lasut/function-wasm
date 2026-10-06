//! The cloudflare-origin guest: a Crossplane composition function, compiled
//! to a WebAssembly component implementing ABI v2 (docs/abi-v2.md), that
//! lets only Cloudflare reach an origin. It composes the origin's AWS
//! security group and, when the composite resource asks for Cloudflare
//! exposure, one ingress rule per range Cloudflare publishes - fetched at
//! reconcile time through the host's egress, which the module's manifest
//! requests and the policy layers grant.
//!
//! `run_function` is ordinary async Rust over the protobuf messages prost
//! generated from the vendored crossplane proto; `bindings` holds the
//! wit-bindgen world (the `run` export, the typed `log` import and the
//! async `wasi:http/client` fetch) and only exists on the wasm target, so
//! the crate also builds and tests natively.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::IpAddr;

use prost::Message;
use prost_types::value::Kind;
use prost_types::{Duration, Struct, Value};
use serde::Deserialize;

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
    RequestMeta, ResponseMeta, Result as FnResult, RunFunctionRequest, RunFunctionResponse,
};
use fnv1::{Severity, Target};

const DEFAULT_TTL_SECONDS: i64 = 60;

/// Cloudflare's published edge ranges: both address families and an etag
/// in one small JSON document, no credentials needed.
pub const DEFAULT_IPS_URL: &str = "https://api.cloudflare.com/client/v4/ips";

const SECURITY_GROUP_API: &str = "ec2.aws.m.upbound.io/v1beta1";

/// What the composite resource asks for.
#[derive(Debug, PartialEq)]
struct Origin {
    name: String,
    region: String,
    vpc_id: String,
    port: u16,
    cloudflare: bool,
}

/// Cloudflare's ranges, each checked to be a CIDR of its family.
#[derive(Debug, PartialEq)]
pub struct Ranges {
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub etag: String,
}

/// Composes the origin's security group and, for Cloudflare exposure, its
/// ingress rules. `fetch` GETs a URL - the async wasi:http client on the
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
    let origin = read_origin(req)?;
    log::info(
        "Composing origin",
        &[
            ("origin", &origin.name),
            ("cloudflare", &origin.cloudflare.to_string()),
        ],
    );

    let mut desired = req.desired.clone().unwrap_or_default();
    desired.resources.insert(
        "security-group".to_string(),
        resource(security_group(&origin)),
    );

    let message = if origin.cloudflare {
        let url = config_string(req, "ipsUrl")?.unwrap_or_else(|| DEFAULT_IPS_URL.to_string());
        // A failed fetch is a fatal result, never an empty rule set: Crossplane
        // applies nothing from a fatal pipeline, so the rules composed last
        // time stay in place until Cloudflare's list can be read again.
        let body = fetch(url)
            .await
            .map_err(|e| format!("cannot fetch Cloudflare's IP ranges: {e}"))?;
        let ranges = parse_ranges(&body)?;
        for cidr in ranges.ipv4.iter().chain(&ranges.ipv6) {
            desired
                .resources
                .insert(rule_name(cidr), resource(ingress_rule(&origin, cidr)));
        }
        let count = ranges.ipv4.len() + ranges.ipv6.len();
        set_status(
            &mut desired,
            object(vec![
                ("etag", string(&ranges.etag)),
                ("ranges", number(count as f64)),
            ]),
        );
        format!(
            "{count} Cloudflare ranges may reach {} on port {}",
            origin.name, origin.port
        )
    } else {
        format!("{} takes no ingress: exposure is private", origin.name)
    };

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
            message,
            target: Some(Target::Composite as i32),
            ..Default::default()
        }],
        ..Default::default()
    })
}

/// Reads the composite resource's spec; the XRD defaults port and exposure,
/// so a missing one falls back to the same defaults.
fn read_origin(req: &RunFunctionRequest) -> Result<Origin, String> {
    let xr = req
        .observed
        .as_ref()
        .and_then(|s| s.composite.as_ref())
        .and_then(|c| c.resource.as_ref())
        .ok_or("cannot get observed composite resource: none in request")?;
    let name = field(xr, &["metadata", "name"])
        .and_then(string_value)
        .unwrap_or_default()
        .to_string();
    let spec_string = |key: &str| -> Result<String, String> {
        field(xr, &["spec", key])
            .and_then(string_value)
            .map(str::to_string)
            .ok_or_else(|| format!("spec.{key} is required"))
    };
    let port = match field(xr, &["spec", "port"]) {
        None => 443,
        Some(v) => match v.kind {
            Some(Kind::NumberValue(n)) if n.fract() == 0.0 && (1.0..=65535.0).contains(&n) => {
                n as u16
            }
            _ => return Err("spec.port must be a port number".to_string()),
        },
    };
    let cloudflare = match field(xr, &["spec", "exposure"]).and_then(string_value) {
        None | Some("private") => false,
        Some("cloudflare") => true,
        Some(other) => {
            return Err(format!(
                "spec.exposure {other:?} is not one of cloudflare, private"
            ))
        }
    };
    Ok(Origin {
        name,
        region: spec_string("region")?,
        vpc_id: spec_string("vpcId")?,
        port,
        cloudflare,
    })
}

/// Parses Cloudflare's `/client/v4/ips` answer. Every range must be a CIDR
/// of its list's family: a malformed list is a fatal result, never a
/// security group rule.
pub fn parse_ranges(body: &str) -> Result<Ranges, String> {
    #[derive(Deserialize)]
    struct Answer {
        success: bool,
        result: Option<Lists>,
    }
    #[derive(Deserialize)]
    struct Lists {
        ipv4_cidrs: Vec<String>,
        ipv6_cidrs: Vec<String>,
        #[serde(default)]
        etag: String,
    }
    let answer: Answer = serde_json::from_str(body)
        .map_err(|e| format!("cannot parse Cloudflare's IP ranges: {e}"))?;
    let lists = match answer {
        Answer {
            success: true,
            result: Some(lists),
        } => lists,
        _ => return Err("Cloudflare's IP ranges answer reports no success".to_string()),
    };
    if lists.ipv4_cidrs.is_empty() && lists.ipv6_cidrs.is_empty() {
        return Err("Cloudflare's IP ranges answer lists no ranges".to_string());
    }
    for (cidrs, v4) in [(&lists.ipv4_cidrs, true), (&lists.ipv6_cidrs, false)] {
        for cidr in cidrs {
            check_cidr(cidr, v4)?;
        }
    }
    Ok(Ranges {
        ipv4: lists.ipv4_cidrs,
        ipv6: lists.ipv6_cidrs,
        etag: lists.etag,
    })
}

fn check_cidr(cidr: &str, v4: bool) -> Result<(), String> {
    let family = if v4 { "IPv4" } else { "IPv6" };
    let bad = || format!("Cloudflare's {family} range {cidr:?} is not an {family} CIDR");
    let (addr, prefix) = cidr.split_once('/').ok_or_else(bad)?;
    let addr: IpAddr = addr.parse().map_err(|_| bad())?;
    let prefix: u8 = prefix.parse().map_err(|_| bad())?;
    match addr {
        IpAddr::V4(_) if v4 && prefix <= 32 => Ok(()),
        IpAddr::V6(_) if !v4 && prefix <= 128 => Ok(()),
        _ => Err(bad()),
    }
}

/// A composed resource's name, derived from its range so a list change only
/// replaces the rules whose ranges changed.
fn rule_name(cidr: &str) -> String {
    let mut name = String::from("cloudflare-");
    name.extend(cidr.chars().map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '-'
        }
    }));
    name
}

fn security_group(o: &Origin) -> Struct {
    let exposure = if o.cloudflare { "Cloudflare" } else { "no" };
    fields(vec![
        ("apiVersion", string(SECURITY_GROUP_API)),
        ("kind", string("SecurityGroup")),
        (
            "spec",
            object(vec![(
                "forProvider",
                object(vec![
                    ("region", string(&o.region)),
                    ("vpcId", string(&o.vpc_id)),
                    (
                        "description",
                        string(&format!("Origin {}: {exposure} ingress", o.name)),
                    ),
                ]),
            )]),
        ),
    ])
}

fn ingress_rule(o: &Origin, cidr: &str) -> Struct {
    let cidr_field = if cidr.contains(':') {
        "cidrIpv6"
    } else {
        "cidrIpv4"
    };
    fields(vec![
        ("apiVersion", string(SECURITY_GROUP_API)),
        ("kind", string("SecurityGroupIngressRule")),
        (
            "spec",
            object(vec![(
                "forProvider",
                object(vec![
                    ("region", string(&o.region)),
                    // The security group this composite resource composed.
                    (
                        "securityGroupIdSelector",
                        object(vec![("matchControllerRef", boolean(true))]),
                    ),
                    (cidr_field, string(cidr)),
                    ("ipProtocol", string("tcp")),
                    ("fromPort", number(f64::from(o.port))),
                    ("toPort", number(f64::from(o.port))),
                    ("description", string(&format!("Cloudflare {cidr}"))),
                ]),
            )]),
        ),
    ])
}

/// Sets status.cloudflare on the desired composite resource, keeping what
/// earlier pipeline steps put there.
fn set_status(desired: &mut fnv1::State, cloudflare: Value) {
    let xr = desired
        .composite
        .get_or_insert_with(Default::default)
        .resource
        .get_or_insert_with(Default::default);
    let status = xr
        .fields
        .entry("status".to_string())
        .or_insert_with(|| object(vec![]));
    if let Some(Kind::StructValue(s)) = &mut status.kind {
        s.fields.insert("cloudflare".to_string(), cloudflare);
    }
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
            .ok_or_else(|| format!("cannot read config: {key} must be a string")),
    }
}

fn field<'a>(s: &'a Struct, path: &[&str]) -> Option<&'a Value> {
    let (last, parents) = path.split_last()?;
    let mut current = s;
    for key in parents {
        current = current.fields.get(*key).and_then(struct_value)?;
    }
    current.fields.get(*last)
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

fn number(n: f64) -> Value {
    Value {
        kind: Some(Kind::NumberValue(n)),
    }
}

fn boolean(b: bool) -> Value {
    Value {
        kind: Some(Kind::BoolValue(b)),
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

fn resource(s: Struct) -> fnv1::Resource {
    fnv1::Resource {
        resource: Some(s),
        ..Default::default()
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
        emit(msg, kv);
    }

    #[cfg(target_arch = "wasm32")]
    fn emit(msg: &str, kv: &[(&str, &str)]) {
        let kv: Vec<(String, String)> = kv
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        crate::bindings::log(crate::bindings::LogLevel::Info, msg, &kv);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn emit(msg: &str, kv: &[(&str, &str)]) {
        eprintln!("{msg} {kv:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IPS: &str = r#"{"result":{"ipv4_cidrs":["173.245.48.0/20","103.21.244.0/22"],"ipv6_cidrs":["2400:cb00::/32"],"etag":"38f79d050aa027e3be3865e495dcc9bc"},"success":true,"errors":[],"messages":[]}"#;

    /// The native fetch double: Cloudflare's list answers, anything else is
    /// refused the way the runtime words a policy refusal.
    async fn fake_fetch(url: String) -> Result<String, String> {
        if url == DEFAULT_IPS_URL {
            Ok(IPS.to_string())
        } else {
            Err(format!(
                "internal-error: sandbox.egress: no rule admits host {url:?}"
            ))
        }
    }

    async fn no_fetch(url: String) -> Result<String, String> {
        panic!("a private origin fetched {url}")
    }

    fn origin(exposure: Option<&str>) -> fnv1::State {
        let mut spec = vec![
            ("region", string("eu-central-1")),
            ("vpcId", string("vpc-0a1b2c3d")),
        ];
        if let Some(e) = exposure {
            spec.push(("exposure", string(e)));
        }
        fnv1::State {
            composite: Some(resource(fields(vec![
                ("apiVersion", string("network.example.org/v1alpha1")),
                ("kind", string("Origin")),
                ("metadata", object(vec![("name", string("shop"))])),
                ("spec", object(spec)),
            ]))),
            ..Default::default()
        }
    }

    fn request(exposure: Option<&str>) -> RunFunctionRequest {
        RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".into(),
                ..Default::default()
            }),
            observed: Some(origin(exposure)),
            ..Default::default()
        }
    }

    fn names(rsp: &RunFunctionResponse) -> Vec<String> {
        let mut names: Vec<String> = rsp
            .desired
            .as_ref()
            .unwrap()
            .resources
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    fn for_provider<'a>(rsp: &'a RunFunctionResponse, name: &str) -> &'a Struct {
        let r = rsp.desired.as_ref().unwrap().resources[name]
            .resource
            .as_ref()
            .unwrap();
        struct_value(field(r, &["spec", "forProvider"]).unwrap()).unwrap()
    }

    #[test]
    fn cloudflare_exposure_composes_a_rule_per_range() {
        let rsp =
            pollster::block_on(run_function(&request(Some("cloudflare")), fake_fetch)).unwrap();
        assert_eq!(
            names(&rsp),
            [
                "cloudflare-103-21-244-0-22",
                "cloudflare-173-245-48-0-20",
                "cloudflare-2400-cb00---32",
                "security-group"
            ]
        );
        let v6 = for_provider(&rsp, "cloudflare-2400-cb00---32");
        assert_eq!(string_value(&v6.fields["cidrIpv6"]), Some("2400:cb00::/32"));
        assert_eq!(v6.fields["fromPort"], number(443.0));
        assert_eq!(string_value(&v6.fields["ipProtocol"]), Some("tcp"));
        let status = rsp.desired.as_ref().unwrap().composite.as_ref().unwrap();
        let cloudflare =
            field(status.resource.as_ref().unwrap(), &["status", "cloudflare"]).unwrap();
        assert_eq!(
            *cloudflare,
            object(vec![
                ("etag", string("38f79d050aa027e3be3865e495dcc9bc")),
                ("ranges", number(3.0)),
            ])
        );
        assert_eq!(
            rsp.results[0].message,
            "3 Cloudflare ranges may reach shop on port 443"
        );
    }

    #[test]
    fn private_exposure_composes_the_group_alone_without_fetching() {
        for exposure in [None, Some("private")] {
            let rsp = pollster::block_on(run_function(&request(exposure), no_fetch)).unwrap();
            assert_eq!(names(&rsp), ["security-group"]);
            assert!(rsp.desired.as_ref().unwrap().composite.is_none());
        }
    }

    #[test]
    fn earlier_steps_desired_state_is_kept() {
        let mut req = request(Some("cloudflare"));
        let mut desired = fnv1::State::default();
        desired
            .resources
            .insert("other".into(), fnv1::Resource::default());
        desired.composite = Some(resource(fields(vec![(
            "status",
            object(vec![("tier", string("gold"))]),
        )])));
        req.desired = Some(desired);
        let rsp = pollster::block_on(run_function(&req, fake_fetch)).unwrap();
        let d = rsp.desired.as_ref().unwrap();
        assert!(d.resources.contains_key("other"));
        let xr = d.composite.as_ref().unwrap().resource.as_ref().unwrap();
        assert_eq!(field(xr, &["status", "tier"]), Some(&string("gold")));
        assert!(field(xr, &["status", "cloudflare"]).is_some());
    }

    #[test]
    fn a_failed_fetch_is_fatal_not_an_empty_rule_set() {
        let mut req = request(Some("cloudflare"));
        req.input = Some(fields(vec![(
            "config",
            object(vec![("ipsUrl", string("https://evil.example.com/ips"))]),
        )]));
        assert_eq!(
            pollster::block_on(run_function(&req, fake_fetch)).unwrap_err(),
            "cannot fetch Cloudflare's IP ranges: internal-error: sandbox.egress: no rule admits host \"https://evil.example.com/ips\""
        );
    }

    #[test]
    fn spec_is_checked() {
        let mut req = request(Some("public"));
        assert_eq!(
            pollster::block_on(run_function(&req, no_fetch)).unwrap_err(),
            "spec.exposure \"public\" is not one of cloudflare, private"
        );
        req.observed = Some(fnv1::State {
            composite: Some(resource(fields(vec![("spec", object(vec![]))]))),
            ..Default::default()
        });
        assert_eq!(
            pollster::block_on(run_function(&req, no_fetch)).unwrap_err(),
            "spec.region is required"
        );
    }

    #[test]
    fn malformed_ranges_are_refused() {
        let cases = [
            (
                "not json",
                "cannot parse Cloudflare's IP ranges: expected ident at line 1 column 2",
            ),
            (
                r#"{"success":false,"result":null}"#,
                "Cloudflare's IP ranges answer reports no success",
            ),
            (
                r#"{"success":true,"result":{"ipv4_cidrs":[],"ipv6_cidrs":[]}}"#,
                "Cloudflare's IP ranges answer lists no ranges",
            ),
            (
                r#"{"success":true,"result":{"ipv4_cidrs":["2400:cb00::/32"],"ipv6_cidrs":[]}}"#,
                "Cloudflare's IPv4 range \"2400:cb00::/32\" is not an IPv4 CIDR",
            ),
            (
                r#"{"success":true,"result":{"ipv4_cidrs":["10.0.0.0/33"],"ipv6_cidrs":[]}}"#,
                "Cloudflare's IPv4 range \"10.0.0.0/33\" is not an IPv4 CIDR",
            ),
            (
                r#"{"success":true,"result":{"ipv4_cidrs":["0.0.0.0/0; drop"],"ipv6_cidrs":[]}}"#,
                "Cloudflare's IPv4 range \"0.0.0.0/0; drop\" is not an IPv4 CIDR",
            ),
        ];
        for (body, want) in cases {
            assert_eq!(parse_ranges(body).unwrap_err(), want, "{body}");
        }
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
