//! The OCI module source - the Rust port of `internal/module`'s oci.go and
//! auth.go over a hand-rolled distribution client (the Rust ecosystem has
//! no go-containerregistry equal): the reference pins the manifest, the
//! manifest pins its layer, and the layer is the module. Resolution touches
//! no registry; the manifest is fetched inside fetch and verified against
//! the pinned digest, layers are verified against theirs through the blob
//! store, a wasm-typed layer is the module and a tar layer must hold it at
//! exactly /fn.wasm. Auth is anonymous, Basic, or the Bearer token flow,
//! with credentials from the pipeline step or the local Docker config.

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use base64::Engine as _;
use serde::Deserialize;

use crate::location::OciReference;

/// Media types recognised as a raw wasm module layer: the CNCF wasm OCI
/// artifact layout and the older wasm-to-oci / Spin / wasmCloud conventions.
const WASM_LAYER_TYPES: &[&str] = &[
    "application/wasm",
    "application/vnd.wasm.content.layer.v1+wasm",
    "application/vnd.module.wasm.content.layer.v1+wasm",
];

/// The media type of the artifact layer carrying the module manifest.
pub const MANIFEST_LAYER_TYPE: &str = "application/vnd.wasmfn.manifest.v1+json";

/// Where a FROM scratch image must hold the module: `COPY fn.wasm /`. The
/// one path looked for in a tar layer; nothing is guessed and the name is
/// not configurable.
pub const SCRATCH_MODULE_PATH: &str = "/fn.wasm";

/// One registry credential.
#[derive(Debug, Clone)]
pub struct Auth {
    pub username: String,
    pub password: String,
}

/// An artifact's parsed OCI manifest - the fields the layer rules read.
#[derive(Debug, Deserialize)]
pub struct OciManifest {
    #[serde(default, rename = "mediaType")]
    pub media_type: String,
    #[serde(default)]
    pub layers: Vec<Descriptor>,
    #[serde(default)]
    manifests: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Descriptor {
    #[serde(default, rename = "mediaType")]
    pub media_type: String,
    #[serde(default)]
    pub digest: String,
}

/// A distribution-API client for one artifact's registry.
pub struct RegistryClient {
    http: reqwest::blocking::Client,
    base: String,
    repository: String,
    auth: Option<Auth>,
    token: std::sync::Mutex<Option<String>>,
}

impl RegistryClient {
    pub fn new(reference: &OciReference, auth: Option<Auth>) -> RegistryClient {
        // Docker Hub's API host differs from its policy-location name, and a
        // local registry (tests, kind clusters) speaks plain HTTP - the same
        // carve-outs go-containerregistry makes.
        let host = if reference.registry == "index.docker.io" {
            "registry-1.docker.io"
        } else {
            &reference.registry
        };
        let scheme = if host.starts_with("localhost")
            || host.starts_with("127.0.0.1")
            || host.starts_with("[::1]")
        {
            "http"
        } else {
            "https"
        };
        RegistryClient {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5 * 60))
                .build()
                .expect("the registry client's configuration is static"),
            base: format!("{scheme}://{host}"),
            repository: reference.repository.clone(),
            auth,
            token: std::sync::Mutex::new(None),
        }
    }

    /// Fetches and parses the artifact's manifest, verified against the
    /// pinned digest - the root of the trust chain. An image index is
    /// refused: the reference must name the manifest holding the module.
    pub fn manifest(&self, reference: &OciReference) -> Result<OciManifest, String> {
        let url = format!(
            "{}/v2/{}/manifests/{}",
            self.base, self.repository, reference.digest
        );
        let accept = "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json";
        let raw = self
            .get(&url, Some(accept), usize::MAX)
            .map_err(|e| format!("cannot fetch manifest {}: {e}", reference.digest))?;
        let got = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&raw)));
        if got != reference.digest {
            return Err(format!(
                "manifest content is {got}, want {}",
                reference.digest
            ));
        }
        let m: OciManifest = serde_json::from_slice(&raw)
            .map_err(|e| format!("cannot parse manifest {}: {e}", reference.digest))?;
        if m.media_type.contains("index")
            || m.media_type.contains("manifest.list")
            || !m.manifests.is_empty()
        {
            return Err(format!(
                "{}/{}@{} is an image index; reference the manifest holding the module",
                reference.registry, reference.repository, reference.digest
            ));
        }
        Ok(m)
    }

    /// Fetches one layer blob by digest, bounded to limit; the caller
    /// verifies the content against the digest through the blob store.
    pub fn blob(&self, digest: &str, limit: u64, label: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}/v2/{}/blobs/{digest}", self.base, self.repository);
        self.get(&url, None, limit as usize)
            .map_err(|e| format!("cannot fetch {label}: {e}"))
    }

    /// One authenticated GET: anonymous first, then the challenge the
    /// registry answers with - Basic, or the Bearer token flow (the token
    /// endpoint queried with the credential, the request retried with the
    /// token, which is cached for the artifact's other requests).
    fn get(&self, url: &str, accept: Option<&str>, limit: usize) -> Result<Vec<u8>, String> {
        let attempt = |bearer: Option<&str>| -> Result<reqwest::blocking::Response, String> {
            let mut req = self.http.get(url);
            if let Some(a) = accept {
                req = req.header("Accept", a);
            }
            if let Some(t) = bearer {
                req = req.bearer_auth(t);
            } else if let Some(auth) = &self.auth {
                req = req.basic_auth(&auth.username, Some(&auth.password));
            }
            req.send().map_err(|e| e.to_string())
        };

        let cached = self.token.lock().expect("poisoned").clone();
        let mut rsp = attempt(cached.as_deref())?;
        if rsp.status() == reqwest::StatusCode::UNAUTHORIZED {
            let challenge = rsp
                .headers()
                .get("www-authenticate")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if let Some(token) = self.bearer_token(&challenge)? {
                *self.token.lock().expect("poisoned") = Some(token.clone());
                rsp = attempt(Some(&token))?;
            }
        }
        if !rsp.status().is_success() {
            return Err(rsp.status().to_string());
        }
        let mut out = Vec::new();
        let mut limited = rsp.take((limit as u64).saturating_add(1));
        limited.read_to_end(&mut out).map_err(|e| e.to_string())?;
        if out.len() > limit {
            return Err(format!("module exceeds the size limit of {limit} bytes"));
        }
        Ok(out)
    }

    /// Answers a Bearer challenge: GET realm?service=&scope=, with the
    /// credential as Basic when there is one, and read token/access_token.
    fn bearer_token(&self, challenge: &str) -> Result<Option<String>, String> {
        let Some(rest) = challenge.strip_prefix("Bearer ") else {
            return Ok(None);
        };
        let mut fields: HashMap<&str, &str> = HashMap::new();
        for part in rest.split(',') {
            if let Some((k, v)) = part.trim().split_once('=') {
                fields.insert(k, v.trim_matches('"'));
            }
        }
        let Some(realm) = fields.get("realm") else {
            return Ok(None);
        };
        let scope = fields
            .get("scope")
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("repository:{}:pull", self.repository));
        let mut req = self.http.get(*realm).query(&[
            (
                "service",
                fields.get("service").copied().unwrap_or_default(),
            ),
            ("scope", scope.as_str()),
        ]);
        if let Some(auth) = &self.auth {
            req = req.basic_auth(&auth.username, Some(&auth.password));
        }
        let rsp = req
            .send()
            .map_err(|e| format!("cannot fetch registry token: {e}"))?;
        if !rsp.status().is_success() {
            return Err(format!("cannot fetch registry token: {}", rsp.status()));
        }
        #[derive(Deserialize)]
        struct Token {
            #[serde(default)]
            token: String,
            #[serde(default)]
            access_token: String,
        }
        let t: Token = rsp
            .json()
            .map_err(|e| format!("cannot parse registry token: {e}"))?;
        let token = if t.token.is_empty() {
            t.access_token
        } else {
            t.token
        };
        Ok((!token.is_empty()).then_some(token))
    }
}

/// Picks the layer of a manifest that holds the module: a wasm-typed layer
/// if there is one, otherwise the only layer that is not the
/// module-manifest layer.
pub fn wasm_layer(m: &OciManifest) -> Result<Descriptor, String> {
    let mut candidates = Vec::new();
    for l in &m.layers {
        if WASM_LAYER_TYPES.contains(&l.media_type.as_str()) {
            return Ok(l.clone());
        }
        if l.media_type != MANIFEST_LAYER_TYPE {
            candidates.push(l.clone());
        }
    }
    if candidates.len() == 1 {
        return Ok(candidates.remove(0));
    }
    Err(format!(
        "has {} layers and none is a wasm layer",
        m.layers.len()
    ))
}

/// Picks the module-manifest layer of an artifact's manifest, if any.
pub fn manifest_layer(m: &OciManifest) -> Option<Descriptor> {
    m.layers
        .iter()
        .find(|l| l.media_type == MANIFEST_LAYER_TYPE)
        .cloned()
}

/// Whether a layer media type is a tar archive - a FROM scratch image
/// holding the module as a file - rather than a raw module.
pub fn is_tar_layer(media_type: &str) -> bool {
    media_type.contains("tar")
}

/// Returns SCRATCH_MODULE_PATH from a (possibly gzipped) tar layer; an
/// archive without that exact entry is refused, whatever else it holds. The
/// archive may expand to at most eight times limit before the entry is
/// found, so a gzip bomb costs bounded work.
pub fn extract_wasm(b: &[u8], limit: u64) -> Result<Vec<u8>, String> {
    let reader: Box<dyn Read> = if b.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(b))
    } else {
        Box::new(b)
    };
    let capped = CappedReader {
        r: reader,
        left: 8 * limit as i64,
    };
    let mut archive = tar::Archive::new(capped);
    let entries = archive
        .entries()
        .map_err(|e| format!("cannot read module layer archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("cannot read module layer archive: {e}"))?;
        let is_file = entry.header().entry_type().is_file();
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Builders name the entry fn.wasm, ./fn.wasm or /fn.wasm; all are
        // the root's fn.wasm, and nothing else is.
        let cleaned = format!("/{}", name.trim_start_matches(['.', '/']));
        if is_file && cleaned == SCRATCH_MODULE_PATH {
            let mut out = Vec::new();
            let mut limited = (&mut entry).take(limit + 1);
            limited
                .read_to_end(&mut out)
                .map_err(|e| format!("cannot read module layer archive: {e}"))?;
            if out.len() as u64 > limit {
                return Err(format!("module exceeds the size limit of {limit} bytes"));
            }
            return Ok(out);
        }
    }
    Err(format!(
        "module layer is a tar archive without {SCRATCH_MODULE_PATH}: a FROM scratch image must COPY the module to {SCRATCH_MODULE_PATH}"
    ))
}

struct CappedReader<R> {
    r: R,
    left: i64,
}

impl<R: Read> Read for CappedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left <= 0 {
            return Err(std::io::Error::other(format!(
                "module layer archive exceeds the size limit before {SCRATCH_MODULE_PATH}"
            )));
        }
        let cap = (self.left as usize).min(buf.len());
        let n = self.r.read(&mut buf[..cap])?;
        self.left -= n as i64;
        Ok(n)
    }
}

/// The authenticator for a pipeline-step credential: a .dockerconfigjson
/// key, or username and password keys - the port of module.AuthFor.
pub fn auth_for(registry: &str, data: &HashMap<String, Vec<u8>>) -> Result<Auth, String> {
    if let Some(raw) = data.get(".dockerconfigjson") {
        return docker_config_auth(registry, raw)
            .ok_or_else(|| format!("the .dockerconfigjson carries no entry for {registry:?}"));
    }
    let field = |k: &str| {
        data.get(k)
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .filter(|v| !v.is_empty())
    };
    match (field("username"), field("password")) {
        (Some(username), Some(password)) => Ok(Auth { username, password }),
        _ => Err(
            "the credential carries neither a .dockerconfigjson nor username and password keys"
                .to_string(),
        ),
    }
}

/// The local Docker config's credential for a registry, if any - the
/// keychain fallback when the step names no credential. Credential helpers
/// are not consulted.
pub fn keychain_auth(registry: &str) -> Option<Auth> {
    let path = std::env::var_os("DOCKER_CONFIG")
        .map(|d| std::path::PathBuf::from(d).join("config.json"))
        .or_else(|| std::env::home_dir().map(|h| h.join(".docker/config.json")))?;
    let raw = std::fs::read(path).ok()?;
    docker_config_auth(registry, &raw)
}

fn docker_config_auth(registry: &str, raw: &[u8]) -> Option<Auth> {
    #[derive(Deserialize)]
    struct Config {
        #[serde(default)]
        auths: HashMap<String, Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        #[serde(default)]
        auth: String,
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: String,
    }
    let config: Config = serde_json::from_slice(raw).ok()?;
    // The registry appears under several spellings; Docker Hub's legacy key
    // included.
    let keys = [
        registry.to_string(),
        format!("https://{registry}"),
        format!("http://{registry}"),
        "https://index.docker.io/v1/".to_string(),
    ];
    let entry = keys.iter().find_map(|k| {
        if k == "https://index.docker.io/v1/" && registry != "index.docker.io" {
            return None;
        }
        config.auths.get(k)
    })?;
    if !entry.auth.is_empty()
        && let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(&entry.auth)
        && let Ok(text) = String::from_utf8(decoded)
        && let Some((u, p)) = text.split_once(':')
    {
        return Some(Auth {
            username: u.to_string(),
            password: p.to_string(),
        });
    }
    if !entry.username.is_empty() {
        return Some(Auth {
            username: entry.username.clone(),
            password: entry.password.clone(),
        });
    }
    None
}

use sha2::Digest as _;

#[cfg(test)]
pub(crate) mod testregistry {
    //! A minimal distribution registry for tests: manifests and blobs by
    //! digest, optionally behind the Bearer token flow.

    use std::collections::HashMap;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Arc;

    pub struct TestRegistry {
        pub manifests: HashMap<String, Vec<u8>>,
        pub blobs: HashMap<String, Vec<u8>>,
        pub bearer: bool,
    }

    pub fn serve(registry: TestRegistry) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let registry = Arc::new(registry);
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut conn = conn;
                let mut buf = [0u8; 4096];
                let n = std::io::Read::read(&mut conn, &mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let authorized = !registry.bearer
                    || head.contains("Bearer testtoken")
                    || path.starts_with("/token");
                let (status, body): (&str, Vec<u8>) = if !authorized {
                    (
                        "401 Unauthorized\r\nWww-Authenticate: Bearer realm=\"http://REALM/token\",service=\"registry\"",
                        b"{}".to_vec(),
                    )
                } else if path.starts_with("/token") {
                    ("200 OK", br#"{"token":"testtoken"}"#.to_vec())
                } else if let Some(digest) = path.split("/manifests/").nth(1) {
                    match registry.manifests.get(digest) {
                        Some(m) => ("200 OK", m.clone()),
                        None => ("404 Not Found", Vec::new()),
                    }
                } else if let Some(digest) = path.split("/blobs/").nth(1) {
                    match registry.blobs.get(digest) {
                        Some(b) => ("200 OK", b.clone()),
                        None => ("404 Not Found", Vec::new()),
                    }
                } else {
                    ("200 OK", Vec::new())
                };
                let status = status.replace("REALM", &addr.to_string());
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = conn.write_all(header.as_bytes());
                let _ = conn.write_all(&body);
            }
        });
        addr.to_string()
    }

    pub fn digest_of(b: &[u8]) -> String {
        use sha2::Digest as _;
        format!("sha256:{}", hex::encode(sha2::Sha256::digest(b)))
    }

    /// A single-wasm-layer artifact; returns (manifest digest, registry).
    pub fn wasm_artifact(
        wasm: &[u8],
        module_manifest: Option<&[u8]>,
        bearer: bool,
    ) -> (String, String) {
        let mut blobs = HashMap::new();
        let wasm_digest = digest_of(wasm);
        blobs.insert(wasm_digest.clone(), wasm.to_vec());
        let mut layers = vec![serde_json::json!({
            "mediaType": "application/wasm",
            "digest": wasm_digest,
            "size": wasm.len(),
        })];
        if let Some(m) = module_manifest {
            let d = digest_of(m);
            blobs.insert(d.clone(), m.to_vec());
            layers.push(serde_json::json!({
                "mediaType": super::MANIFEST_LAYER_TYPE,
                "digest": d,
                "size": m.len(),
            }));
        }
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(b"{}"), "size": 2},
            "layers": layers,
        }))
        .expect("manifest json");
        let manifest_digest = digest_of(&manifest);
        let mut manifests = HashMap::new();
        manifests.insert(manifest_digest.clone(), manifest);
        let addr = serve(TestRegistry {
            manifests,
            blobs,
            bearer,
        });
        (manifest_digest, addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::location::parse_oci_reference;

    fn reference(addr: &str, digest: &str) -> OciReference {
        parse_oci_reference(&format!("{addr}/example/greeter@{digest}")).expect("reference")
    }

    #[test]
    fn fetches_a_wasm_layer_artifact() {
        let (digest, addr) = testregistry::wasm_artifact(b"fake wasm", None, false);
        let r = reference(&addr, &digest);
        let client = RegistryClient::new(&r, None);
        let m = client.manifest(&r).expect("manifest");
        let layer = wasm_layer(&m).expect("layer");
        let blob = client
            .blob(&layer.digest, 1 << 20, "module layer")
            .expect("blob");
        assert_eq!(blob, b"fake wasm");
        assert!(manifest_layer(&m).is_none());
    }

    #[test]
    fn fetches_through_the_bearer_token_flow() {
        let (digest, addr) = testregistry::wasm_artifact(b"fake wasm", Some(br#"{"abi":1}"#), true);
        let r = reference(&addr, &digest);
        let client = RegistryClient::new(&r, None);
        let m = client.manifest(&r).expect("manifest");
        let layer = wasm_layer(&m).expect("layer");
        assert_eq!(
            client
                .blob(&layer.digest, 1 << 20, "module layer")
                .expect("blob"),
            b"fake wasm"
        );
        let mlayer = manifest_layer(&m).expect("manifest layer");
        assert_eq!(
            client
                .blob(&mlayer.digest, 1 << 20, "manifest layer")
                .expect("blob"),
            br#"{"abi":1}"#
        );
    }

    #[test]
    fn a_wrong_manifest_digest_is_refused() {
        let (_, addr) = testregistry::wasm_artifact(b"fake wasm", None, false);
        let bogus = format!("sha256:{}", "1".repeat(64));
        let r = reference(&addr, &bogus);
        let client = RegistryClient::new(&r, None);
        let err = client.manifest(&r).expect_err("refuse");
        assert!(err.starts_with("cannot fetch manifest"), "{err}");
    }

    #[test]
    fn tar_layers_must_hold_fn_wasm_exactly() {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(9);
        header.set_cksum();
        builder
            .append_data(&mut header, "fn.wasm", &b"fake wasm"[..])
            .expect("append");
        let archive = builder.into_inner().expect("archive");
        assert_eq!(
            extract_wasm(&archive, 1 << 20).expect("extract"),
            b"fake wasm"
        );

        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_cksum();
        builder
            .append_data(&mut header, "other.wasm", &b"nope!"[..])
            .expect("append");
        let archive = builder.into_inner().expect("archive");
        let err = extract_wasm(&archive, 1 << 20).expect_err("refuse");
        assert!(err.contains("without /fn.wasm"), "{err}");
    }

    #[test]
    fn step_credentials_resolve() {
        let mut data = HashMap::new();
        data.insert("username".to_string(), b"u".to_vec());
        data.insert("password".to_string(), b"p".to_vec());
        let auth = auth_for("ghcr.io", &data).expect("basic");
        assert_eq!((auth.username.as_str(), auth.password.as_str()), ("u", "p"));

        let mut data = HashMap::new();
        data.insert(
            ".dockerconfigjson".to_string(),
            br#"{"auths":{"ghcr.io":{"auth":"dTpw"}}}"#.to_vec(),
        );
        let auth = auth_for("ghcr.io", &data).expect("dockerconfig");
        assert_eq!((auth.username.as_str(), auth.password.as_str()), ("u", "p"));
        assert!(auth_for("other.io", &data).is_err());
    }
}
