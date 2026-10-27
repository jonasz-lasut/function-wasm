//! What a test of the runtime builds its stage from (feature `testutil`,
//! docs/project/testing.md): an in-memory registry, the signers, an
//! in-process runtime, a greeting server and the open operator policy. Each
//! suite used to declare its own copy of these; a change to one shape now
//! lands here once. Guest fixtures come from the engine's testing module.

pub use crate::cosign::testutil::{COSIGN_SIGN_PREDICATE, Shape, TestKey, attach};
pub use crate::testsigstore::Instance as Sigstore;
pub use function_wasm_engine::testing as guests;

pub mod registry {
    //! An in-memory distribution registry assembled by name.

    use std::collections::HashMap;

    use crate::oci::testregistry::{TestRegistry, digest_of, serve};
    use crate::oci::{MANIFEST_LAYER_TYPE, OCI_MANIFEST_TYPE};

    /// A registry under construction: empty, anonymous, with the referrers
    /// tag schema, until told otherwise. It derefs to the registry's
    /// tables, so a test may still reach into `manifests` and `blobs`.
    pub struct Registry(TestRegistry);

    impl Default for Registry {
        fn default() -> Registry {
            Registry::new()
        }
    }

    impl std::ops::Deref for Registry {
        type Target = TestRegistry;

        fn deref(&self) -> &TestRegistry {
            &self.0
        }
    }

    impl std::ops::DerefMut for Registry {
        fn deref_mut(&mut self) -> &mut TestRegistry {
            &mut self.0
        }
    }

    impl Registry {
        pub fn new() -> Registry {
            Registry(TestRegistry {
                manifests: HashMap::new(),
                blobs: HashMap::new(),
                bearer: false,
                referrers_api: false,
                basic: false,
            })
        }

        /// Pulls go through the Bearer token flow.
        pub fn bearer(mut self) -> Registry {
            self.0.bearer = true;
            self
        }

        /// The registry answers the OCI 1.1 referrers API instead of the
        /// sha256-<hex> tag schema.
        pub fn referrers_api(mut self) -> Registry {
            self.0.referrers_api = true;
            self
        }

        /// Every request needs the Basic credential (BASIC_USER, BASIC_PASSWORD).
        pub fn basic(mut self) -> Registry {
            self.0.basic = true;
            self
        }

        /// Stores a blob under its digest; returns the digest.
        pub fn blob(&mut self, bytes: &[u8]) -> String {
            let digest = digest_of(bytes);
            self.0.blobs.insert(digest.clone(), bytes.to_vec());
            digest
        }

        /// Stores a manifest under its digest; returns the digest.
        pub fn manifest(&mut self, bytes: &[u8]) -> String {
            let digest = digest_of(bytes);
            self.0.manifests.insert(digest.clone(), bytes.to_vec());
            digest
        }

        /// A module artifact: the wasm layer, the module manifest layer when
        /// given, and an empty config, titled so two modules of the same
        /// bytes get distinct digests. Returns the artifact's manifest digest.
        pub fn module(
            &mut self,
            wasm: &[u8],
            module_manifest: Option<&[u8]>,
            title: &str,
        ) -> String {
            let manifest = module_artifact(wasm, module_manifest, title);
            self.blob(wasm);
            self.blob(b"{}");
            if let Some(m) = module_manifest {
                self.blob(m);
            }
            self.manifest(&manifest)
        }

        /// Attaches a Sigstore bundle to subject as cosign 3 does; returns
        /// the referrer's digest.
        pub fn attach(&mut self, subject: &str, bundle: &[u8], shape: super::Shape) -> String {
            super::attach(&mut self.0, subject, bundle, shape)
        }

        /// Stores a referrer manifest as a pushing client does (the tag
        /// index maintained where the registry has no referrers API).
        pub fn push_referrer(&mut self, manifest: Vec<u8>) {
            self.0.push_referrer(manifest);
        }

        pub fn into_inner(self) -> TestRegistry {
            self.0
        }

        /// Serves the registry on loopback; returns its host:port.
        pub fn serve(self) -> String {
            serve(self.0)
        }
    }

    /// The OCI manifest of a module artifact as `guestfn push` shapes it,
    /// titled so two modules of the same bytes get distinct digests.
    pub fn module_artifact(wasm: &[u8], module_manifest: Option<&[u8]>, title: &str) -> Vec<u8> {
        let config = b"{}";
        let mut layers = vec![serde_json::json!({
            "mediaType": "application/wasm",
            "digest": digest_of(wasm),
            "size": wasm.len(),
        })];
        if let Some(m) = module_manifest {
            layers.push(serde_json::json!({
                "mediaType": MANIFEST_LAYER_TYPE,
                "digest": digest_of(m),
                "size": m.len(),
            }));
        }
        let mut manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_TYPE,
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(config), "size": config.len()},
            "layers": layers,
        });
        if !title.is_empty() {
            manifest["annotations"] = serde_json::json!({"org.opencontainers.image.title": title});
        }
        serde_json::to_vec(&manifest).expect("manifest json")
    }
}

pub mod runtime {
    //! The runtime in process, as `function serve` assembles it from its
    //! flags, with the defaults a test wants.

    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::authz::{IpPrefix, IpRules, OperatorPolicy};
    use crate::cache::{CacheOptions, ModuleCache};
    use crate::cosign::Verifier;
    use crate::egress::Egress;
    use crate::resolver::Resolver;
    use crate::runner::WasmFunction;
    use function_wasm_engine::concurrency::StepSlots;
    use function_wasm_engine::{Config, Engine};

    /// The module size ceiling the suites run with.
    pub const MAX_MODULE_SIZE: u64 = 128 << 20;

    #[derive(Default)]
    pub struct Builder {
        module_dir: Option<PathBuf>,
        policy: Option<OperatorPolicy>,
        verifier: Option<Verifier>,
        loopback: bool,
        ttl: Option<Duration>,
    }

    /// A runtime builder: no module dir, no policy, no verifier, the default
    /// egress block list, a 60 s TTL.
    pub fn builder() -> Builder {
        Builder::default()
    }

    impl Builder {
        /// Serves `module.type: Path` entries from dir (--module-dir).
        pub fn module_dir(mut self, dir: impl Into<PathBuf>) -> Builder {
            self.module_dir = Some(dir.into());
            self
        }

        /// The operator grant policy (--sandbox-policy-file).
        pub fn policy(mut self, policy: OperatorPolicy) -> Builder {
            self.policy = Some(policy);
            self
        }

        /// The cosign verifier (--cosign-key, --cosign-trusted-root).
        pub fn verifier(mut self, verifier: Verifier) -> Builder {
            self.verifier = Some(verifier);
            self
        }

        /// Opens the loopback ranges in the egress block list, so a guest
        /// may reach a server the test runs (a dialAddress permit).
        pub fn allow_loopback(mut self) -> Builder {
            self.loopback = true;
            self
        }

        pub fn ttl(mut self, ttl: Duration) -> Builder {
            self.ttl = Some(ttl);
            self
        }

        pub fn build(self) -> WasmFunction {
            let engine = Arc::new(Engine::new(Config::default()).expect("engine"));
            let rules = if self.loopback {
                IpRules {
                    blocked: Vec::new(),
                    allowed: vec![
                        IpPrefix::parse("127.0.0.0/8").expect("prefix"),
                        IpPrefix::parse("::1/128").expect("prefix"),
                    ],
                }
            } else {
                IpRules::default()
            };
            WasmFunction {
                cache: Arc::new(ModuleCache::new(
                    Arc::clone(&engine),
                    CacheOptions::default(),
                )),
                engine,
                resolver: Arc::new(Resolver::new(self.module_dir, MAX_MODULE_SIZE, None)),
                ttl: self.ttl.unwrap_or(Duration::from_secs(60)),
                policy: self.policy.map(Arc::new),
                egress: Arc::new(Egress::new(rules, 0.0, 0)),
                step_slots: Arc::new(StepSlots::new()),
                verifier: self.verifier.map(Arc::new),
            }
        }
    }

    /// Serves function over plain gRPC on a free loopback port, the way
    /// `function serve --insecure` does, and returns the port. The server
    /// runs on the current tokio runtime until the test ends.
    pub async fn serve(function: Arc<WasmFunction>) -> u16 {
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
        let (_health, server) = crate::grpc::serve(function, &args).await.expect("serve");
        tokio::spawn(server);
        port
    }
}

pub mod http {
    //! Servers a guest reaches through the host's egress.

    use std::io::Write as _;

    /// A tiny greeting server on loopback: /en answers "howdy\n", anything
    /// else 404. Returns its host:port.
    pub fn greeting_server() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut conn = conn;
                let mut buf = [0u8; 1024];
                let n = std::io::Read::read(&mut conn, &mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                let (status, body) = if path == "/en" {
                    ("200 OK", "howdy\n")
                } else {
                    ("404 Not Found", "")
                };
                let _ = conn.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        addr.to_string()
    }
}

pub mod policy {
    //! Operator policies a test runs under.

    use crate::authz::OperatorPolicy;

    /// An operator grant policy that enables every sandbox capability for
    /// any caller - a fully open operator layer. The layer is the enabler,
    /// so a function without one grants nothing.
    pub fn permissive() -> OperatorPolicy {
        OperatorPolicy::new(
            "test.cedar",
            r#"
permit (principal, action == Action::"usePrivateTmp", resource);
permit (principal, action == Action::"setEnv", resource);
permit (principal, action == Action::"grantEgress", resource);
permit (principal, action == Action::"spendCredential", resource);
"#,
        )
        .expect("policy")
    }
}
