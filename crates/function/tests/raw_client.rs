//! A raw-bytes gRPC client against the served runtime: the definitive
//! transparency proof. A typed client would drop an unknown protobuf field
//! before it ever left the process, so this client speaks the wire format
//! directly - the request bytes carry fields this runtime's vendored proto
//! does not know, between step credentials the module was not granted, an
//! echo guest returns its request buffer, and the caller must get its exact
//! bytes back through the whole gRPC stack, less exactly those credentials.

use std::collections::HashMap;
use std::sync::Arc;

use function_sdk_rust::proto::v1::{
    CredentialData, Credentials, RequestMeta, RunFunctionRequest, credentials,
};
use function_sdk_rust::resource;
use function_wasm::authz::IpRules;
use function_wasm::cache::{CacheOptions, ModuleCache};
use function_wasm::grpc;
use function_wasm::resolver::Resolver;
use function_wasm::runner::WasmFunction;
use function_wasm_engine::{Config, Engine};
use prost::Message as _;

/// A component whose run returns the request list it was given.
const ECHO_WAT: &str = r#"(component
  (core module $m
    (memory (export "memory") 2)
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 4096)
    (func (export "run") (param i32 i32) (result i32)
      (i32.store8 (i32.const 64) (i32.const 0))
      (i32.store (i32.const 68) (local.get 0))
      (i32.store (i32.const 72) (local.get 1))
      (i32.const 64)))
  (core instance $i (instantiate $m))
  (func (export "run") (param "request" (list u8)) (result (result (list u8) (error string)))
    (canon lift (core func $i "run") (memory (core memory $i "memory")) (realloc (core func $i "cabi_realloc"))))
)"#;

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
        metrics_address: String::new(),
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
                "apiVersion": "wasm.fn.crossplane.io/v1",
                "kind": "Input",
                "module": {"type": "Path", "path": "fn.wasm"},
            })
            .as_object()
            .expect("object"),
        )),
        ..Default::default()
    };
    // Fields this runtime's vendored proto does not know (999 and 1000),
    // between and after step credentials: the module has no manifest, so it
    // was granted none of them.
    let unknown = [0xba, 0x3e, 0x03, b'x', b'y', b'z'];
    let unknown_too = [0xc2, 0x3e, 0x02, b'u', b'v'];
    let head = typed.encode_to_vec();
    let mut raw = head.clone();
    raw.extend(credential_entry("registry"));
    raw.extend_from_slice(&unknown);
    raw.extend(credential_entry("cmdb"));
    raw.extend(credential_entry("api"));
    raw.extend_from_slice(&unknown_too);
    let mut want = head;
    want.extend_from_slice(&unknown);
    want.extend_from_slice(&unknown_too);

    let channel = connect(port).await;
    let mut client = tonic::client::Grpc::new(channel);
    client.ready().await.expect("ready");
    let path = tonic::codegen::http::uri::PathAndQuery::from_static(
        "/apiextensions.fn.proto.v1.FunctionRunnerService/RunFunction",
    );
    let rsp = client
        .unary(tonic::Request::new(raw), path, RawClientCodec)
        .await
        .expect("RunFunction")
        .into_inner();

    // The echo guest returned the forwarded request: the caller's exact
    // bytes - the unknown fields included, in place - came back through the
    // whole stack, with every credentials entry edited out and nothing else.
    assert_eq!(rsp, want);
    assert!(!rsp.windows(6).any(|w| w == b"secret"));

    // The served call showed up in the Go runtime's gRPC server series -
    // started when it arrived, handled OK and one message each way when its
    // trailers went out (the trailers may land a beat after the client saw
    // the response, so the handled count is polled briefly).
    let labels = [
        ("grpc_type", "unary"),
        (
            "grpc_service",
            "apiextensions.fn.proto.v1.FunctionRunnerService",
        ),
        ("grpc_method", "RunFunction"),
    ];
    let sample = function_wasm_engine::metrics::sample;
    assert_eq!(
        sample("grpc_server_started_total", &labels),
        Some(1.0),
        "started"
    );
    assert_eq!(
        sample("grpc_server_msg_received_total", &labels),
        Some(1.0),
        "msg_received"
    );
    let handled_labels = [
        ("grpc_type", "unary"),
        (
            "grpc_service",
            "apiextensions.fn.proto.v1.FunctionRunnerService",
        ),
        ("grpc_method", "RunFunction"),
        ("grpc_code", "OK"),
    ];
    let mut handled = 0.0;
    for _ in 0..50 {
        handled = sample("grpc_server_handled_total", &handled_labels).unwrap_or(0.0);
        if handled >= 1.0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(handled, 1.0, "handled OK");
    assert_eq!(
        sample("grpc_server_msg_sent_total", &labels),
        Some(1.0),
        "msg_sent"
    );

    // InitializeMetrics parity: the streaming methods this server carries
    // exist as zero series from startup, never incremented (the Go
    // interceptor was unary-only too).
    assert_eq!(
        sample(
            "grpc_server_started_total",
            &[
                ("grpc_type", "server_stream"),
                ("grpc_service", "grpc.health.v1.Health"),
                ("grpc_method", "Watch"),
            ]
        ),
        Some(0.0),
        "streaming methods stay zero"
    );
}

/// One step credential's credentials map entry on the wire: what a request
/// carrying only that credential encodes to.
fn credential_entry(name: &str) -> Vec<u8> {
    RunFunctionRequest {
        credentials: HashMap::from([(
            name.to_string(),
            Credentials {
                source: Some(credentials::Source::CredentialData(CredentialData {
                    data: HashMap::from([("token".to_string(), format!("{name} secret").into())]),
                })),
            },
        )]),
        ..Default::default()
    }
    .encode_to_vec()
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
