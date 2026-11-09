//! The pass-through gRPC codec: messages as their raw bytes. prost drops
//! fields it doesn't know, so what crosses the runtime is never decoded on
//! the way through, whether the runtime is serving Crossplane
//! (`crates/function/src/grpc.rs`) or calling an OCI module's server.

use bytes::{Buf as _, BufMut as _, Bytes};
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

/// The method every composition function serves.
pub const RUN_FUNCTION: &str = "/apiextensions.fn.proto.v1.FunctionRunnerService/RunFunction";

/// A pass-through codec: gRPC messages as their raw bytes.
#[derive(Default, Clone, Copy)]
pub struct RawCodec;

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

pub struct RawEncoder;

impl Encoder for RawEncoder {
    type Item = Vec<u8>;
    type Error = Status;

    fn encode(&mut self, item: Vec<u8>, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        dst.put_slice(&item);
        Ok(())
    }
}

pub struct RawDecoder;

impl Decoder for RawDecoder {
    type Item = Bytes;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}
