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

pub mod tuf {
    //! A TUF repository a test publishes trusted roots through, the shape
    //! of Sigstore's: four roles signed with seeded P-256 keys, consistent
    //! snapshots (version-prefixed metadata, hash-prefixed targets), served
    //! on loopback. A test bootstraps a [`crate::trustroot::Source`] from
    //! its root, publishes a trusted root, republishes another for a
    //! rotation, expires the timestamp for a freeze, or takes it offline.

    use std::collections::HashMap;
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use p256::ecdsa::signature::Signer as _;
    use p256::pkcs8::EncodePublicKey as _;
    use serde_json::{Value, json};
    use sha2::Digest as _;

    /// When the metadata expires, unless expired on purpose.
    pub const FAR_FUTURE: &str = "2036-01-01T00:00:00Z";
    /// An expiry in the past: the freeze TUF refuses.
    pub const IN_THE_PAST: &str = "2020-01-01T00:00:00Z";
    /// The TUF specification version the metadata states.
    const SPEC_VERSION: &str = "1.0.31";

    /// One role's key.
    struct Role {
        signing: p256::ecdsa::SigningKey,
        key: Value,
        keyid: String,
    }

    impl Role {
        fn from_seed(seed: u8) -> Role {
            let signing = p256::ecdsa::SigningKey::from_bytes((&[seed; 32]).into()).expect("key");
            let public = signing
                .verifying_key()
                .to_public_key_pem(p256::pkcs8::LineEnding::LF)
                .expect("pem");
            let key = json!({
                "keytype": "ecdsa",
                "scheme": "ecdsa-sha2-nistp256",
                "keyval": {"public": public},
            });
            let keyid = serde_json::from_value::<sigstore_tuf::Key>(key.clone())
                .expect("a TUF key")
                .key_id()
                .expect("key id");
            Role {
                signing,
                key,
                keyid,
            }
        }

        /// The metadata file: signed, with one signature over its canonical
        /// JSON (ASN.1 DER ECDSA, hex, as securesystemslib writes it).
        fn envelope(&self, signed: Value) -> Vec<u8> {
            let canonical =
                sigstore_tuf::canonical_json::to_canonical_bytes(&signed).expect("canonical");
            let signature: p256::ecdsa::Signature = self.signing.sign(&canonical);
            serde_json::to_vec(&json!({
                "signatures": [{"keyid": self.keyid, "sig": hex::encode(signature.to_der())}],
                "signed": signed,
            }))
            .expect("metadata")
        }
    }

    /// The repository: its files by path, served while online.
    pub struct Repository {
        timestamp: Role,
        snapshot: Role,
        targets: Role,
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        online: Arc<AtomicBool>,
        /// The version of targets, snapshot and timestamp, moved together.
        version: u64,
        url: String,
        root_json: Vec<u8>,
    }

    impl Repository {
        /// Signs the root and starts serving on a free loopback port. Nothing
        /// is published yet: a fetch fails until [`Repository::publish`].
        pub fn serve() -> Repository {
            let root = Role::from_seed(41);
            let timestamp = Role::from_seed(42);
            let snapshot = Role::from_seed(43);
            let targets = Role::from_seed(44);
            let role = |r: &Role| json!({"keyids": [r.keyid], "threshold": 1});
            let root_json = root.envelope(json!({
                "_type": "root",
                "spec_version": SPEC_VERSION,
                "version": 1,
                "expires": FAR_FUTURE,
                "consistent_snapshot": true,
                "keys": {
                    root.keyid.clone(): root.key,
                    timestamp.keyid.clone(): timestamp.key,
                    snapshot.keyid.clone(): snapshot.key,
                    targets.keyid.clone(): targets.key,
                },
                "roles": {
                    "root": role(&root),
                    "timestamp": role(&timestamp),
                    "snapshot": role(&snapshot),
                    "targets": role(&targets),
                },
            }));
            let files = Arc::new(Mutex::new(HashMap::from([(
                "1.root.json".to_string(),
                root_json.clone(),
            )])));
            let online = Arc::new(AtomicBool::new(true));
            let url = serve_files(Arc::clone(&files), Arc::clone(&online));
            Repository {
                timestamp,
                snapshot,
                targets,
                files,
                online,
                version: 0,
                url,
                root_json,
            }
        }

        pub fn url(&self) -> &str {
            &self.url
        }

        /// The root that bootstraps a client of this repository.
        pub fn root_json(&self) -> &[u8] {
            &self.root_json
        }

        /// Publishes trusted_root as the repository's trusted_root.json: a
        /// new version of targets, snapshot and timestamp.
        pub fn publish(&mut self, trusted_root: &[u8]) {
            self.version += 1;
            let v = self.version;
            let hash = hex::encode(sha2::Sha256::digest(trusted_root));
            let targets = self.targets.envelope(json!({
                "_type": "targets",
                "spec_version": SPEC_VERSION,
                "version": v,
                "expires": FAR_FUTURE,
                "targets": {
                    "trusted_root.json": {"length": trusted_root.len(), "hashes": {"sha256": hash}},
                },
            }));
            let snapshot = self.snapshot.envelope(json!({
                "_type": "snapshot",
                "spec_version": SPEC_VERSION,
                "version": v,
                "expires": FAR_FUTURE,
                "meta": {"targets.json": meta(v, &targets)},
            }));
            let timestamp = self.timestamp.envelope(json!({
                "_type": "timestamp",
                "spec_version": SPEC_VERSION,
                "version": v,
                "expires": FAR_FUTURE,
                "meta": {"snapshot.json": meta(v, &snapshot)},
            }));
            let mut files = self.files.lock().expect("poisoned");
            files.insert(
                format!("targets/{hash}.trusted_root.json"),
                trusted_root.to_vec(),
            );
            files.insert(format!("{v}.targets.json"), targets);
            files.insert(format!("{v}.snapshot.json"), snapshot);
            files.insert("timestamp.json".to_string(), timestamp);
        }

        /// Serves a newer timestamp that has already expired: a client
        /// takes it (the version moved) and refuses the repository.
        pub fn expire_timestamp(&mut self) {
            let mut files = self.files.lock().expect("poisoned");
            let snapshot = files
                .get(&format!("{}.snapshot.json", self.version))
                .expect("a published snapshot")
                .clone();
            self.version += 1;
            let timestamp = self.timestamp.envelope(json!({
                "_type": "timestamp",
                "spec_version": SPEC_VERSION,
                "version": self.version,
                "expires": IN_THE_PAST,
                "meta": {"snapshot.json": meta(self.version - 1, &snapshot)},
            }));
            files.insert("timestamp.json".to_string(), timestamp);
        }

        /// Offline, the server answers 503 to everything.
        pub fn set_online(&self, online: bool) {
            self.online.store(online, Ordering::SeqCst);
        }

        /// A source of this repository caching under cache_dir.
        pub fn source(&self, cache_dir: impl Into<std::path::PathBuf>) -> crate::trustroot::Source {
            crate::trustroot::Source::custom("test", &self.url, self.root_json.clone(), cache_dir)
        }
    }

    /// A snapshot or timestamp meta entry for a metadata file.
    fn meta(version: u64, bytes: &[u8]) -> Value {
        json!({
            "version": version,
            "length": bytes.len(),
            "hashes": {"sha256": hex::encode(sha2::Sha256::digest(bytes))},
        })
    }

    /// Serves files on loopback: GET /<path> answers the file or 404, and
    /// 503 while offline. Returns the base URL.
    fn serve_files(files: Arc<Mutex<HashMap<String, Vec<u8>>>>, online: Arc<AtomicBool>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = conn.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .trim_start_matches('/')
                    .to_string();
                let (status, body) = if !online.load(Ordering::SeqCst) {
                    ("503 Service Unavailable", Vec::new())
                } else {
                    match files.lock().expect("poisoned").get(&path) {
                        Some(bytes) => ("200 OK", bytes.clone()),
                        None => ("404 Not Found", Vec::new()),
                    }
                };
                let _ = conn.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = conn.write_all(&body);
            }
        });
        format!("http://{addr}")
    }
}
