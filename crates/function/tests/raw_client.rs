//! A raw-bytes gRPC client against the served runtime: the definitive
//! transparency proof. A typed client would drop an unknown protobuf field
//! before it ever left the process, so this client speaks the wire format
//! directly - the request bytes carry a field this runtime's vendored proto
//! does not know, an echo guest returns its request buffer, and the caller
//! must get its exact bytes back through the whole gRPC stack.

use std::sync::Arc;

use function_sdk_rust::proto::v1::{RequestMeta, RunFunctionRequest};
use function_sdk_rust::resource;
use function_wasm::authz::IpRules;
use function_wasm::cache::{CacheOptions, ModuleCache};
use function_wasm::grpc;
use function_wasm::resolver::Resolver;
use function_wasm::runner::WasmFunction;
use function_wasm_engine::{Config, Engine};
use prost::Message as _;

const ECHO_WAT: &str = r#"(module
  (memory (export "memory") 2)
  (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 1024)
  (func (export "wasmfn_run") (param i32 i32) (result i64)
    (i64.or
      (i64.shl (i64.extend_i32_u (local.get 0)) (i64.const 32))
      (i64.extend_i32_u (local.get 1)))))"#;

/// A pass-through client codec: gRPC messages as raw bytes.
#[derive(Default)]
struct RawClientCodec;

impl tonic::codec::Codec for RawClientCodec {
    type Encode = Vec<u8>;
    type Decode = Vec<u8>;
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

impl tonic::codec::Encoder for RawEncoder {
    type Item = Vec<u8>;
    type Error = tonic::Status;

    fn encode(
        &mut self,
        item: Vec<u8>,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), tonic::Status> {
        use bytes::BufMut as _;
        dst.put_slice(&item);
        Ok(())
    }
}

struct RawDecoder;

impl tonic::codec::Decoder for RawDecoder {
    type Item = Vec<u8>;
    type Error = tonic::Status;

    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Vec<u8>>, tonic::Status> {
        use bytes::Buf as _;
        let mut out = vec![0u8; src.remaining()];
        src.copy_to_slice(&mut out);
        Ok(Some(out))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_served_runtime_is_byte_transparent() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("fn.wasm"),
        wat::parse_str(ECHO_WAT).expect("wat"),
    )
    .expect("write");
    let engine = Arc::new(Engine::new(Config::default()).expect("engine"));
    let function = WasmFunction {
        cache: Arc::new(ModuleCache::new(
            Arc::clone(&engine),
            CacheOptions::default(),
        )),
        engine,
        resolver: Arc::new(Resolver::new(Some(dir.path().to_owned()), 128 << 20, None)),
        ttl: std::time::Duration::from_secs(60),
        policy: None,
        egress: Arc::new(function_wasm::egress::Egress::new(
            IpRules::default(),
            0.0,
            0,
        )),
        step_slots: Arc::new(function_wasm_engine::concurrency::StepSlots::new()),
        verifier: None,
        profile_dir: None,
    };

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let args = function_sdk_rust::Args {
        debug: false,
        address: format!("127.0.0.1:{port}"),
        tls_certs_dir: None,
        insecure: true,
        max_recv_message_size: None,
    };
    let (_health, server) = grpc::serve(Arc::new(function), &args).await.expect("serve");
    tokio::spawn(server);

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
    raw.extend_from_slice(&[0xba, 0x3e, 0x03, b'x', b'y', b'z']);

    let channel = connect(port).await;
    let mut client = tonic::client::Grpc::new(channel);
    client.ready().await.expect("ready");
    let path = tonic::codegen::http::uri::PathAndQuery::from_static(
        "/apiextensions.fn.proto.v1.FunctionRunnerService/RunFunction",
    );
    let rsp = client
        .unary(tonic::Request::new(raw.clone()), path, RawClientCodec)
        .await
        .expect("RunFunction")
        .into_inner();

    // The echo guest returned the forwarded request; the caller's exact
    // bytes - the unknown field included - came back through the whole
    // stack.
    assert_eq!(rsp, raw);
}

async fn connect(port: u16) -> tonic::transport::Channel {
    for _ in 0..50 {
        if let Ok(channel) =
            tonic::transport::Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
                .expect("uri")
                .connect()
                .await
        {
            return channel;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("cannot connect to the function under test");
}
