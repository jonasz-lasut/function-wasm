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
use crate::cache::ModuleCache;
use crate::input::Input;
use crate::resolver::Resolver;

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

        let resolved = match self.resolver.resolve(&input.module.path) {
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

        // The whole request is forwarded and the whole response returned; the
        // engine works on the protobuf bytes.
        let bytes = req.encode_to_vec();
        let opts = RunOptions {
            timeout: admitted.timeout,
            memory_limit: admitted.memory_limit,
            module: resolved.description.clone(),
            digest: resolved.digest.clone(),
            ..Default::default()
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
            cache: ModuleCache::new(Arc::clone(&engine)),
            engine,
            resolver: Arc::new(Resolver::new(dir, 128 << 20)),
            ttl: Duration::from_secs(60),
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
        std::fs::write(dir.path().join("fn.wasm"), EMPTY_RESPONSE_WAT).expect("write");
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
        std::fs::write(dir.path().join("fn.wasm"), EMPTY_RESPONSE_WAT).expect("write");

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
