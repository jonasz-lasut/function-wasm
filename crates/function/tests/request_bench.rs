//! A hand-run benchmark of the host's side of a warm request: handle_raw
//! in process, with an empty guest and with the end-to-end suite's committed
//! guest, over requests carrying 0, 5, 10, 100 and 1000 composed resources
//! of about 3 KB each in both the observed and the desired state (5 to 10
//! is what a composition usually holds; 100 and 1000 are stress sizes).
//! Beside each end-to-end figure it times the stages on their own - the
//! request's check and partial decode (and the whole decode they stand in
//! for), admission, the capability decision, the run, the response's, the
//! fences - so a change to one of them can be read against what it costs.
//! Not part of the suite:
//!
//!   cargo test -p guestfn-runtime --test request_bench --release -- --ignored --nocapture
//!
//! BENCH_SIZES=5,10 narrows the sizes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use function_sdk_rust::proto::v1::{
    CredentialData, Credentials, RequestMeta, Resource, RunFunctionRequest, RunFunctionResponse,
    State, credentials,
};
use guestfn_engine::{Config, Engine as _, RunOptions};
use guestfn_engine_wasm::Wasmtime;
use guestfn_runtime::authz::{OperatorPolicy, Principal};
use guestfn_runtime::input::Input;
use guestfn_runtime::manifest::Manifest;
use guestfn_runtime::runner::WasmFunction;
use guestfn_runtime::testing::{registry::Registry, runtime};
use guestfn_runtime::{admission, compose, from, partial};
use prost::Message;

const E2E_GUEST: &[u8] = include_bytes!("../../e2e/guest/fn.wasm");

/// A guest that reads nothing and answers with an empty response, with
/// room for the largest request here: what is left is the host's own cost.
const EMPTY_GUEST: &str = r#"(component
  (core module $m
    (memory (export "memory") 160)
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 4096)
    (func (export "run") (param i32 i32) (result i32)
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (i32.const 1024))
      (i32.store (i32.const 72) (i32.const 0))
      (i32.const 64)))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory (core memory $i "memory")) (realloc (core func $i "cabi_realloc")))))"#;

const PLAIN: &[u8] = br#"{"contract":"wasm@1.0","name":"e2e-guest","version":"0.1.0"}"#;

/// The end-to-end suite's "everything" manifest: a private /tmp, one egress
/// rule, one env binding and one whole credential.
const EVERYTHING: &[u8] = br#"{"contract":"wasm@1.0","name":"e2e-guest","version":"0.1.0-everything","requires":{"egress":{"http":[{"host":"registry.e2e.svc","methods":["GET"],"pathPrefix":"/v2/"}]},"filesystem":{"privateTmp":true},"env":[{"name":"E2E_TOKEN","fromCredential":{"name":"e2e","key":"token"}}],"credentials":["e2e"]}}"#;

/// The same without the private /tmp, whose directory costs the run more
/// than the decision costs the host.
const GRANTS: &[u8] = br#"{"contract":"wasm@1.0","name":"e2e-guest","version":"0.1.0-grants","requires":{"egress":{"http":[{"host":"registry.e2e.svc","methods":["GET"],"pathPrefix":"/v2/"}]},"env":[{"name":"E2E_TOKEN","fromCredential":{"name":"e2e","key":"token"}}],"credentials":["e2e"]}}"#;

/// The end-to-end suite's default operator policy, less the dial rule.
const OPERATOR: &str = r#"
permit (principal, action == Action::"requireSignature", resource)
when {
  resource in Repository::"registry.e2e.svc:5000" ||
  resource in Repository::"ghcr.io/jonasz-lasut/guestfn/examples"
};
permit (principal, action == Action::"grantEgress", resource == HostPattern::"registry.e2e.svc")
when { context.method == "GET" };
permit (principal, action == Action::"spendCredential", resource == Credential::"e2e");
permit (principal, action == Action::"setEnv", resource)
when { context.keys.contains("E2E_TOKEN") };
permit (principal, action == Action::"usePrivateTmp", resource);
"#;

/// A composition layer that narrows every capability, permitting what the
/// module asks.
const COMPOSITION: &str = r#"
permit (principal, action == Action::"grantEgress", resource == HostPattern::"registry.e2e.svc");
permit (principal, action == Action::"spendCredential", resource == Credential::"e2e");
permit (principal, action == Action::"setEnv", resource);
permit (principal, action == Action::"usePrivateTmp", resource);
"#;

/// A composition layer that fences what the module composes.
const FENCE: &str = r#"
permit (principal, action == Action::"composeResource", resource in Group::"nop.crossplane.io");
permit (principal, action == Action::"composeResource", resource in Group::"ec2.aws.upbound.io");
permit (principal, action == Action::"composeResource", resource in Group::"e2e.guestfn.io");
"#;

fn sizes() -> Vec<usize> {
    match std::env::var("BENCH_SIZES") {
        Ok(s) => s.split(',').map(|n| n.parse().expect("a size")).collect(),
        Err(_) => vec![0, 5, 10, 100, 1000],
    }
}

fn resource(v: serde_json::Value) -> Resource {
    Resource {
        resource: Some(serde_json::from_value(v).expect("a Struct")),
        ..Default::default()
    }
}

/// A managed resource of about 3 KB of JSON, in the shape of a
/// provider-upjet-aws SecurityGroup as the API server returns it: metadata
/// with annotations, labels and an owner reference, spec.forProvider,
/// status.atProvider and the conditions.
fn managed(i: usize, observed: bool) -> serde_json::Value {
    let rule = |port: u32| {
        serde_json::json!({
            "cidrBlocks": ["10.0.0.0/8", "172.16.0.0/12"],
            "description": format!("allow tcp {port} from the private ranges"),
            "fromPort": port, "toPort": port, "protocol": "tcp",
            "ipv6CidrBlocks": [], "prefixListIds": [], "securityGroups": [], "self": false,
        })
    };
    let tags = serde_json::json!({
        "crossplane-kind": "securitygroup.ec2.aws.upbound.io",
        "crossplane-name": format!("origin-{i}"),
        "crossplane-providerconfig": "default",
        "team": "platform", "cost-center": "cc-1234", "environment": "production",
    });
    let mut v = serde_json::json!({
        "apiVersion": "ec2.aws.upbound.io/v1beta1",
        "kind": "SecurityGroup",
        "metadata": {
            "name": format!("origin-{i}-7k2xq"),
            "generateName": format!("origin-{i}-"),
            "annotations": {
                "crossplane.io/composition-resource-name": format!("res-{i}"),
                "crossplane.io/external-name": format!("sg-0a1b2c3d4e5f6{i:04}"),
                "crossplane.io/external-create-pending": "2026-11-01T10:00:00Z",
                "crossplane.io/external-create-succeeded": "2026-11-01T10:00:02Z",
                "upjet.crossplane.io/provider-meta": "{\"e2bfb730-ecaa-11e6-8f88-34363bc7c4c0\":{\"create\":600000000000,\"delete\":900000000000},\"schema_version\":\"1\"}",
            },
            "labels": {
                "crossplane.io/claim-name": "origin",
                "crossplane.io/claim-namespace": "team-a",
                "crossplane.io/composite": "origin-x9z8y",
            },
            "ownerReferences": [{
                "apiVersion": "platform.example.org/v1alpha1", "kind": "XOrigin",
                "name": "origin-x9z8y", "uid": "3f0c8a52-51a9-4a0e-9d55-1f2f3c4d5e6f",
                "controller": true, "blockOwnerDeletion": true,
            }],
        },
        "spec": {
            "deletionPolicy": "Delete",
            "managementPolicies": ["*"],
            "providerConfigRef": {"name": "default"},
            "forProvider": {
                "region": "eu-central-1",
                "name": format!("origin-{i}"),
                "description": "the origin's security group, managed by Crossplane",
                "vpcId": "vpc-0123456789abcdef0",
                "tags": tags,
            },
            "initProvider": {},
        },
    });
    if observed {
        v["metadata"]["uid"] = serde_json::json!("9b2d7c1e-6f3a-4b8c-a1d2-e3f4a5b6c7d8");
        v["metadata"]["resourceVersion"] = serde_json::json!("48211973");
        v["metadata"]["generation"] = serde_json::json!(3);
        v["metadata"]["creationTimestamp"] = serde_json::json!("2026-11-01T10:00:00Z");
        v["metadata"]["finalizers"] =
            serde_json::json!(["finalizer.managedresource.crossplane.io"]);
        v["status"] = serde_json::json!({
            "atProvider": {
                "arn": format!("arn:aws:ec2:eu-central-1:123456789012:security-group/sg-0a1b2c3d4e5f6{i:04}"),
                "id": format!("sg-0a1b2c3d4e5f6{i:04}"),
                "name": format!("origin-{i}"),
                "description": "the origin's security group, managed by Crossplane",
                "ownerId": "123456789012",
                "region": "eu-central-1",
                "revokeRulesOnDelete": false,
                "vpcId": "vpc-0123456789abcdef0",
                "ingress": [rule(443), rule(80), rule(8443)],
                "egress": [rule(443)],
                "tags": tags, "tagsAll": tags,
            },
            "conditions": [
                {"type": "Ready", "status": "True", "reason": "Available",
                 "lastTransitionTime": "2026-11-01T10:00:05Z", "observedGeneration": 3},
                {"type": "Synced", "status": "True", "reason": "ReconcileSuccess",
                 "lastTransitionTime": "2026-11-01T10:00:03Z", "observedGeneration": 3},
                {"type": "LastAsyncOperation", "status": "True", "reason": "Success",
                 "lastTransitionTime": "2026-11-01T10:00:04Z"},
            ],
            "observedGeneration": 3,
        });
    }
    v
}

fn xr() -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "e2e.guestfn.io/v1alpha1",
        "kind": "Guest",
        "metadata": {"name": "probe", "namespace": "team-a"},
        "spec": {"nopResources": 1},
        "status": {"module": "e2e-guest"},
    })
}

/// A request with `n` composed resources in both states, the Input given
/// and the step credential e2e when asked.
fn request(input: &serde_json::Value, n: usize, credential: bool) -> Vec<u8> {
    let mut observed = HashMap::new();
    let mut desired = HashMap::new();
    for i in 0..n {
        observed.insert(format!("res-{i}"), resource(managed(i, true)));
        desired.insert(format!("res-{i}"), resource(managed(i, false)));
    }
    let mut step = HashMap::new();
    if credential {
        step.insert(
            "e2e".to_string(),
            Credentials {
                source: Some(credentials::Source::CredentialData(CredentialData {
                    data: HashMap::from([("token".to_string(), vec![b'x'; 32])]),
                })),
            },
        );
    }
    RunFunctionRequest {
        meta: Some(RequestMeta {
            tag: "bench".to_string(),
            ..Default::default()
        }),
        observed: Some(State {
            composite: Some(resource(xr())),
            resources: observed,
        }),
        desired: Some(State {
            composite: Some(resource(xr())),
            resources: desired,
        }),
        input: Some(serde_json::from_value(input.clone()).expect("a Struct")),
        credentials: step,
        ..Default::default()
    }
    .encode_to_vec()
}

fn input(module: serde_json::Value, composition_policy: &str) -> serde_json::Value {
    let mut v = serde_json::json!({
        "apiVersion": "guestfn.crossplane.io/v1",
        "kind": "Input",
        "module": module,
    });
    if !composition_policy.is_empty() {
        v["compositionPolicy"] = serde_json::json!(composition_policy);
    }
    v
}

struct Samples(Vec<Duration>);

impl Samples {
    fn of(iters: usize, mut f: impl FnMut()) -> Samples {
        for _ in 0..(iters / 10).max(3) {
            f();
        }
        let mut s = Vec::with_capacity(iters);
        for _ in 0..iters {
            let t = Instant::now();
            f();
            s.push(t.elapsed());
        }
        s.sort();
        Samples(s)
    }

    fn at(&self, q: f64) -> f64 {
        let n = self.0.len();
        self.0[((n as f64 - 1.0) * q) as usize].as_secs_f64() * 1e6
    }

    fn print(&self, name: &str) {
        println!(
            "  {name:44} p50 {:>10.1} us   p99 {:>10.1} us",
            self.at(0.5),
            self.at(0.99)
        );
    }
}

fn iterations(n: usize) -> usize {
    match n {
        0..=10 => 2000,
        11..=100 => 400,
        _ => 60,
    }
}

async fn end_to_end(f: &WasmFunction<Wasmtime>, raw: &[u8], iters: usize) -> (Samples, Vec<u8>) {
    let mut last = Vec::new();
    for _ in 0..(iters / 10).max(3) {
        last = f.handle_raw(raw.to_vec(), None).await.expect("a response");
    }
    let rsp = RunFunctionResponse::decode(last.as_slice()).expect("a response");
    let fatal: Vec<_> = rsp.results.iter().filter(|r| r.severity == 1).collect();
    assert!(fatal.is_empty(), "a fatal result: {fatal:?}");
    let mut s = Vec::with_capacity(iters);
    for _ in 0..iters {
        let bytes = raw.to_vec();
        let t = Instant::now();
        let out = f.handle_raw(bytes, None).await.expect("a response");
        s.push(t.elapsed());
        std::hint::black_box(out);
    }
    s.sort();
    (Samples(s), last)
}

/// The stages of one request on their own, as handle_raw runs them, and
/// the whole decodes it keeps for a fatal result, for a fenced response and
/// for bytes the wire check will not vouch for.
fn stages(
    raw: &[u8],
    response: &[u8],
    wasm: &[u8],
    manifest: &[u8],
    policy: Option<&OperatorPolicy>,
    iters: usize,
) {
    Samples::of(iters, || {
        std::hint::black_box(RunFunctionRequest::decode(raw).expect("a request"));
    })
    .print("request decode, whole (fatal path, fallback)");
    Samples::of(iters, || {
        assert!(partial::request_decodes(raw));
        std::hint::black_box(partial::Request::decode(raw).expect("a request"));
    })
    .print("request check + partial decode");

    let req = RunFunctionRequest::decode(raw).expect("a request");
    let partial = partial::Request::decode(raw).expect("a request");
    let config = Config::default();
    Samples::of(iters, || {
        let input: Input = partial.input().expect("an Input");
        let admitted = admission::admit(&input, &config, "wasm").expect("admitted");
        let source = from::from_composite(&input.module, admitted.composition.as_deref(), None)
            .expect("a source")
            .expect("a module");
        std::hint::black_box(from::signature_required(policy, false, &source));
    })
    .print("admission (Input, admit, signature rule)");

    let input: Input = function_sdk_rust::request::get_input(&req).expect("an Input");
    let admitted = admission::admit(&input, &config, "wasm").expect("admitted");
    let principal = Principal {
        namespace: "team-a".to_string(),
        xr_kind: "Guest".to_string(),
        composition: String::new(),
    };
    let egress = Arc::new(guestfn_runtime::egress::Egress::new(
        Default::default(),
        0.0,
        0,
    ));
    Samples::of(iters, || {
        let m = (!manifest.is_empty()).then(|| Manifest::parse(manifest).expect("a manifest"));
        let caps = admission::admit_requires(
            m.as_ref().and_then(|m| m.requires.as_ref()),
            Some(&egress),
            policy,
            admitted.composition.as_deref(),
            &principal,
        )
        .expect("granted");
        std::hint::black_box(caps.private_tmp);
    })
    .print("manifest parse + admit_requires");

    let engine = Wasmtime::new(Config::default()).expect("engine");
    let module = engine.compile(wasm).expect("compiles");
    Samples::of(iters, || {
        std::hint::black_box(
            engine
                .run(&module, raw, RunOptions::default())
                .expect("runs"),
        );
    })
    .print("engine.run");

    Samples::of(iters, || {
        std::hint::black_box(RunFunctionResponse::decode(response).expect("a response"));
    })
    .print("response decode, whole (fenced)");
    Samples::of(iters, || {
        assert!(partial::response_decodes(response));
        std::hint::black_box(partial::ResponseHead::decode(response).expect("a response"));
    })
    .print("response check + meta (unfenced)");

    if admission::compose_fenced(policy, admitted.composition.as_deref()) {
        let got = RunFunctionResponse::decode(response).expect("a response");
        Samples::of(iters, || {
            let changes = compose::changes(
                req.desired.as_ref(),
                got.desired.as_ref(),
                Some(("e2e.guestfn.io", "Guest")),
            )
            .expect("changes");
            admission::admit_compose(
                &changes,
                policy,
                admitted.composition.as_deref(),
                &principal,
            )
            .expect("permitted");
        })
        .print("output fence (diff + decisions)");
    }
}

struct Case {
    name: &'static str,
    wasm: Vec<u8>,
    manifest: &'static [u8],
    oci: bool,
    operator: bool,
    composition: &'static str,
    credential: bool,
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "hand-run benchmark"]
async fn a_warm_request_by_stage() {
    let empty = wat::parse_str(EMPTY_GUEST).expect("wat");
    let cases = [
        Case {
            name: "empty guest, Path, no policy",
            wasm: empty.clone(),
            manifest: b"",
            oci: false,
            operator: false,
            composition: "",
            credential: false,
        },
        Case {
            name: "e2e guest, OCI, no requires, operator policy",
            wasm: E2E_GUEST.to_vec(),
            manifest: PLAIN,
            oci: true,
            operator: true,
            composition: "",
            credential: false,
        },
        Case {
            name: "e2e guest, OCI, egress+env+credential, operator layer",
            wasm: E2E_GUEST.to_vec(),
            manifest: GRANTS,
            oci: true,
            operator: true,
            composition: "",
            credential: true,
        },
        Case {
            name: "e2e guest, OCI, egress+env+credential, both layers",
            wasm: E2E_GUEST.to_vec(),
            manifest: GRANTS,
            oci: true,
            operator: true,
            composition: COMPOSITION,
            credential: true,
        },
        Case {
            name: "e2e guest, OCI, everything (private /tmp), both layers",
            wasm: E2E_GUEST.to_vec(),
            manifest: EVERYTHING,
            oci: true,
            operator: true,
            composition: COMPOSITION,
            credential: true,
        },
        Case {
            name: "e2e guest, OCI, no requires, output fence",
            wasm: E2E_GUEST.to_vec(),
            manifest: PLAIN,
            oci: true,
            operator: true,
            composition: FENCE,
            credential: false,
        },
    ];
    for case in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let module = if case.oci {
            let mut registry = Registry::new();
            let digest = registry.module(&case.wasm, Some(case.manifest), case.name);
            let addr = registry.serve();
            serde_json::json!({"type": "OCI", "oci": {"ref": format!("{addr}/e2e/guest@{digest}")}})
        } else {
            std::fs::write(dir.path().join("fn.wasm"), &case.wasm).expect("write");
            serde_json::json!({"type": "Path", "path": "fn.wasm"})
        };
        let policy = case
            .operator
            .then(|| OperatorPolicy::new("bench.cedar", OPERATOR).expect("policy"));
        let mut builder = runtime::builder().module_dir(dir.path());
        if case.operator {
            builder = builder.policy(OperatorPolicy::new("bench.cedar", OPERATOR).expect("policy"));
        }
        let f = builder.build();
        let input = input(module, case.composition);
        for n in sizes() {
            let raw = request(&input, n, case.credential);
            let iters = iterations(n);
            let (total, response) = end_to_end(&f, &raw, iters).await;
            println!(
                "{} | {n} resources | request {} B, response {} B",
                case.name,
                raw.len(),
                response.len()
            );
            total.print("handle_raw, end to end");
            // The engine blocks on its own runtime, as it does under
            // handle_raw's spawn_blocking.
            tokio::task::block_in_place(|| {
                stages(
                    &raw,
                    &response,
                    &case.wasm,
                    case.manifest,
                    policy.as_ref(),
                    iters,
                )
            });
        }
    }
}
