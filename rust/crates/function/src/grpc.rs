//! The gRPC server over raw message bytes - what keeps the transparent
//! proxy honest. The generated tonic service decodes into prost types,
//! which drop protobuf fields newer than the vendored proto; this server
//! swaps only the codec: the FunctionRunnerService path hands
//! WasmFunction::handle_raw the caller's exact bytes and returns the
//! guest's exact bytes, while TLS, health, reflection and shutdown mirror
//! function-sdk-rust's serve.

use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Buf as _, BufMut as _, Bytes};
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::server::NamedService;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};

use crate::runner::WasmFunction;

const SERVICE: &str = "apiextensions.fn.proto.v1.FunctionRunnerService";
const RUN_FUNCTION: &str = "/apiextensions.fn.proto.v1.FunctionRunnerService/RunFunction";

/// A pass-through codec: gRPC messages as their raw bytes.
#[derive(Default)]
struct RawCodec;

impl Codec for RawCodec {
    type Encode = Vec<u8>;
    type Decode = Bytes;
    type Encoder = RawEncoder;
    type Decoder = RawDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        RawEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        RawDecoder
    }
}

struct RawEncoder;

impl Encoder for RawEncoder {
    type Item = Vec<u8>;
    type Error = Status;

    fn encode(&mut self, item: Vec<u8>, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put_slice(&item);
        Ok(())
    }
}

struct RawDecoder;

impl Decoder for RawDecoder {
    type Item = Bytes;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}

/// The FunctionRunnerService served raw, modeled on the generated tonic
/// server with only the codec swapped.
#[derive(Clone)]
pub struct RawFunctionServer {
    function: Arc<WasmFunction>,
    max_decoding_message_size: Option<usize>,
}

impl RawFunctionServer {
    pub fn new(function: Arc<WasmFunction>, max_decoding_message_size: Option<usize>) -> Self {
        RawFunctionServer {
            function,
            max_decoding_message_size,
        }
    }
}

impl NamedService for RawFunctionServer {
    const NAME: &'static str = SERVICE;
}

impl<B> tonic::codegen::Service<http::Request<B>> for RawFunctionServer
where
    B: tonic::codegen::Body + Send + 'static,
    B::Error: Into<tonic::codegen::StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        if req.uri().path() != RUN_FUNCTION {
            return Box::pin(async move {
                let mut response = http::Response::new(tonic::body::Body::default());
                let headers = response.headers_mut();
                headers.insert(
                    Status::GRPC_STATUS,
                    (tonic::Code::Unimplemented as i32).into(),
                );
                headers.insert(
                    http::header::CONTENT_TYPE,
                    tonic::metadata::GRPC_CONTENT_TYPE,
                );
                Ok(response)
            });
        }
        struct RunFunctionRaw {
            function: Arc<WasmFunction>,
            deadline: Option<std::time::Instant>,
        }
        impl tonic::server::UnaryService<Bytes> for RunFunctionRaw {
            type Response = Vec<u8>;
            type Future = tonic::codegen::BoxFuture<tonic::Response<Vec<u8>>, Status>;

            fn call(&mut self, request: tonic::Request<Bytes>) -> Self::Future {
                let function = Arc::clone(&self.function);
                let deadline = self.deadline;
                Box::pin(async move {
                    let out = function
                        .handle_raw(request.into_inner().to_vec(), deadline)
                        .await?;
                    Ok(tonic::Response::new(out))
                })
            }
        }
        // The request's own deadline, read from the raw header before the
        // codec sees the message.
        let deadline = req
            .headers()
            .get("grpc-timeout")
            .and_then(|v| v.to_str().ok())
            .and_then(crate::runner::parse_grpc_timeout)
            .map(|t| std::time::Instant::now() + t);
        let function = Arc::clone(&self.function);
        let max_decoding = self.max_decoding_message_size;
        Box::pin(async move {
            let method = RunFunctionRaw { function, deadline };
            let mut grpc = tonic::server::Grpc::new(RawCodec)
                .apply_max_message_size_config(max_decoding, None);
            Ok(grpc.unary(method, req).await)
        })
    }
}

/// Builds the raw FunctionRunnerService server - the transport of
/// function-sdk-rust's serve (mTLS from the certs dir unless insecure, v1
/// and v1alpha reflection, gRPC health, graceful shutdown on SIGTERM or
/// SIGINT), assembled here from the same public pieces because only the
/// codec differs: the SDK's generated server cannot carry the raw bytes
/// the transparent proxy needs. Hands back the health reporter with the
/// service NOT_SERVING next to the server future, so warm-up flips
/// readiness when it finishes; requests are served regardless of health,
/// and nothing listens until the future is awaited.
pub async fn serve(
    function: Arc<WasmFunction>,
    args: &function_sdk_rust::Args,
) -> Result<
    (
        tonic_health::server::HealthReporter,
        impl Future<Output = Result<(), String>> + use<>,
    ),
    String,
> {
    let address: SocketAddr = args
        .address
        .parse()
        .map_err(|e| format!("cannot parse listen address: {e}"))?;

    let mut builder = Server::builder();
    if !args.insecure {
        let dir = args
            .tls_certs_dir
            .as_deref()
            .ok_or("no credentials were provided - supply --tls-certs-dir or use --insecure")?;
        builder = builder
            .tls_config(tls_config(dir)?)
            .map_err(|e| format!("cannot configure TLS: {e}"))?;
    }

    let service = RawFunctionServer::new(function, args.max_recv_message_size);

    let reflection_v1 = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(function_sdk_rust::proto::FILE_DESCRIPTOR_SET)
        .build_v1()
        .map_err(|e| format!("cannot build gRPC reflection service: {e}"))?;
    let reflection_v1alpha = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(function_sdk_rust::proto::FILE_DESCRIPTOR_SET)
        .build_v1alpha()
        .map_err(|e| format!("cannot build gRPC reflection service: {e}"))?;

    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter.set_not_serving::<RawFunctionServer>().await;

    let insecure = args.insecure;
    let server = async move {
        tracing::info!(%address, insecure, "serving FunctionRunnerService");
        builder
            .add_service(service)
            .add_service(health_service)
            .add_service(reflection_v1)
            .add_service(reflection_v1alpha)
            .serve_with_shutdown(address, shutdown_signal())
            .await
            .map_err(|e| format!("gRPC server error: {e}"))
    };
    Ok((health_reporter, server))
}

fn tls_config(dir: &Path) -> Result<ServerTlsConfig, String> {
    let read = |name: &str| {
        let path = dir.join(name);
        std::fs::read(&path).map_err(|e| {
            format!(
                "cannot read TLS certificate or key from {}: {e}",
                path.display()
            )
        })
    };
    let cert = read("tls.crt")?;
    let key = read("tls.key")?;
    let ca = read("ca.crt")?;
    Ok(ServerTlsConfig::new()
        .identity(Identity::from_pem(cert, key))
        .client_ca_root(Certificate::from_pem(ca))
        .client_auth_optional(false))
}

#[cfg(unix)]
async fn shutdown_signal() {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("cannot install SIGTERM handler");
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    tracing::info!("shutting down");
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
