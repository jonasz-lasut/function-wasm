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
    pub cache: ModuleCache,
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
        response::fatal(&mut rsp, reason);
        rsp
    }
}

#[tonic::async_trait]
impl FunctionRunnerService for WasmFunction {
    async fn run_function(
        &self,
        request: Request<RunFunctionRequest>,
    ) -> Result<Response<RunFunctionResponse>, Status> {
        let req = request.into_inner();
        let tag = req.meta.as_ref().map(|m| m.tag.clone()).unwrap_or_default();
        tracing::info!(tag, "Running function");
        let rsp = response::to(&req, self.ttl);

        let input: Input = match request::get_input(&req) {
            Ok(input) => input,
            Err(e) => {
                return Ok(Response::new(self.fatal(
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
            Err(e) => return Ok(Response::new(self.fatal(rsp, OUTCOME_REFUSED, e))),
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
                return Ok(Response::new(self.fatal(
                    rsp,
                    OUTCOME_REFUSED,
                    format!("cannot resolve module: {e}"),
                )));
            }
        };
        if let Err(e) = admission::require_ported(&source) {
            return Ok(Response::new(self.fatal(rsp, OUTCOME_REFUSED, e)));
        }

        let resolved = match self.resolver.resolve(&source.path) {
            Ok(resolved) => resolved,
            Err(e) => {
                return Ok(Response::new(self.fatal(
                    rsp,
                    OUTCOME_REFUSED,
                    format!("cannot resolve module: {e}"),
                )));
            }
        };

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
                    return Ok(Response::new(self.fatal(
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
        let manifest = if source.manifest_path.is_empty() {
            None
        } else {
            let raw = match self.resolver.read_manifest(&source.manifest_path) {
                Ok(raw) => raw,
                Err(e) => {
                    return Ok(Response::new(self.fatal(
                        rsp,
                        OUTCOME_REFUSED,
                        format!(
                            "cannot read the manifest of module {}: {e}",
                            resolved.description
                        ),
                    )));
                }
            };
            match crate::manifest::Manifest::parse(&raw) {
                Ok(m) => Some(m),
                Err(e) => {
                    return Ok(Response::new(self.fatal(
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
                return Ok(Response::new(self.fatal(
                    rsp,
                    OUTCOME_REFUSED,
                    format!("module {} {e}", resolved.description),
                )));
            }
        };
        if let Some(m) = &manifest
            && let Err(e) = m.check(&caps.grants(), input.config.as_ref(), "")
        {
            return Ok(Response::new(self.fatal(
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
                credentials: &req.credentials,
                withheld: "",
            };
            match sandboxenv::materialize(&caps.env, &sources) {
                Ok(env) => env,
                Err(e) => {
                    return Ok(Response::new(self.fatal(
                        rsp,
                        OUTCOME_REFUSED,
                        format!("module {}: {e}", resolved.description),
                    )));
                }
            }
        };

        // The whole request is forwarded and the whole response returned; the
        // engine works on the protobuf bytes.
        let bytes = req.encode_to_vec();
        // The per-run client logs every request with the module's reference
        // and digest attached.
        let http = caps.grant.map(|grant| {
            Arc::new(grant.client(resolved.description.clone(), resolved.digest.clone()))
                as Arc<dyn function_wasm_engine::HttpRequester>
        });
        let opts = RunOptions {
            timeout: admitted.timeout,
            memory_limit: admitted.memory_limit,
            private_tmp: caps.private_tmp,
            env,
            http,
            module: resolved.description.clone(),
            digest: resolved.digest.clone(),
        };
        let engine = Arc::clone(&self.engine);
        let out = tokio::task::spawn_blocking(move || engine.run(&module, &bytes, opts)).await;
        let out = match out {
            Ok(out) => out,
            Err(e) => {
                // A panic in the run is this request's fatal result, never
                // the process's end.
                return Ok(Response::new(self.fatal(
                    rsp,
                    OUTCOME_ERROR,
                    format!("internal error while running the module: {e}"),
                )));
            }
        };
        let out = match out {
            Ok(out) => out,
            Err(e) => {
                return Ok(Response::new(self.fatal(
                    rsp,
                    OUTCOME_ERROR,
                    format!("module {} failed: {e}", resolved.description),
                )));
            }
        };
        let mut got = match RunFunctionResponse::decode(out.as_slice()) {
            Ok(got) => got,
            Err(e) => {
                return Ok(Response::new(self.fatal(
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
        // still gets a well-formed reply.
        if got.meta.is_none() {
            got.meta = Some(ResponseMeta {
                tag,
                ttl: Some(pbjson_types::Duration {
                    seconds: self.ttl.as_secs() as i64,
                    nanos: self.ttl.subsec_nanos() as i32,
                }),
            });
        }
        Ok(Response::new(got))
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
            cache: ModuleCache::new(Arc::clone(&engine), crate::cache::CacheOptions::default()),
            engine,
            resolver: Arc::new(Resolver::new(dir, 128 << 20)),
            ttl: Duration::from_secs(60),
            policy: None,
            egress: Arc::new(crate::egress::Egress::new(Default::default(), 0.0, 0)),
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
