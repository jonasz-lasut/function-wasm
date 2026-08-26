//! The FunctionRunnerService implementation: resolves, loads and runs the
//! module named by each request's Input and returns its response verbatim.
//! Everything that stops the module from running to completion is reported
//! as a fatal result, never a gRPC error - one request must never take the
//! process down. The flow and its messages are the Rust port of
//! cmd/function/fn.go for Path sources.

use std::sync::Arc;
use std::time::Duration;

use function_sdk_rust::proto::v1::function_runner_service_server::FunctionRunnerService;
use function_sdk_rust::proto::v1::{ResponseMeta, RunFunctionRequest, RunFunctionResponse};
use function_sdk_rust::{request, response};
use function_wasm_engine::{Engine, RunOptions};
use prost::Message;
use tonic::{Request, Response, Status};

use crate::admission;
use crate::authz::{OperatorPolicy, Principal};
use crate::cache::ModuleCache;
use crate::input::Input;
use crate::resolver::Resolver;
use crate::sandboxenv;

/// Request outcomes, as the Go runtime's metrics label them: refused is the
/// runtime declining before running the module, error is the load or the run
/// failing.
const OUTCOME_REFUSED: &str = "refused";
const OUTCOME_ERROR: &str = "error";

pub struct WasmFunction {
    pub engine: Arc<Engine>,
    pub cache: Arc<ModuleCache>,
    pub resolver: Arc<Resolver>,
    pub ttl: Duration,
    /// The operator's grant policy (--sandbox-policy-file), the sole
    /// authority that enables a sandbox capability; None refuses every
    /// sandbox grant, so the runtime offers only the default sandbox.
    pub policy: Option<OperatorPolicy>,
    /// The egress mechanism: the SSRF block list, fixed budgets, the
    /// operator's Cedar CIDR rules and rate limit. Always built; whether a
    /// run may use it is the policy layers' grantEgress decision.
    pub egress: Arc<crate::egress::Egress>,
    /// Per-step run slots for limits.concurrency, keyed by module digest.
    pub step_slots: Arc<function_wasm_engine::concurrency::StepSlots>,
    /// The cosign verifier (--cosign-key); None serves unsigned modules
    /// except where the operator policy requires a signature, which then
    /// refuses.
    pub verifier: Option<Arc<crate::cosign::Verifier>>,
}

impl WasmFunction {
    /// Turns a refusal or failure into the request's fatal result and makes
    /// it visible on the runtime side too: one log line with the reason, so
    /// an operator of a shared runtime can see what is being refused without
    /// reading every XR's conditions.
    fn fatal(
        &self,
        mut rsp: RunFunctionResponse,
        outcome: &str,
        reason: String,
    ) -> RunFunctionResponse {
        tracing::info!(outcome, reason = %reason, "Request ended with a fatal result");
        function_wasm_engine::metrics::REQUESTS
            .with_label_values(&[outcome])
            .inc();
        response::fatal(&mut rsp, reason);
        rsp
    }
}

impl WasmFunction {
    /// The raw request path: the caller's bytes in, response bytes out. The
    /// guest receives the caller's own bytes - fields newer than this
    /// runtime's vendored proto included, which a prost decode cannot
    /// retain - with only the withheld pull credential edited out at the
    /// wire level; the guest's response bytes travel back untouched, a meta
    /// field appended when the guest omitted one.
    pub async fn handle_raw(
        &self,
        raw: Vec<u8>,
        deadline: Option<std::time::Instant>,
    ) -> Result<Vec<u8>, Status> {
        let req = RunFunctionRequest::decode(raw.as_slice())
            .map_err(|e| Status::internal(format!("cannot decode the request: {e}")))?;
        let tag = req.meta.as_ref().map(|m| m.tag.clone()).unwrap_or_default();
        tracing::info!(tag, "Running function");
        let rsp = response::to(&req, self.ttl);

        let input: Input = match request::get_input(&req) {
            Ok(input) => input,
            Err(e) => {
                return Ok(raw_rsp(self.fatal(
                    rsp,
                    OUTCOME_REFUSED,
                    format!("cannot get function input from *v1.RunFunctionRequest: {e}"),
                )));
            }
        };

        // What the Composition asks of the runtime is settled before any
        // module is read or compiled: nothing will run if it is refused.
        let admitted = match admission::admit(&input, &self.engine.config()) {
            Ok(admitted) => admitted,
            Err(e) => return Ok(raw_rsp(self.fatal(rsp, OUTCOME_REFUSED, e))),
        };
        // A module.from source names a field of the composite resource; the
        // compositionPolicy fences what it may pick (from.rs, default-deny).
        let composite = if input.module.from.is_empty() {
            None
        } else {
            req.observed
                .as_ref()
                .and_then(|o| o.composite.as_ref())
                .and_then(|c| c.resource.as_ref())
                .map(function_sdk_rust::resource::struct_to_json)
        };
        let source = match crate::from::from_composite(
            &input.module,
            admitted.composition.as_deref(),
            composite.as_ref(),
        ) {
            Ok(source) => source,
            Err(e) => {
                return Ok(raw_rsp(self.fatal(
                    rsp,
                    OUTCOME_REFUSED,
                    format!("cannot resolve module: {e}"),
                )));
            }
        };
        // Whether this module must carry a cosign signature is settled
        // before it is resolved: the legacy all-or-nothing --cosign-key
        // requires every module (and refuses non-OCI), while an operator
        // policy requires it per repository.
        let signature_required =
            crate::from::signature_required(self.policy.as_ref(), self.verifier.is_some(), &source);
        if signature_required && source.r#type != "OCI" {
            return Ok(raw_rsp(self.fatal(
                rsp,
                OUTCOME_REFUSED,
                format!(
                    "cannot resolve module: {}",
                    crate::from::non_oci_signature_refusal(self.policy.as_ref(), &source)
                ),
            )));
        }

        // The credential that pulls the module is the host's business: the
        // guest sees every other step credential, as a native function
        // would, but not the one that fetched it. The full set is kept
        // aside for the manifest's env bindings, which still may not name
        // the withheld one.
        let all_credentials = req.credentials.clone();
        let mut withheld = String::new();
        let mut auth = None;
        if source.r#type == "OCI"
            && let Some(oci) = &source.oci
            && !oci.credentials.is_empty()
        {
            let name = oci.credentials.clone();
            let Some(data) = all_credentials.get(&name).and_then(|c| {
                c.source.as_ref().map(
                    |function_sdk_rust::proto::v1::credentials::Source::CredentialData(d)| &d.data,
                )
            }) else {
                return Ok(raw_rsp(self.fatal(
                    rsp,
                    OUTCOME_REFUSED,
                    format!("cannot get credentials {name:?} for module.oci: the request carries no such credential"),
                )));
            };
            let registry = crate::location::parse_oci_reference(&oci.r#ref)
                .map(|r| r.registry)
                .unwrap_or_default();
            auth = match crate::oci::auth_for(&registry, data) {
                Ok(a) => Some(a),
                Err(e) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_REFUSED,
                        format!("cannot use credentials {name:?} for module.oci: {e}"),
                    )));
                }
            };
            withheld = name;
        }

        // resolve hashes a path module's file (up to --max-module-size) on
        // a stamp miss, so it runs off the async executor like the other
        // blocking steps.
        let resolved = {
            let resolver = Arc::clone(&self.resolver);
            let source = source.clone();
            let auth = auth.clone();
            match tokio::task::spawn_blocking(move || resolver.resolve(&source, auth)).await {
                Ok(Ok(resolved)) => resolved,
                Ok(Err(e)) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_REFUSED,
                        format!("cannot resolve module: {e}"),
                    )));
                }
                Err(e) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_ERROR,
                        format!("internal error while running the module: {e}"),
                    )));
                }
            }
        };
        // Signature verification gates serving, not fetching: it runs
        // before any cache is consulted (an artifact on disk may predate
        // the key), once per digest per process.
        if signature_required {
            let Some(verifier) = self.verifier.clone() else {
                return Ok(raw_rsp(self.fatal(
                    rsp,
                    OUTCOME_REFUSED,
                    format!(
                        "cannot verify module {}: the operator policy requires a cosign signature, but the runtime has no --cosign-key to verify it",
                        resolved.description
                    ),
                )));
            };
            let reference = crate::location::parse_oci_reference(
                &source
                    .oci
                    .as_ref()
                    .expect("an OCI source was required above")
                    .r#ref,
            )
            .expect("validated");
            let verify_auth = auth.clone();
            let verdict = tokio::task::spawn_blocking(move || {
                let client = crate::oci::RegistryClient::new(&reference, verify_auth);
                verifier.verify(&client, &reference)
            })
            .await;
            match verdict {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_REFUSED,
                        format!("cannot verify module {}: {e}", resolved.description),
                    )));
                }
                Err(e) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_ERROR,
                        format!("internal error while running the module: {e}"),
                    )));
                }
            }
        }

        let module = {
            let resolver = Arc::clone(&self.resolver);
            let target = resolved.clone();
            let fetch = move || {
                resolver
                    .fetch(&target)
                    .map_err(|e| format!("cannot fetch module: {e}"))
            };
            match self.cache.get(&resolved.digest, fetch).await {
                Ok(module) => module,
                Err(e) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_ERROR,
                        format!("cannot load module {}: {e}", resolved.description),
                    )));
                }
            }
        };

        // The module's ask - its manifest's requires - is decided by the
        // three layers: the manifest requests, the compositionPolicy and the
        // operator policy permit. A module without one gets the default
        // sandbox.
        let manifest_bytes = {
            let resolver = Arc::clone(&self.resolver);
            let target = resolved.clone();
            match tokio::task::spawn_blocking(move || resolver.manifest(&target)).await {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(e)) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_REFUSED,
                        format!(
                            "cannot read the manifest of module {}: {e}",
                            resolved.description
                        ),
                    )));
                }
                Err(e) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_ERROR,
                        format!("internal error while running the module: {e}"),
                    )));
                }
            }
        };
        let manifest = if manifest_bytes.is_empty() {
            None
        } else {
            match crate::manifest::Manifest::parse(&manifest_bytes) {
                Ok(m) => Some(m),
                Err(e) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_REFUSED,
                        format!(
                            "module {} has an invalid manifest: {e}",
                            resolved.description
                        ),
                    )));
                }
            }
        };
        let principal = principal_from(&req);
        let caps = match admission::admit_requires(
            manifest.as_ref().and_then(|m| m.requires.as_ref()),
            Some(&self.egress),
            self.policy.as_ref(),
            admitted.composition.as_deref(),
            &principal,
        ) {
            Ok(caps) => caps,
            Err(e) => {
                return Ok(raw_rsp(self.fatal(
                    rsp,
                    OUTCOME_REFUSED,
                    format!("module {} {e}", resolved.description),
                )));
            }
        };
        if let Some(m) = &manifest
            && let Err(e) = m.check(
                &caps.grants(),
                input.config.as_ref(),
                crate::manifest::runtime_version(),
            )
        {
            return Ok(raw_rsp(self.fatal(
                rsp,
                OUTCOME_REFUSED,
                format!("module {} {e}", resolved.description),
            )));
        }
        // The manifest's env bindings resolve against the request's own
        // credentials. (The withheld pull credential arrives with OCI
        // sources; a Path source has none.)
        let env = if caps.env.is_empty() {
            Default::default()
        } else {
            let sources = sandboxenv::Sources {
                credentials: &all_credentials,
                withheld: &withheld,
            };
            match sandboxenv::materialize(&caps.env, &sources) {
                Ok(env) => env,
                Err(e) => {
                    return Ok(raw_rsp(self.fatal(
                        rsp,
                        OUTCOME_REFUSED,
                        format!("module {}: {e}", resolved.description),
                    )));
                }
            }
        };

        // The whole request is forwarded and the whole response returned:
        // the caller's own bytes, with only the pull credential edited out
        // at the wire level; the engine works on the protobuf bytes.
        let bytes = if withheld.is_empty() {
            raw
        } else {
            crate::protowire::strip_credential(&raw, &withheld)
        };
        // The per-run client logs every request with the module's reference
        // and digest attached.
        let http = caps.grant.map(|grant| {
            Arc::new(grant.client(resolved.description.clone(), resolved.digest.clone()))
                as Arc<dyn function_wasm_engine::HttpRequester>
        });
        let opts = RunOptions {
            timeout: admitted.timeout,
            deadline,
            memory_limit: admitted.memory_limit,
            private_tmp: caps.private_tmp,
            env,
            http,
            module: resolved.description.clone(),
            digest: resolved.digest.clone(),
        };
        // A per-step slot, when limits.concurrency is set, is taken before
        // the engine's global slot: one step does not take every global slot
        // from every other. Held until the run ends.
        let step_slots = Arc::clone(&self.step_slots);
        let concurrency = admitted.concurrency;
        let step_key = resolved.digest.clone();
        let mut wait =
            std::time::Instant::now() + admitted.timeout.unwrap_or(self.engine.config().timeout);
        if let Some(d) = deadline
            && d < wait
        {
            wait = d;
        }
        let engine = Arc::clone(&self.engine);
        let out = tokio::task::spawn_blocking(move || {
            let _step = if concurrency > 0 {
                Some(step_slots.acquire(&step_key, concurrency, wait)?)
            } else {
                None
            };
            engine.run(&module, &bytes, opts).map_err(|e| e.to_string())
        })
        .await;
        let out = match out {
            Ok(out) => out,
            Err(e) => {
                // A panic in the run is this request's fatal result, never
                // the process's end.
                return Ok(raw_rsp(self.fatal(
                    rsp,
                    OUTCOME_ERROR,
                    format!("internal error while running the module: {e}"),
                )));
            }
        };
        let out = match out {
            Ok(out) => out,
            Err(e) => {
                return Ok(raw_rsp(self.fatal(
                    rsp,
                    OUTCOME_ERROR,
                    format!("module {} failed: {e}", resolved.description),
                )));
            }
        };
        let got = match RunFunctionResponse::decode(out.as_slice()) {
            Ok(got) => got,
            Err(e) => {
                return Ok(raw_rsp(self.fatal(
                    rsp,
                    OUTCOME_ERROR,
                    format!(
                        "module {} failed: cannot decode response: {e}",
                        resolved.description
                    ),
                )));
            }
        };
        // A guest that skipped the response meta (a non-Go guest, typically)
        // still gets a well-formed reply - appended to its own bytes, which
        // otherwise travel back exactly as produced.
        function_wasm_engine::metrics::REQUESTS
            .with_label_values(&["ok"])
            .inc();
        if got.meta.is_some() {
            return Ok(out);
        }
        let meta = ResponseMeta {
            tag,
            ttl: Some(pbjson_types::Duration {
                seconds: self.ttl.as_secs() as i64,
                nanos: self.ttl.subsec_nanos() as i32,
            }),
        };
        Ok(crate::protowire::append_meta(out, &meta.encode_to_vec()))
    }
}

/// A runtime-produced response (a fatal result) as wire bytes.
fn raw_rsp(rsp: RunFunctionResponse) -> Vec<u8> {
    rsp.encode_to_vec()
}

#[tonic::async_trait]
impl FunctionRunnerService for WasmFunction {
    async fn run_function(
        &self,
        request: Request<RunFunctionRequest>,
    ) -> Result<Response<RunFunctionResponse>, Status> {
        // The request's own deadline: its gRPC timeout, when the caller set
        // one. (The typed path re-encodes; the production server serves
        // handle_raw directly with the caller's bytes.)
        let deadline = request
            .metadata()
            .get("grpc-timeout")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_grpc_timeout)
            .map(|t| std::time::Instant::now() + t);
        let out = self
            .handle_raw(request.into_inner().encode_to_vec(), deadline)
            .await?;
        let rsp = RunFunctionResponse::decode(out.as_slice())
            .map_err(|e| Status::internal(format!("cannot encode the response: {e}")))?;
        Ok(Response::new(rsp))
    }
}

/// The operator-policy principal from the observed composite resource: its
/// kind and namespace, read without converting the whole object. A request
/// with no observed composite yields the zero principal, which matches no
/// principal condition - safe, since the policy layers only narrow.
fn principal_from(req: &RunFunctionRequest) -> Principal {
    use pbjson_types::value::Kind;
    let Some(xr) = req
        .observed
        .as_ref()
        .and_then(|o| o.composite.as_ref())
        .and_then(|c| c.resource.as_ref())
    else {
        return Principal::default();
    };
    let str_field = |s: &pbjson_types::Struct, key: &str| -> String {
        s.fields
            .get(key)
            .and_then(|v| match &v.kind {
                Some(Kind::StringValue(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default()
    };
    let namespace = xr
        .fields
        .get("metadata")
        .and_then(|v| match &v.kind {
            Some(Kind::StructValue(md)) => Some(str_field(md, "namespace")),
            _ => None,
        })
        .unwrap_or_default();
    Principal {
        namespace,
        xr_kind: str_field(xr, "kind"),
        composition: String::new(),
    }
}

/// Parses a gRPC timeout header ("5S", "100m"): an integer value and one of
/// H, M, S (hours, minutes, seconds) or m, u, n (milli, micro, nano).
pub(crate) fn parse_grpc_timeout(v: &str) -> Option<Duration> {
    let (value, unit) = v.split_at(v.len().checked_sub(1)?);
    let value: u64 = value.parse().ok()?;
    Some(match unit {
        "H" => Duration::from_secs(value * 3600),
        "M" => Duration::from_secs(value * 60),
        "S" => Duration::from_secs(value),
        "m" => Duration::from_millis(value),
        "u" => Duration::from_micros(value),
        "n" => Duration::from_nanos(value),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use function_sdk_rust::proto::v1::{RequestMeta, Severity};
    use function_sdk_rust::resource;
    use function_wasm_engine::Config;

    // A valid ABI v1 module whose wasmfn_run returns 0: an empty response.
    const EMPTY_RESPONSE_WAT: &str = r#"(module
      (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#;

    fn function(dir: Option<std::path::PathBuf>) -> WasmFunction {
        let engine = Arc::new(Engine::new(Config::default()).expect("engine"));
        WasmFunction {
            cache: Arc::new(ModuleCache::new(
                Arc::clone(&engine),
                crate::cache::CacheOptions::default(),
            )),
            engine,
            resolver: Arc::new(Resolver::new(dir, 128 << 20, None)),
            ttl: Duration::from_secs(60),
            policy: None,
            egress: Arc::new(crate::egress::Egress::new(Default::default(), 0.0, 0)),
            step_slots: Arc::new(function_wasm_engine::concurrency::StepSlots::new()),
            verifier: None,
        }
    }

    fn request(input: serde_json::Value) -> Request<RunFunctionRequest> {
        Request::new(RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".to_string(),
                ..Default::default()
            }),
            input: Some(resource::json_to_struct(input.as_object().expect("object"))),
            ..Default::default()
        })
    }

    async fn run(f: &WasmFunction, input: serde_json::Value) -> RunFunctionResponse {
        f.run_function(request(input))
            .await
            .expect("never a gRPC error")
            .into_inner()
    }

    fn input(module: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "apiVersion": "wasm.fn.crossplane.io/v1beta1",
            "kind": "Input",
            "module": module,
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_a_path_module() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("fn.wasm"),
            wat::parse_str(EMPTY_RESPONSE_WAT).expect("wat"),
        )
        .expect("write");
        let f = function(Some(dir.path().to_owned()));

        let rsp = run(
            &f,
            input(serde_json::json!({"type": "Path", "path": "fn.wasm"})),
        )
        .await;
        assert!(
            rsp.results.is_empty(),
            "unexpected results: {:?}",
            rsp.results
        );
        // The guest omitted meta, so the host filled it.
        assert_eq!(rsp.meta.expect("meta").tag, "t");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_an_oci_module_with_its_manifest_granted() {
        let wasm = wat::parse_str(EMPTY_RESPONSE_WAT).expect("wat");
        let module_manifest = br#"{"abi":1,"requires":{"filesystem":{"privateTmp":true}}}"#;
        let (digest, addr) =
            crate::oci::testregistry::wasm_artifact(&wasm, Some(module_manifest), true);
        let mut f = function(None);
        f.policy = Some(
            crate::authz::OperatorPolicy::new(
                "test",
                r#"permit (principal, action == Action::"usePrivateTmp", resource);"#,
            )
            .expect("policy"),
        );
        let rsp = run(
            &f,
            input(serde_json::json!({
                "type": "OCI",
                "oci": {"ref": format!("{addr}/example/greeter@{digest}")},
            })),
        )
        .await;
        assert!(
            rsp.results.is_empty(),
            "unexpected results: {:?}",
            rsp.results
        );
        assert_eq!(rsp.meta.expect("meta").tag, "t");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn metrics_record_the_request_and_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("fn.wasm"),
            wat::parse_str(EMPTY_RESPONSE_WAT).expect("wat"),
        )
        .expect("write");
        let f = function(Some(dir.path().to_owned()));
        let rsp = run(
            &f,
            input(serde_json::json!({"type": "Path", "path": "fn.wasm"})),
        )
        .await;
        assert!(rsp.results.is_empty(), "{rsp:?}");
        // The wiring, not the values: other tests in this process share the
        // default registry.
        let m = function_wasm_engine::metrics::sample;
        assert!(
            m("function_wasm_module_requests_total", &[("outcome", "ok")]).unwrap_or(0.0) >= 1.0
        );
        assert!(
            m(
                "function_wasm_module_run_duration_seconds",
                &[("outcome", "ok")]
            )
            .unwrap_or(0.0)
                >= 1.0
        );
        assert!(m("function_wasm_module_compile_duration_seconds", &[]).unwrap_or(0.0) >= 1.0);
        assert!(
            m(
                "function_wasm_module_fetch_duration_seconds",
                &[("source", "path")]
            )
            .unwrap_or(0.0)
                >= 1.0
        );
        assert!(
            m(
                "function_wasm_module_cache_events_total",
                &[("cache", "compiled"), ("event", "miss")]
            )
            .unwrap_or(0.0)
                >= 1.0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_proxy_is_transparent_to_unknown_fields() {
        // An echo guest: wasmfn_run returns the request buffer itself, so
        // the response is exactly what the runtime forwarded.
        const ECHO_WAT: &str = r#"(module
          (memory (export "memory") 2)
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 1024)
          (func (export "wasmfn_run") (param i32 i32) (result i64)
            (i64.or
              (i64.shl (i64.extend_i32_u (local.get 0)) (i64.const 32))
              (i64.extend_i32_u (local.get 1)))))"#;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("fn.wasm"),
            wat::parse_str(ECHO_WAT).expect("wat"),
        )
        .expect("write");
        let f = function(Some(dir.path().to_owned()));

        let typed = RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".to_string(),
                ..Default::default()
            }),
            input: Some(resource::json_to_struct(
                serde_json::json!({
                    "apiVersion": "wasm.fn.crossplane.io/v1beta1",
                    "kind": "Input",
                    "module": {"type": "Path", "path": "fn.wasm"},
                })
                .as_object()
                .expect("object"),
            )),
            ..Default::default()
        };
        let mut raw = typed.encode_to_vec();
        // A field this runtime's vendored proto does not know: field 999.
        let unknown = [0xba, 0x3e, 0x03, b'x', b'y', b'z'];
        raw.extend_from_slice(&unknown);

        let out = f.handle_raw(raw.clone(), None).await.expect("run");
        // The guest saw - and echoed - the caller's exact bytes, the
        // unknown field included; and since the echo carries a field-1
        // message (read back as meta), the response returned untouched.
        assert_eq!(out, raw);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_expired_deadline_ends_the_run() {
        const LOOP_WAT: &str = r#"(module (memory (export "memory") 1)
          (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
          (func (export "wasmfn_run") (param i32 i32) (result i64)
            (loop $l br $l)
            i64.const 0))"#;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("fn.wasm"),
            wat::parse_str(LOOP_WAT).expect("wat"),
        )
        .expect("write");
        let f = function(Some(dir.path().to_owned()));
        let raw = RunFunctionRequest {
            input: Some(resource::json_to_struct(
                serde_json::json!({
                    "apiVersion": "wasm.fn.crossplane.io/v1beta1",
                    "kind": "Input",
                    "module": {"type": "Path", "path": "fn.wasm"},
                })
                .as_object()
                .expect("object"),
            )),
            ..Default::default()
        };
        let out = f
            .handle_raw(raw.encode_to_vec(), Some(std::time::Instant::now()))
            .await
            .expect("handled");
        let rsp = RunFunctionResponse::decode(out.as_slice()).expect("decode");
        let result = rsp.results.first().expect("a fatal result");
        assert_eq!(result.severity, Severity::Fatal as i32);
        assert!(
            result.message.contains("exceeded its execution deadline"),
            "{}",
            result.message
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refusals_are_fatal_results() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("fn.wasm"),
            wat::parse_str(EMPTY_RESPONSE_WAT).expect("wat"),
        )
        .expect("write");

        let cases: &[(&str, Option<std::path::PathBuf>, serde_json::Value, &str)] = &[
            (
                "NoModuleDir",
                None,
                input(serde_json::json!({"type": "Path", "path": "fn.wasm"})),
                "cannot resolve module: module.path is refused: the function was started without --module-dir",
            ),
            (
                "TimeoutOverCeiling",
                Some(dir.path().to_owned()),
                {
                    let mut v = input(serde_json::json!({"type": "Path", "path": "fn.wasm"}));
                    v["limits"] = serde_json::json!({"timeout": "1m"});
                    v
                },
                "limits.timeout 1m0s exceeds the runtime's --module-timeout of 30s",
            ),
            (
                "MissingFile",
                Some(dir.path().to_owned()),
                input(serde_json::json!({"type": "Path", "path": "other.wasm"})),
                "cannot resolve module: cannot stat module file",
            ),
        ];
        for (name, dir, in_value, want_prefix) in cases {
            let f = function(dir.clone());
            let rsp = run(&f, in_value.clone()).await;
            let result = rsp
                .results
                .first()
                .unwrap_or_else(|| panic!("{name}: no result"));
            assert_eq!(result.severity, Severity::Fatal as i32, "{name}");
            assert!(
                result.message.starts_with(want_prefix),
                "{name}: message {:?} does not start with {want_prefix:?}",
                result.message
            );
        }
    }
}
