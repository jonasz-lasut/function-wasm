//! The gRPC service over raw message bytes - what keeps the transparent
//! proxy honest. The generated tonic service decodes into prost types,
//! which drop protobuf fields newer than the vendored proto; this service
//! swaps only the codec: the FunctionRunnerService path hands
//! WasmFunction::handle_raw the caller's exact bytes and returns the
//! guest's exact bytes. The transport (mTLS, health, reflection,
//! shutdown) is function-sdk-rust's serve_service.

use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Buf as _, BufMut as _, Bytes};
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::server::NamedService;

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
