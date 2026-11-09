//! The test module as an OCI image's function (docs/one-pager-oci-runtime.md,
//! "The contract"): the same `run_function` the component wraps, behind
//! function-sdk-rust's server. Built static for musl by `make e2e.oci.guest`
//! and put into a FROM scratch image by the suite's setup.

use clap::Parser as _;
use e2e_guest::fnv1::RunFunctionRequest as GuestRequest;
use function_sdk_rust::proto::v1::function_runner_service_server::FunctionRunnerService;
use function_sdk_rust::proto::v1::{RunFunctionRequest, RunFunctionResponse};
use function_sdk_rust::{Args, serve};
use prost::Message as _;
use tonic::{Request, Response, Status};

struct Guest;

#[tonic::async_trait]
impl FunctionRunnerService for Guest {
    async fn run_function(
        &self,
        request: Request<RunFunctionRequest>,
    ) -> Result<Response<RunFunctionResponse>, Status> {
        let bytes = request.into_inner().encode_to_vec();
        let request = GuestRequest::decode(bytes.as_slice())
            .map_err(|e| Status::invalid_argument(format!("cannot decode the request: {e}")))?;
        // On its own thread: the module's knobs spin and grow, and its
        // future is the component's, with no need to be Send.
        let response = tokio::task::spawn_blocking(move || {
            // Egress goes through the host's proxy, which the OCI runtime
            // does not offer yet; the knob reports the refusal the way the
            // component reports the host's.
            let fetch = |url: String| async move {
                Err(format!("GET {url}: the OCI runtime offers no egress yet"))
            };
            pollster::block_on(e2e_guest::run_function(&request, fetch))
        })
        .await
        .map_err(|e| Status::internal(format!("the module's thread ended: {e}")))?
        .map_err(Status::unknown)?;
        let response = RunFunctionResponse::decode(response.encode_to_vec().as_slice())
            .map_err(|e| Status::internal(format!("cannot encode the response: {e}")))?;
        Ok(Response::new(response))
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = serve(Guest, &Args::parse()).await {
        eprintln!("cannot serve: {e}");
        std::process::exit(1);
    }
}
