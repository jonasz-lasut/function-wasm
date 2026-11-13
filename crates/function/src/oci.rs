//! The OCI module source over a hand-rolled distribution client (the Rust
//! ecosystem has no equal of a full container-registry library): the reference pins the manifest, the
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
use sha2::Digest as _;

use crate::location::OciReference;

/// Media types recognised as a raw wasm module layer: the CNCF wasm OCI
/// artifact layout and the older wasm-to-oci / Spin / wasmCloud conventions.
const WASM_LAYER_TYPES: &[&str] = &[
    "application/wasm",
    "application/vnd.wasm.content.layer.v1+wasm",
    "application/vnd.module.wasm.content.layer.v1+wasm",
];

/// The media type of the artifact layer carrying the module manifest.
pub const MANIFEST_LAYER_TYPE: &str = "application/vnd.guestfn.manifest.v1+json";

/// An OCI image becomes an `oci@0.1` module by a layer `guestfn push
/// --image` appends to it (docs/one-pager-oci-runtime.md): a plain tar
/// layer with its diff_id in the image config like any other, holding the
/// module manifest at MANIFEST_LAYER_PATH, marked by this annotation on its
/// descriptor. The image stays what every container runtime pulls and runs;
/// the Input pins the appended image's digest, one signature covers image
/// and manifest, and the runtime reads the manifest from that one layer
/// before it pulls the rest.
pub const MANIFEST_LAYER_ANNOTATION: &str = "io.crossplane.guestfn";
pub const MANIFEST_LAYER_ANNOTATION_VALUE: &str = "manifest";
pub const MANIFEST_LAYER_PATH: &str = "/.guestfn/manifest.json";
/// OCI 1.1's empty descriptor: the config of an artifact that has none.
pub const EMPTY_CONFIG_TYPE: &str = "application/vnd.oci.empty.v1+json";
pub const EMPTY_CONFIG: &[u8] = b"{}";

/// The media types of an OCI image manifest and image index.
pub const OCI_MANIFEST_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
pub const OCI_INDEX_TYPE: &str = "application/vnd.oci.image.index.v1+json";

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
    /// OCI 1.1's artifact type, when the publisher set one.
    #[serde(default, rename = "artifactType")]
    pub artifact_type: String,
    #[serde(default)]
    pub config: Option<Descriptor>,
    #[serde(default)]
    pub layers: Vec<Descriptor>,
    #[serde(default)]
    pub annotations: std::collections::BTreeMap<String, String>,
    /// The manifest this one refers to (OCI 1.1): what makes it a referrer.
    #[serde(default)]
    pub subject: Option<Descriptor>,
    #[serde(default)]
    manifests: Vec<serde_json::Value>,
}

/// An image index - here, a referrers listing.
#[derive(Debug, Deserialize)]
struct OciIndex {
    #[serde(default)]
    manifests: Vec<Descriptor>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Descriptor {
    #[serde(default, rename = "mediaType")]
    pub media_type: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub annotations: std::collections::BTreeMap<String, String>,
    /// An index entry's platform.
    #[serde(default)]
    pub platform: Option<Platform>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Platform {
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub os: String,
}

/// The media types of an image's config: what makes a manifest an image
/// rather than an artifact.
pub const IMAGE_CONFIG_TYPES: &[&str] = &[
    "application/vnd.oci.image.config.v1+json",
    "application/vnd.docker.container.image.v1+json",
];

impl OciManifest {
    /// Whether this is an image index (a manifest list).
    pub fn is_index(&self) -> bool {
        self.media_type == OCI_INDEX_TYPE
            || self.media_type.contains("manifest.list")
            || (self.media_type.is_empty() && !self.manifests.is_empty())
    }

    /// An index's entries, with their platforms.
    pub fn index_entries(&self) -> Vec<Descriptor> {
        self.manifests
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect()
    }

    /// Whether this is a container image: its config is an image config
    /// (the OCI runtime runs it as a manifest-less module).
    pub fn is_image(&self) -> bool {
        self.config
            .as_ref()
            .is_some_and(|c| IMAGE_CONFIG_TYPES.contains(&c.media_type.as_str()))
    }
}

/// A distribution-API client for one artifact's registry.
pub struct RegistryClient {
    http: reqwest::blocking::Client,
    base: String,
    repository: String,
    auth: Option<Auth>,
    token: std::sync::Mutex<Option<String>>,
}

/// The most a registry may answer a manifest request with: what the OCI
/// distribution specification asks a registry to accept, and far above any
/// manifest a module or an image has.
pub const MAX_MANIFEST_SIZE: usize = 4 << 20;

/// The most a token endpoint may answer with.
const MAX_TOKEN_SIZE: usize = 1 << 20;

/// Reads a response body of at most limit bytes; a longer one is refused,
/// naming what was fetched, without being read to its end.
fn read_bounded(body: impl Read, limit: usize, what: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    body.take((limit as u64).saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() > limit {
        return Err(format!("{what} exceeds the size limit of {limit} bytes"));
    }
    Ok(out)
}

impl RegistryClient {
    pub fn new(reference: &OciReference, auth: Option<Auth>) -> RegistryClient {
        // Docker Hub's API host differs from its policy-location name, and a
        // local registry (tests, kind clusters) speaks plain HTTP - the same
        // carve-outs container tooling makes.
        let host = if reference.registry == "index.docker.io" {
            "registry-1.docker.io"
        } else {
            &reference.registry
        };
        let scheme = scheme(host);
        RegistryClient {
            http: http_client(),
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
        let m = self.manifest_by_digest(&reference.digest, MAX_MANIFEST_SIZE)?;
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

    /// Fetches and parses a manifest by digest, verified against it and
    /// bounded to limit bytes.
    pub fn manifest_by_digest(&self, digest: &str, limit: usize) -> Result<OciManifest, String> {
        let url = format!("{}/v2/{}/manifests/{digest}", self.base, self.repository);
        let accept = "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json";
        let raw = self
            .get_as(&url, Some(accept), limit, "manifest")
            .map_err(|e| format!("cannot fetch manifest {digest}: {e}"))?;
        let got = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&raw)));
        if got != digest {
            return Err(format!("manifest content is {got}, want {digest}"));
        }
        serde_json::from_slice(&raw).map_err(|e| format!("cannot parse manifest {digest}: {e}"))
    }

    /// Lists the referrers of a manifest (OCI distribution 1.1): the
    /// referrers API, or - where the registry answers it 404, as GHCR and
    /// registry:3 do - the referrers tag schema's index, tagged
    /// sha256-<hex> and maintained by the pushing client; a missing tag is
    /// no referrers. Neither listing is digest-pinned (both change as
    /// referrers are pushed): a caller fetches each referrer by its digest
    /// and checks its subject.
    pub fn referrers(&self, digest: &str, limit: usize) -> Result<Vec<Descriptor>, String> {
        let api = format!("{}/v2/{}/referrers/{digest}", self.base, self.repository);
        let raw = match self.get_as(&api, Some(OCI_INDEX_TYPE), limit, "referrers index") {
            Ok(raw) => raw,
            Err(e) if e.starts_with("404") => {
                let tag = referrers_tag(digest);
                let url = format!("{}/v2/{}/manifests/{tag}", self.base, self.repository);
                match self.get_as(&url, Some(OCI_INDEX_TYPE), limit, "referrers index") {
                    Ok(raw) => raw,
                    Err(e) if e.starts_with("404") => return Ok(Vec::new()),
                    Err(e) => return Err(format!("cannot list the referrers of {digest}: {e}")),
                }
            }
            Err(e) => return Err(format!("cannot list the referrers of {digest}: {e}")),
        };
        let index: OciIndex = serde_json::from_slice(&raw)
            .map_err(|e| format!("cannot parse the referrers of {digest}: {e}"))?;
        Ok(index.manifests)
    }

    /// Fetches one layer blob by digest, bounded to limit; the caller
    /// verifies the content against the digest through the blob store.
    pub fn blob(&self, digest: &str, limit: u64, label: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}/v2/{}/blobs/{digest}", self.base, self.repository);
        self.get(&url, None, limit as usize)
            .map_err(|e| format!("cannot fetch {label}: {e}"))
    }

    /// Lists the repository's tags, bounded to limit bytes.
    pub fn tags(&self, limit: usize) -> Result<Vec<String>, String> {
        #[derive(Deserialize)]
        struct TagList {
            #[serde(default)]
            tags: Option<Vec<String>>,
        }
        let url = format!("{}/v2/{}/tags/list?n=1000", self.base, self.repository);
        let raw = self.get_as(&url, None, limit, "tag list")?;
        let list: TagList =
            serde_json::from_slice(&raw).map_err(|e| format!("cannot parse the tag list: {e}"))?;
        Ok(list.tags.unwrap_or_default())
    }

    /// Fetches a small blob read straight from the registry rather than
    /// through the blob store, bounded to limit and verified against its
    /// digest here.
    pub fn verified_blob(&self, digest: &str, limit: u64, label: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}/v2/{}/blobs/{digest}", self.base, self.repository);
        let raw = self
            .get_as(&url, None, limit as usize, label)
            .map_err(|e| format!("cannot fetch {label} {digest}: {e}"))?;
        let got = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&raw)));
        if got != digest {
            return Err(format!("{label} content is {got}, want {digest}"));
        }
        Ok(raw)
    }

    fn get(&self, url: &str, accept: Option<&str>, limit: usize) -> Result<Vec<u8>, String> {
        self.get_as(url, accept, limit, "module")
    }

    /// One authenticated GET: anonymous first, then the challenge the
    /// registry answers with - Basic, or the Bearer token flow (the token
    /// endpoint queried with the credential, the request retried with the
    /// token, which is cached for the artifact's other requests). A body
    /// over limit is refused, naming what was fetched.
    fn get_as(
        &self,
        url: &str,
        accept: Option<&str>,
        limit: usize,
        what: &str,
    ) -> Result<Vec<u8>, String> {
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
        read_bounded(rsp, limit, what)
    }

    /// Fetches a manifest by tag or digest and returns its raw bytes and
    /// content digest - what guestfn inspect and manifest show read.
    pub fn raw_manifest(&self, reference: &str) -> Result<(Vec<u8>, String), String> {
        let url = format!("{}/v2/{}/manifests/{reference}", self.base, self.repository);
        let accept = "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json";
        let raw = self
            .get_as(&url, Some(accept), MAX_MANIFEST_SIZE, "manifest")
            .map_err(|e| format!("cannot fetch manifest {reference}: {e}"))?;
        let digest = format!("sha256:{}", hex::encode(sha2::Sha256::digest(&raw)));
        Ok((raw, digest))
    }

    /// Uploads one blob (monolithic: POST an upload, PUT the bytes) - what
    /// guestfn push publishes layers and configs with.
    pub fn push_blob(&self, digest: &str, data: &[u8]) -> Result<(), String> {
        let url = format!("{}/v2/{}/blobs/uploads/", self.base, self.repository);
        let rsp = self
            .authed(|req| req, reqwest::Method::POST, &url, Vec::new(), None)
            .map_err(|e| format!("cannot start blob upload: {e}"))?;
        let location = rsp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or("registry answered the upload with no Location")?;
        let upload = if location.starts_with("http") {
            location.to_string()
        } else {
            format!("{}{}", self.base, location)
        };
        let sep = if upload.contains('?') { '&' } else { '?' };
        let upload = format!("{upload}{sep}digest={digest}");
        self.authed(
            |req| req.header("Content-Type", "application/octet-stream"),
            reqwest::Method::PUT,
            &upload,
            data.to_vec(),
            None,
        )
        .map_err(|e| format!("cannot upload blob {digest}: {e}"))?;
        Ok(())
    }

    /// Publishes a manifest under a tag or digest.
    pub fn push_manifest(
        &self,
        reference: &str,
        media_type: &str,
        data: &[u8],
    ) -> Result<(), String> {
        let url = format!("{}/v2/{}/manifests/{reference}", self.base, self.repository);
        self.authed(
            |req| req,
            reqwest::Method::PUT,
            &url,
            data.to_vec(),
            Some(media_type),
        )
        .map_err(|e| format!("cannot push manifest {reference}: {e}"))?;
        Ok(())
    }

    /// One authenticated non-GET request through the same anonymous ->
    /// challenge -> token dance as get.
    fn authed(
        &self,
        decorate: impl Fn(reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder,
        method: reqwest::Method,
        url: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<reqwest::blocking::Response, String> {
        let attempt = |bearer: Option<&str>| -> Result<reqwest::blocking::Response, String> {
            let mut req = self.http.request(method.clone(), url).body(body.clone());
            if let Some(ct) = content_type {
                req = req.header("Content-Type", ct);
            }
            req = decorate(req);
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
        Ok(rsp)
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
        let raw = read_bounded(rsp, MAX_TOKEN_SIZE, "the token response")
            .map_err(|e| format!("cannot fetch registry token: {e}"))?;
        let t: Token = serde_json::from_slice(&raw)
            .map_err(|e| format!("cannot parse registry token: {e}"))?;
        let token = if t.token.is_empty() {
            t.access_token
        } else {
            t.token
        };
        Ok((!token.is_empty()).then_some(token))
    }
}

/// The HTTP client behind every RegistryClient, one for the process: a
/// blocking reqwest client owns a thread and a connection pool, so sharing
/// it starts the thread once and lets a load's manifest, blob and referrer
/// requests reuse one connection and TLS session instead of each paying a
/// handshake. The client's configuration is static (a five-minute request
/// timeout); what differs per registry is the URL and the credential,
/// which travel on the request.
fn http_client() -> reqwest::blocking::Client {
    static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5 * 60))
                .build()
                .expect("the registry client's configuration is static")
        })
        .clone()
}

/// The referrers tag schema's tag for a manifest digest: sha256-<hex>.
pub fn referrers_tag(digest: &str) -> String {
    digest.replacen(':', "-", 1)
}

/// Whether a layer descriptor is the module manifest's: the manifest layer
/// of a WebAssembly artifact (its own media type) or the annotated tar
/// layer of an OCI module image.
pub fn is_manifest_layer(l: &Descriptor) -> bool {
    l.media_type == MANIFEST_LAYER_TYPE
        || (is_tar_layer(&l.media_type)
            && l.annotations
                .get(MANIFEST_LAYER_ANNOTATION)
                .is_some_and(|v| v == MANIFEST_LAYER_ANNOTATION_VALUE))
}

/// Whether m is an OCI image with a module manifest layer: an `oci@0.1`
/// module as `guestfn push --image` publishes it. A plain image is not one;
/// the OCI runtime runs it in the default sandbox.
pub fn is_module_image(m: &OciManifest) -> bool {
    m.is_image() && m.layers.iter().any(is_manifest_layer)
}

/// The refusal of an OCI module image by this package: the OCI runtime's
/// module, named so the reader knows which package serves it.
pub const MODULE_IMAGE_REFUSAL: &str = "is an OCI image with a module manifest (an oci@0.1 module), which function-wasm does not serve; the OCI runtime is function-oci (docs/one-pager-oci-runtime.md)";

/// Picks the layer of a manifest that holds the module: a wasm-typed layer
/// if there is one, otherwise the only layer that is not the module
/// manifest's. An OCI module image has none, and is refused by what it is
/// rather than by its layer count.
pub fn wasm_layer(m: &OciManifest) -> Result<Descriptor, String> {
    if is_module_image(m) {
        return Err(MODULE_IMAGE_REFUSAL.to_string());
    }
    let mut candidates = Vec::new();
    for l in &m.layers {
        if WASM_LAYER_TYPES.contains(&l.media_type.as_str()) {
            return Ok(l.clone());
        }
        if !is_manifest_layer(l) {
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

/// Picks the module-manifest layer of an artifact's or an image's
/// manifest, if any.
pub fn manifest_layer(m: &OciManifest) -> Option<Descriptor> {
    m.layers.iter().find(|l| is_manifest_layer(l)).cloned()
}

/// The module manifest's bytes out of its layer blob: a JSON layer as it
/// is, or MANIFEST_LAYER_PATH out of an image's annotated tar layer, the
/// entry bounded by limit.
pub fn manifest_from_layer(layer: &Descriptor, blob: &[u8], limit: u64) -> Result<Vec<u8>, String> {
    if !is_tar_layer(&layer.media_type) {
        return Ok(blob.to_vec());
    }
    match tar_entry(
        blob,
        MANIFEST_LAYER_PATH,
        limit,
        "manifest layer",
        "manifest",
    )? {
        Some(bytes) => Ok(bytes),
        None => Err(format!(
            "manifest layer is a tar archive without {MANIFEST_LAYER_PATH}"
        )),
    }
}

/// Whether a layer media type is a tar archive - a FROM scratch image
/// holding the module as a file - rather than a raw module.
pub fn is_tar_layer(media_type: &str) -> bool {
    media_type.contains("tar")
}

/// Returns SCRATCH_MODULE_PATH from a (possibly gzipped) tar layer; an
/// archive without that exact entry is refused, whatever else it holds.
pub fn extract_wasm(b: &[u8], limit: u64) -> Result<Vec<u8>, String> {
    match tar_entry(b, SCRATCH_MODULE_PATH, limit, "module layer", "module")? {
        Some(out) => Ok(out),
        None => Err(format!(
            "module layer is a tar archive without {SCRATCH_MODULE_PATH}: a FROM scratch image must COPY the module to {SCRATCH_MODULE_PATH}"
        )),
    }
}

/// The file at path out of a (possibly gzipped) tar layer, None when the
/// archive has no such entry. Builders name an entry with or without a
/// leading ./ or /, and all of them are the root's file. The archive may
/// expand to at most eight times limit before the entry is found, so a
/// gzip bomb costs bounded work, and the entry itself may not exceed
/// limit. `archive` and `entry` name the layer and the file in the errors.
fn tar_entry(
    b: &[u8],
    path: &str,
    limit: u64,
    archive: &str,
    entry_name: &str,
) -> Result<Option<Vec<u8>>, String> {
    let reader: Box<dyn Read> = if b.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(b))
    } else {
        Box::new(b)
    };
    let capped = CappedReader {
        r: reader,
        left: 8 * limit as i64,
        exceeded: format!("{archive} archive exceeds the size limit before {path}"),
    };
    let mut tar = tar::Archive::new(capped);
    let entries = tar
        .entries()
        .map_err(|e| format!("cannot read {archive} archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("cannot read {archive} archive: {e}"))?;
        let is_file = entry.header().entry_type().is_file();
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let trimmed = name
            .strip_prefix("./")
            .or_else(|| name.strip_prefix('/'))
            .unwrap_or(&name);
        let cleaned = format!("/{}", trimmed.trim_start_matches('/'));
        if is_file && cleaned == path {
            let mut out = Vec::new();
            let mut limited = (&mut entry).take(limit + 1);
            limited
                .read_to_end(&mut out)
                .map_err(|e| format!("cannot read {archive} archive: {e}"))?;
            if out.len() as u64 > limit {
                return Err(format!(
                    "{entry_name} exceeds the size limit of {limit} bytes"
                ));
            }
            return Ok(Some(out));
        }
    }
    Ok(None)
}

struct CappedReader<R> {
    r: R,
    left: i64,
    exceeded: String,
}

impl<R: Read> Read for CappedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left <= 0 {
            return Err(std::io::Error::other(self.exceeded.clone()));
        }
        let cap = (self.left as usize).min(buf.len());
        let n = self.r.read(&mut buf[..cap])?;
        self.left -= n as i64;
        Ok(n)
    }
}

/// The scheme a registry (host[:port]) is spoken to with: plain HTTP for a
/// loopback host - localhost or a loopback IP address, exactly - and HTTPS
/// for every other, a name that merely starts like one included.
fn scheme(registry: &str) -> &'static str {
    if crate::location::cleartext(registry) {
        "http"
    } else {
        "https"
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

/// The credential a module's registry is read with: the step's, else the
/// local Docker config's. The pull and the signature check both go through
/// it, so a module the runtime can pull is one whose signature it can read.
pub fn pull_auth(registry: &str, step: Option<Auth>) -> Option<Auth> {
    step.or_else(|| keychain_auth(registry))
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
    let entry = keys
        .iter()
        .find_map(|k| {
            if k == "https://index.docker.io/v1/" && registry != "index.docker.io" {
                return None;
            }
            config.auths.get(k)
        })
        .or_else(|| {
            // The reference's registry is canonical (location.rs); a config
            // written with another spelling of it - a default port, another
            // case - names the same registry. The first such key in order,
            // so the choice does not depend on the map's.
            let mut spelled: Vec<_> = config
                .auths
                .iter()
                .filter(|(k, _)| config_key_registry(k).as_deref() == Some(registry))
                .collect();
            spelled.sort_by_key(|(k, _)| k.as_str());
            spelled.first().map(|(_, entry)| *entry)
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

/// The canonical registry a Docker config key names: the key less its
/// scheme and path.
fn config_key_registry(key: &str) -> Option<String> {
    let authority = key
        .strip_prefix("https://")
        .or_else(|| key.strip_prefix("http://"))
        .unwrap_or(key);
    let host = authority.split('/').next().unwrap_or_default();
    crate::location::canonical_registry(host)
}

#[cfg(any(test, feature = "testutil"))]
pub mod testregistry {
    //! A minimal distribution registry for tests: manifests and blobs by
    //! digest, optionally behind the Bearer token flow, with OCI 1.1
    //! referrers through the referrers API or the referrers tag schema.

    use std::collections::HashMap;
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::sync::Arc;

    pub struct TestRegistry {
        pub manifests: HashMap<String, Vec<u8>>,
        pub blobs: HashMap<String, Vec<u8>>,
        pub bearer: bool,
        /// Whether the registry answers the referrers API (computed from
        /// the stored manifests' subjects); without it the API is a 404,
        /// as on GHCR and registry:3.
        pub referrers_api: bool,
        /// Whether every request needs Basic auth with BASIC_USER and
        /// BASIC_PASSWORD, as a registry:3 behind htpasswd does.
        pub basic: bool,
    }

    /// The credential a `basic` registry accepts.
    pub const BASIC_USER: &str = "e2e";
    pub const BASIC_PASSWORD: &str = "e2e-password";

    impl TestRegistry {
        /// Stores a referrer manifest under its digest as a pushing client
        /// does: on a registry without the referrers API it also adds the
        /// manifest to its subject's referrers tag index (sha256-<hex>).
        pub fn push_referrer(&mut self, manifest: Vec<u8>) {
            let digest = digest_of(&manifest);
            let (subject, descriptor) =
                referrer_descriptor(&digest, &manifest).expect("a manifest with a subject");
            if !self.referrers_api {
                let tag = super::referrers_tag(&subject);
                let mut index: serde_json::Value = self
                    .manifests
                    .get(&tag)
                    .map(|raw| serde_json::from_slice(raw).expect("index json"))
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "schemaVersion": 2,
                            "mediaType": super::OCI_INDEX_TYPE,
                            "manifests": [],
                        })
                    });
                index["manifests"]
                    .as_array_mut()
                    .expect("manifests")
                    .push(descriptor);
                self.manifests
                    .insert(tag, serde_json::to_vec(&index).expect("index json"));
            }
            self.manifests.insert(digest, manifest);
        }
    }

    /// A referrer's subject digest and its descriptor in a referrers
    /// listing: artifactType is the manifest's, or else its config's media
    /// type, and the manifest's annotations are carried over - as the OCI
    /// distribution spec has registries list them.
    fn referrer_descriptor(digest: &str, manifest: &[u8]) -> Option<(String, serde_json::Value)> {
        let m: serde_json::Value = serde_json::from_slice(manifest).ok()?;
        let subject = m["subject"]["digest"].as_str()?.to_string();
        let artifact_type = m["artifactType"]
            .as_str()
            .or_else(|| m["config"]["mediaType"].as_str())
            .unwrap_or_default();
        let mut descriptor = serde_json::json!({
            "mediaType": super::OCI_MANIFEST_TYPE,
            "digest": digest,
            "size": manifest.len(),
            "artifactType": artifact_type,
        });
        if let Some(annotations) = m.get("annotations") {
            descriptor["annotations"] = annotations.clone();
        }
        Some((subject, descriptor))
    }

    /// The referrers API's answer for a digest: every stored manifest whose
    /// subject is that digest, in digest order.
    fn referrers_index(manifests: &HashMap<String, Vec<u8>>, digest: &str) -> Vec<u8> {
        let mut found: Vec<serde_json::Value> = manifests
            .iter()
            .filter(|(key, raw)| **key == digest_of(raw))
            .filter_map(|(key, raw)| referrer_descriptor(key, raw))
            .filter(|(subject, _)| subject == digest)
            .map(|(_, descriptor)| descriptor)
            .collect();
        found.sort_by(|a, b| a["digest"].as_str().cmp(&b["digest"].as_str()));
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": super::OCI_INDEX_TYPE,
            "manifests": found,
        }))
        .expect("index json")
    }

    /// Reads one HTTP request: the head up to the blank line, then
    /// Content-Length bytes of body.
    fn read_request(conn: &mut std::net::TcpStream) -> (String, Vec<u8>) {
        use std::io::Read as _;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let split = loop {
            match conn.read(&mut chunk) {
                Ok(0) => break buf.len(),
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                }
                Err(_) => break buf.len(),
            }
        };
        let head = String::from_utf8_lossy(&buf[..split.min(buf.len())]).into_owned();
        let length = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .map(str::to_string)
            })
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = buf[split.min(buf.len())..].to_vec();
        while body.len() < length {
            match conn.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
            }
        }
        (head, body)
    }

    pub fn serve(registry: TestRegistry) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let bearer = registry.bearer;
        let referrers_api = registry.referrers_api;
        let basic = registry.basic.then(|| {
            use base64::Engine as _;
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD
                    .encode(format!("{BASIC_USER}:{BASIC_PASSWORD}"))
            )
        });
        let manifests = Arc::new(std::sync::Mutex::new(registry.manifests));
        let blobs = Arc::new(std::sync::Mutex::new(registry.blobs));
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut conn = conn;
                let (head, body) = read_request(&mut conn);
                let mut words = head.split_whitespace();
                let method = words.next().unwrap_or_default().to_string();
                let path = words.next().unwrap_or_default().to_string();
                let authorized =
                    !bearer || head.contains("Bearer testtoken") || path.starts_with("/token");
                let basic_authorized = basic.as_ref().is_none_or(|b| head.contains(b.as_str()));
                let mut extra_header = String::new();
                let (status, rsp_body): (&str, Vec<u8>) = if !basic_authorized {
                    (
                        "401 Unauthorized\r\nWww-Authenticate: Basic realm=\"registry\"",
                        b"{}".to_vec(),
                    )
                } else if !authorized {
                    (
                        "401 Unauthorized\r\nWww-Authenticate: Bearer realm=\"http://REALM/token\",service=\"registry\"",
                        b"{}".to_vec(),
                    )
                } else if path.starts_with("/token") {
                    ("200 OK", br#"{"token":"testtoken"}"#.to_vec())
                } else if method == "POST" && path.ends_with("/blobs/uploads/") {
                    extra_header = "Location: /upload\r\n".to_string();
                    ("202 Accepted", Vec::new())
                } else if method == "PUT" && path.starts_with("/upload") {
                    let digest = path.split("digest=").nth(1).unwrap_or_default().to_string();
                    blobs.lock().expect("poisoned").insert(digest, body);
                    ("201 Created", Vec::new())
                } else if method == "PUT" && path.contains("/manifests/") {
                    let key = path.split("/manifests/").nth(1).unwrap_or_default();
                    let mut m = manifests.lock().expect("poisoned");
                    // Addressable by the pushed reference and by content digest.
                    m.insert(key.to_string(), body.clone());
                    m.insert(digest_of(&body), body);
                    ("201 Created", Vec::new())
                } else if let Some(digest) = path.split("/referrers/").nth(1) {
                    if referrers_api {
                        let digest = digest.split('?').next().unwrap_or_default();
                        let index = referrers_index(&manifests.lock().expect("poisoned"), digest);
                        ("200 OK", index)
                    } else {
                        ("404 Not Found", Vec::new())
                    }
                } else if path.contains("/tags/list") {
                    let tags: Vec<String> = manifests
                        .lock()
                        .expect("poisoned")
                        .keys()
                        .filter(|name| !name.contains(':'))
                        .cloned()
                        .collect();
                    (
                        "200 OK",
                        serde_json::json!({ "tags": tags }).to_string().into_bytes(),
                    )
                } else if let Some((repository, reference)) = path
                    .strip_prefix("/v2/")
                    .and_then(|p| p.split_once("/manifests/"))
                {
                    let manifests = manifests.lock().expect("poisoned");
                    let scoped = format!("{repository}:{reference}");
                    match manifests.get(&scoped).or_else(|| manifests.get(reference)) {
                        Some(m) => ("200 OK", m.clone()),
                        None => ("404 Not Found", Vec::new()),
                    }
                } else if let Some(digest) = path.split("/blobs/").nth(1) {
                    match blobs.lock().expect("poisoned").get(digest) {
                        Some(b) => ("200 OK", b.clone()),
                        None => ("404 Not Found", Vec::new()),
                    }
                } else {
                    ("200 OK", Vec::new())
                };
                let status = status.replace("REALM", &addr.to_string());
                let header = format!(
                    "HTTP/1.1 {status}\r\n{extra_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    rsp_body.len()
                );
                let _ = conn.write_all(header.as_bytes());
                let _ = conn.write_all(&rsp_body);
            }
        });
        addr.to_string()
    }

    pub fn digest_of(b: &[u8]) -> String {
        use sha2::Digest as _;
        format!("sha256:{}", hex::encode(sha2::Sha256::digest(b)))
    }

    /// A single-wasm-layer artifact, served; returns (manifest digest,
    /// registry address).
    pub fn wasm_artifact(
        wasm: &[u8],
        module_manifest: Option<&[u8]>,
        bearer: bool,
    ) -> (String, String) {
        let mut registry = crate::testing::registry::Registry::new();
        if bearer {
            registry = registry.bearer();
        }
        let digest = registry.module(wasm, module_manifest, "");
        (digest, registry.serve())
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
    fn a_response_over_its_limit_is_refused_unread() {
        // An endless body: the read stops one byte past the limit.
        let got = read_bounded(std::io::repeat(b'x'), 16, "manifest");
        assert_eq!(
            got,
            Err("manifest exceeds the size limit of 16 bytes".to_string())
        );
        assert_eq!(
            read_bounded(&b"0123456789abcdef"[..], 16, "manifest"),
            Ok(b"0123456789abcdef".to_vec())
        );
    }

    #[test]
    fn a_manifest_over_the_limit_is_refused() {
        let mut registry = crate::testing::registry::Registry::new();
        let big = format!(
            r#"{{"schemaVersion":2,"annotations":{{"pad":"{}"}}}}"#,
            "x".repeat(MAX_MANIFEST_SIZE)
        );
        let digest = registry.manifest(big.as_bytes());
        let addr = registry.serve();
        let r = reference(&addr, &digest);
        let client = RegistryClient::new(&r, None);
        assert_eq!(
            client.manifest(&r).err(),
            Some(format!(
                "cannot fetch manifest {digest}: manifest exceeds the size limit of 4194304 bytes"
            ))
        );
        assert_eq!(
            client.raw_manifest(&digest).err(),
            Some(format!(
                "cannot fetch manifest {digest}: manifest exceeds the size limit of 4194304 bytes"
            ))
        );
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
        let (digest, addr) =
            testregistry::wasm_artifact(b"fake wasm", Some(br#"{"contract":"wasm@1.0"}"#), true);
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
            br#"{"contract":"wasm@1.0"}"#
        );
    }

    /// An OCI image with the annotated manifest layer is an oci@0.1
    /// module: this package names the package that serves it, and the
    /// manifest comes out of the layer's tar, gzipped or not. A plain image
    /// is neither.
    #[test]
    fn a_module_image_is_refused_by_name_and_its_manifest_read() {
        let manifest_json = br#"{"contract":"oci@0.1","name":"tagger"}"#;
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o444);
        header.set_size(manifest_json.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, ".guestfn/manifest.json", &manifest_json[..])
            .expect("tar");
        let plain = tar.into_inner().expect("tar");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &plain).expect("gzip");
        let gzipped = gz.finish().expect("gzip");
        let image = |layer: &[u8], media_type: &str, annotated: bool| -> OciManifest {
            let mut desc = serde_json::json!({"mediaType": media_type, "digest": testregistry::digest_of(layer), "size": layer.len()});
            if annotated {
                desc["annotations"] =
                    serde_json::json!({MANIFEST_LAYER_ANNOTATION: MANIFEST_LAYER_ANNOTATION_VALUE});
            }
            serde_json::from_value(serde_json::json!({
                "schemaVersion": 2,
                "mediaType": OCI_MANIFEST_TYPE,
                "config": {"mediaType": IMAGE_CONFIG_TYPES[0], "digest": testregistry::digest_of(b"{}"), "size": 2},
                "layers": [
                    {"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip", "digest": testregistry::digest_of(b"rootfs"), "size": 6},
                    desc,
                ],
            }))
            .expect("manifest")
        };
        let module = image(&plain, "application/vnd.oci.image.layer.v1.tar", true);
        assert!(is_module_image(&module));
        assert_eq!(wasm_layer(&module).unwrap_err(), MODULE_IMAGE_REFUSAL);
        let layer = manifest_layer(&module).expect("the annotated layer");
        assert_eq!(
            manifest_from_layer(&layer, &plain, 1 << 16).expect("manifest"),
            manifest_json
        );
        let module_gz = image(
            &gzipped,
            "application/vnd.oci.image.layer.v1.tar+gzip",
            true,
        );
        let layer = manifest_layer(&module_gz).expect("the annotated layer");
        assert_eq!(
            manifest_from_layer(&layer, &gzipped, 1 << 16).expect("manifest"),
            manifest_json
        );
        let without_entry = manifest_from_layer(&layer, b"", 1 << 16).unwrap_err();
        assert!(
            without_entry.contains("without /.guestfn/manifest.json"),
            "{without_entry}"
        );
        // The same layers with no annotation: a plain image, with two layers
        // and no wasm one.
        let plain_image = image(&plain, "application/vnd.oci.image.layer.v1.tar", false);
        assert!(!is_module_image(&plain_image));
        assert!(manifest_layer(&plain_image).is_none());
        assert!(
            wasm_layer(&plain_image)
                .unwrap_err()
                .contains("has 2 layers")
        );
        // A WebAssembly artifact beside its manifest layer is not an image.
        let (digest, addr) = testregistry::wasm_artifact(b"fake wasm", Some(b"{}"), false);
        let r = reference(&addr, &digest);
        let m = RegistryClient::new(&r, None)
            .manifest(&r)
            .expect("manifest");
        assert!(!is_module_image(&m));
        assert!(manifest_layer(&m).is_some());
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
    fn lists_referrers_through_the_api_or_the_tag_schema() {
        let subject = format!("sha256:{}", "a".repeat(64));
        let referrer = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_TYPE,
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": testregistry::digest_of(b"{}"), "size": 2},
            "layers": [],
            "subject": {"mediaType": OCI_MANIFEST_TYPE, "digest": subject, "size": 2},
        }))
        .expect("referrer");
        let referrer_digest = testregistry::digest_of(&referrer);
        for referrers_api in [true, false] {
            let mut registry = crate::testing::registry::Registry::new().bearer();
            if referrers_api {
                registry = registry.referrers_api();
            }
            registry.push_referrer(referrer.clone());
            // Only the listing the registry offers exists: the API answers
            // 404 without it, and a client pushing to a registry with it
            // writes no tag.
            let registry = registry.into_inner();
            assert_eq!(
                registry.manifests.contains_key(&referrers_tag(&subject)),
                !referrers_api
            );
            let addr = testregistry::serve(registry);
            let r = reference(&addr, &subject);
            let client = RegistryClient::new(&r, None);

            let listed = client.referrers(&subject, 1 << 20).expect("referrers");
            let digests: Vec<&str> = listed.iter().map(|d| d.digest.as_str()).collect();
            assert_eq!(
                digests,
                vec![referrer_digest.as_str()],
                "api {referrers_api}"
            );
            assert_eq!(listed[0].media_type, OCI_MANIFEST_TYPE);
            let m = client
                .manifest_by_digest(&listed[0].digest, 1 << 20)
                .expect("referrer manifest");
            assert_eq!(m.subject.map(|s| s.digest), Some(subject.clone()));

            let unreferenced = format!("sha256:{}", "b".repeat(64));
            assert!(
                client
                    .referrers(&unreferenced, 1 << 20)
                    .expect("referrers")
                    .is_empty()
            );
            let err = client.referrers(&subject, 8).expect_err("bounded");
            assert!(
                err.ends_with("referrers index exceeds the size limit of 8 bytes"),
                "{err}"
            );
        }
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

    #[test]
    fn a_docker_config_key_matches_in_any_spelling_of_the_registry() {
        for key in [
            "ghcr.io",
            "GHCR.IO",
            "ghcr.io:443",
            "https://ghcr.io:443",
            "https://ghcr.io./v2/",
        ] {
            let mut data = HashMap::new();
            data.insert(
                ".dockerconfigjson".to_string(),
                format!(r#"{{"auths":{{"{key}":{{"auth":"dTpw"}}}}}}"#).into_bytes(),
            );
            let auth = auth_for("ghcr.io", &data).expect(key);
            assert_eq!(
                (auth.username.as_str(), auth.password.as_str()),
                ("u", "p"),
                "{key}"
            );
            assert!(auth_for("ghcr.io:8443", &data).is_err(), "{key}");
        }
    }

    #[test]
    fn only_a_loopback_host_is_spoken_to_in_cleartext() {
        let cases = [
            ("localhost", "http"),
            ("localhost:5000", "http"),
            ("127.0.0.1:5000", "http"),
            ("127.8.9.10", "http"),
            ("[::1]:5000", "http"),
            ("[::1]", "http"),
            ("localhost.example.org", "https"),
            ("localhost.example.org:5000", "https"),
            ("127.0.0.1.nip.io", "https"),
            ("127.0.0.1.nip.io:5000", "https"),
            ("[::2]:5000", "https"),
            ("10.0.0.1:5000", "https"),
            ("registry.e2e.svc:5000", "https"),
            ("ghcr.io", "https"),
        ];
        for (registry, want) in cases {
            assert_eq!(scheme(registry), want, "{registry}");
        }
        let digest = format!("sha256:{}", "0".repeat(64));
        let base = |r: &str| {
            let reference = parse_oci_reference(&format!("{r}/team/fn@{digest}")).expect(r);
            RegistryClient::new(&reference, None).base
        };
        assert_eq!(base("LOCALHOST.:5000"), "http://localhost:5000");
        assert_eq!(
            base("localhost.example.org"),
            "https://localhost.example.org"
        );
        assert_eq!(base("docker.io"), "https://registry-1.docker.io");
        assert_eq!(
            base("registry-1.docker.io:443"),
            "https://registry-1.docker.io"
        );
    }
}
