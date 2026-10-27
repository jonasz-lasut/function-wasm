//! The suite's module registry: `registry:3` in the cluster, behind Basic
//! auth and TLS from a CA this module generates, and the small distribution
//! client that pushes to it through a port-forward. The runtime speaks plain
//! HTTP to loopback only, so the registry is reached by its in-cluster name
//! with a certificate the function pod trusts through SSL_CERT_FILE - the
//! private-CA shape operators run - while the pusher here trusts the same CA
//! explicitly.

use anyhow::{Context as _, Result, anyhow, bail};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair, SanType,
};
use sha2::Digest as _;

/// Where the registry runs and how the cluster names it.
pub const NAMESPACE: &str = "e2e";
pub const SERVICE: &str = "registry";
pub const PORT: u16 = 5000;
pub const SELECTOR: &str = "app=registry";
/// The Basic credential; the htpasswd line is bcrypt of the password, as
/// `registry:3` requires.
pub const USERNAME: &str = "e2e";
pub const PASSWORD: &str = "e2e-password";
pub const HTPASSWD: &str = "e2e:$2y$05$ZSvcuizala2RPlOfObqCLOmAK9IKVzpQQGWUjYajVe40mnvNkdW8m\n";

pub const WASM_MEDIA_TYPE: &str = "application/wasm";
pub const EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
pub const BUNDLE_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";

/// The registry as the cluster names it: a bare host for egress rules,
/// host:port for module references.
pub fn host() -> String {
    format!("{SERVICE}.{NAMESPACE}.svc")
}

pub fn address() -> String {
    format!("{}:{PORT}", host())
}

/// The registry's TLS material: a CA and a server certificate for every
/// name the registry is reached by, loopback included for the port-forward.
pub struct Tls {
    pub ca_pem: String,
    pub cert_pem: String,
    pub key_pem: String,
}

pub fn tls() -> Result<Tls> {
    let ca_key = KeyPair::generate()?;
    let mut ca = CertificateParams::default();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name = DistinguishedName::new();
    ca.distinguished_name
        .push(DnType::CommonName, "function-wasm e2e registry CA");
    let ca_cert = ca.self_signed(&ca_key)?;
    let issuer = Issuer::from_params(&ca, &ca_key);

    let key = KeyPair::generate()?;
    let mut server = CertificateParams::new(vec![
        host(),
        format!("{}.cluster.local", host()),
        format!("{SERVICE}.{NAMESPACE}"),
        SERVICE.to_string(),
        "localhost".to_string(),
    ])?;
    server
        .subject_alt_names
        .push(SanType::IpAddress(std::net::Ipv4Addr::LOCALHOST.into()));
    server.distinguished_name = DistinguishedName::new();
    server.distinguished_name.push(DnType::CommonName, host());
    let cert = server.signed_by(&key, &issuer)?;
    Ok(Tls {
        ca_pem: ca_cert.pem(),
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    })
}

pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes)))
}

/// An OCI artifact ready to push: its manifest and the blobs it names.
pub struct Artifact {
    pub manifest: Vec<u8>,
    pub media_type: &'static str,
    pub blobs: Vec<(String, Vec<u8>)>,
}

impl Artifact {
    pub fn digest(&self) -> String {
        digest_of(&self.manifest)
    }
}

/// A module artifact in the shape `guestfn push` publishes: a wasm layer and
/// a manifest layer under an empty config.
pub fn module_artifact(wasm: &[u8], module_manifest: &[u8]) -> Artifact {
    let config = b"{}".to_vec();
    let layers = serde_json::json!([
        {"mediaType": WASM_MEDIA_TYPE, "digest": digest_of(wasm), "size": wasm.len()},
        {
            "mediaType": function_wasm::oci::MANIFEST_LAYER_TYPE,
            "digest": digest_of(module_manifest),
            "size": module_manifest.len(),
        },
    ]);
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": function_wasm::oci::OCI_MANIFEST_TYPE,
        "artifactType": WASM_MEDIA_TYPE,
        "config": {"mediaType": EMPTY_CONFIG_MEDIA_TYPE, "digest": digest_of(&config), "size": config.len()},
        "layers": layers,
    }))
    .expect("manifest json");
    Artifact {
        manifest,
        media_type: function_wasm::oci::OCI_MANIFEST_TYPE,
        blobs: vec![
            (digest_of(wasm), wasm.to_vec()),
            (digest_of(module_manifest), module_manifest.to_vec()),
            (digest_of(&config), config),
        ],
    }
}

/// A cosign 3.1 referrer carrying a Sigstore bundle for subject.
pub fn referrer(subject: &str, bundle: &[u8]) -> Artifact {
    let config = b"{}".to_vec();
    let predicate_type = serde_json::from_slice::<serde_json::Value>(bundle)
        .ok()
        .and_then(|b| {
            let payload = b["dsseEnvelope"]["payload"].as_str()?;
            let payload =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, payload).ok()?;
            let statement: serde_json::Value = serde_json::from_slice(&payload).ok()?;
            statement["predicateType"].as_str().map(str::to_string)
        })
        .unwrap_or_default();
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": function_wasm::oci::OCI_MANIFEST_TYPE,
        "artifactType": BUNDLE_MEDIA_TYPE,
        "config": {"mediaType": EMPTY_CONFIG_MEDIA_TYPE, "digest": digest_of(&config), "size": config.len()},
        "layers": [{"mediaType": BUNDLE_MEDIA_TYPE, "digest": digest_of(bundle), "size": bundle.len()}],
        "annotations": {
            "dev.sigstore.bundle.content": "dsse-envelope",
            "dev.sigstore.bundle.predicateType": predicate_type,
        },
        "subject": {"mediaType": function_wasm::oci::OCI_MANIFEST_TYPE, "digest": subject, "size": 0},
    }))
    .expect("referrer json");
    Artifact {
        manifest,
        media_type: function_wasm::oci::OCI_MANIFEST_TYPE,
        blobs: vec![
            (digest_of(bundle), bundle.to_vec()),
            (digest_of(&config), config),
        ],
    }
}

/// Pushes artifacts to the registry over HTTPS with Basic auth, trusting
/// the suite's CA.
pub struct Pusher {
    base: String,
    http: reqwest::Client,
}

impl Pusher {
    pub fn new(base: &str, ca_pem: &str) -> Result<Pusher> {
        let http = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(ca_pem.as_bytes())?)
            .build()?;
        Ok(Pusher {
            base: base.trim_end_matches('/').to_string(),
            http,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Pushes every blob, then the manifest under reference (a tag or a
    /// digest); returns the manifest digest.
    pub async fn push(
        &self,
        repository: &str,
        reference: &str,
        artifact: &Artifact,
    ) -> Result<String> {
        for (digest, data) in &artifact.blobs {
            self.push_blob(repository, digest, data).await?;
        }
        let url = self.url(&format!("/v2/{repository}/manifests/{reference}"));
        let rsp = self
            .http
            .put(&url)
            .basic_auth(USERNAME, Some(PASSWORD))
            .header("Content-Type", artifact.media_type)
            .body(artifact.manifest.clone())
            .send()
            .await
            .with_context(|| format!("PUT {url}"))?;
        if rsp.status() != 201 {
            bail!(
                "PUT {url}: {} {}",
                rsp.status(),
                rsp.text().await.unwrap_or_default()
            );
        }
        Ok(artifact.digest())
    }

    /// Attaches a referrer to its subject the way cosign does on a registry
    /// without the referrers API (`registry:3`, GHCR): the referrer manifest
    /// pushed by digest, then listed in the subject's `sha256-<hex>` tag
    /// index, which the runtime reads when the API answers 404.
    pub async fn attach(&self, repository: &str, subject: &str, referrer: &Artifact) -> Result<()> {
        let digest = self.push(repository, &referrer.digest(), referrer).await?;
        let manifest: serde_json::Value = serde_json::from_slice(&referrer.manifest)?;
        let mut descriptor = serde_json::json!({
            "mediaType": referrer.media_type,
            "digest": digest,
            "size": referrer.manifest.len(),
            "artifactType": manifest["artifactType"].as_str().unwrap_or_default(),
        });
        if let Some(annotations) = manifest.get("annotations") {
            descriptor["annotations"] = annotations.clone();
        }
        let tag = subject.replacen(':', "-", 1);
        let url = self.url(&format!("/v2/{repository}/manifests/{tag}"));
        let existing = self
            .http
            .get(&url)
            .basic_auth(USERNAME, Some(PASSWORD))
            .header("Accept", function_wasm::oci::OCI_INDEX_TYPE)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let mut index = match existing.status().as_u16() {
            200 => existing
                .json::<serde_json::Value>()
                .await
                .context("referrers index")?,
            404 => serde_json::json!({
                "schemaVersion": 2,
                "mediaType": function_wasm::oci::OCI_INDEX_TYPE,
                "manifests": [],
            }),
            other => bail!("GET {url}: {other}"),
        };
        let manifests = index["manifests"]
            .as_array_mut()
            .context("index manifests")?;
        if !manifests.iter().any(|m| m["digest"] == digest) {
            manifests.push(descriptor);
        }
        let index = Artifact {
            manifest: serde_json::to_vec(&index)?,
            media_type: function_wasm::oci::OCI_INDEX_TYPE,
            blobs: Vec::new(),
        };
        self.push(repository, &tag, &index).await?;
        Ok(())
    }

    async fn push_blob(&self, repository: &str, digest: &str, data: &[u8]) -> Result<()> {
        let head = self.url(&format!("/v2/{repository}/blobs/{digest}"));
        let exists = self
            .http
            .head(&head)
            .basic_auth(USERNAME, Some(PASSWORD))
            .send()
            .await
            .with_context(|| format!("HEAD {head}"))?;
        if exists.status() == 200 {
            return Ok(());
        }
        let start = self.url(&format!("/v2/{repository}/blobs/uploads/"));
        let started = self
            .http
            .post(&start)
            .basic_auth(USERNAME, Some(PASSWORD))
            .send()
            .await
            .with_context(|| format!("POST {start}"))?;
        if started.status() != 202 {
            bail!("POST {start}: {}", started.status());
        }
        let location = started
            .headers()
            .get("Location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow!("POST {start}: no Location header"))?
            .to_string();
        let location = if location.starts_with("http") {
            location
        } else {
            self.url(&location)
        };
        let separator = if location.contains('?') { '&' } else { '?' };
        let put = format!("{location}{separator}digest={digest}");
        let done = self
            .http
            .put(&put)
            .basic_auth(USERNAME, Some(PASSWORD))
            .header("Content-Type", "application/octet-stream")
            .body(data.to_vec())
            .send()
            .await
            .with_context(|| format!("PUT {put}"))?;
        if done.status() != 201 {
            bail!(
                "PUT {put}: {} {}",
                done.status(),
                done.text().await.unwrap_or_default()
            );
        }
        Ok(())
    }
}
