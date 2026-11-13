//! Module sources resolved to content digests: a Path source is a file
//! under --module-dir, an HTTP source a URL whose download is verified
//! against the Input's stated digest (digests are stated, never
//! discovered), with fetched blobs kept in the content-addressed store.
//! Refusal strings are pinned by the conformance goldens.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::input::ModuleSource;
use crate::location::OciReference;
use crate::oci::{Auth, RegistryClient};
use crate::store::Store;

/// Remembers the digest of a served file by size and modification time, so
/// an unchanged module is not re-hashed on every request.
#[derive(Clone)]
struct FileStamp {
    size: u64,
    mtime: SystemTime,
    digest: String,
}

/// A resolved module: its content digest, the description refusals and logs
/// name it by, where to fetch it, and where its manifest lives (if it names
/// one).
#[derive(Clone, Debug)]
pub struct Resolved {
    pub digest: String,
    pub description: String,
    source: Source,
    manifest: ManifestSource,
}

#[derive(Clone, Debug)]
enum Source {
    Path {
        full: PathBuf,
    },
    Http {
        url: String,
    },
    Oci {
        reference: OciReference,
        auth: Option<Auth>,
    },
}

#[derive(Clone, Debug)]
enum ManifestSource {
    None,
    /// A guestfn.yaml under --module-dir, read fresh each request.
    PathFile {
        full: PathBuf,
    },
    /// A guestfn.yaml served beside the module, pinned by its own digest.
    Http {
        url: String,
        digest: String,
    },
    /// The artifact's own manifest layer, covered by the pinned digest.
    OciLayer {
        reference: OciReference,
        auth: Option<Auth>,
    },
}

pub struct Resolver {
    dir: Option<PathBuf>,
    max_size: u64,
    /// The content-addressed store of fetched blobs; None (tests, validate)
    /// fetches fresh.
    blobs: Option<Arc<Store>>,
    files: Mutex<HashMap<PathBuf, FileStamp>>,
    http: std::sync::OnceLock<reqwest::blocking::Client>,
    redirects: crate::egress::Guarded,
    /// The on-disk tier of the manifest cache: raw manifest bytes keyed by
    /// their own digest (the manifest layer's digest for an OCI source, the
    /// stated manifestDigest for an HTTP one) and verified on read, never
    /// trusted by name - a planted file under a pinning digest would
    /// otherwise be a manifest the runtime believed (the 1.0 audit's F-5).
    /// None (tests, validate) keeps the memory tier alone.
    manifests: Option<Arc<Store>>,
    /// Raw manifest bytes by the digest that pins them (the artifact's
    /// manifest digest for an OCI source, the stated manifestDigest for an
    /// HTTP one) and by their own digest: the content behind a digest never
    /// changes, so a manifest read once is the manifest, and a warm request
    /// costs no registry round trip. A source with no manifest is remembered
    /// as empty bytes, for the same reason. A guestfn.yaml under --module-dir
    /// is never cached: it is read fresh each request, by design.
    cached_manifests: Mutex<HashMap<String, Vec<u8>>>,
    /// The module type the runtime serves (the engine's kind): an OCI
    /// source's fetch yields the WebAssembly layer on function-wasm and an
    /// image layout, assembled from the image the artifact's manifest pins,
    /// on function-oci.
    serves: &'static str,
}

/// Most manifests the memory tier holds before it starts over; each is at
/// most manifest::MAX_SIZE, so the tier is bounded to a few megabytes.
const MAX_CACHED_MANIFESTS: usize = 4096;

/// The bounds of one HTTP download: its duration, and how many redirects
/// it follows (reqwest's own default, which applied before the resolver
/// followed them itself).
const FETCH_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_REDIRECTS: usize = 10;

impl Resolver {
    pub fn new(dir: Option<PathBuf>, max_size: u64, blobs: Option<Arc<Store>>) -> Self {
        Resolver {
            dir,
            max_size,
            blobs,
            files: Mutex::new(HashMap::new()),
            http: std::sync::OnceLock::new(),
            redirects: crate::egress::Guarded::default(),
            manifests: None,
            cached_manifests: Mutex::new(HashMap::new()),
            serves: crate::manifest::TYPE_WASM,
        }
    }

    /// Serves the engine's module type (`Engine::KIND`); wasm by default.
    pub fn serving(mut self, kind: &'static str) -> Self {
        self.serves = kind;
        self
    }

    /// Keeps fetched manifests in store too, so a restart on a warm disk
    /// cache reads no registry for them either.
    pub fn with_manifest_store(mut self, store: Arc<Store>) -> Self {
        self.manifests = Some(store);
        self
    }

    /// The module's manifest when answering costs no I/O: a source that
    /// names none, or one whose manifest the memory tier holds. None means
    /// manifest must be called, off the async executor.
    pub fn cached_manifest(&self, resolved: &Resolved) -> Option<Vec<u8>> {
        match &resolved.manifest {
            ManifestSource::None => Some(Vec::new()),
            ManifestSource::PathFile { .. } => None,
            ManifestSource::Http { digest, .. } => self
                .remembered(digest)
                .and_then(|raw| manifest_json(&raw).ok()),
            ManifestSource::OciLayer { reference, .. } => self.remembered(&reference.digest),
        }
    }

    fn remembered(&self, key: &str) -> Option<Vec<u8>> {
        self.cached_manifests
            .lock()
            .expect("poisoned")
            .get(key)
            .cloned()
    }

    fn remember(&self, key: &str, manifest: &[u8]) {
        let mut cached = self.cached_manifests.lock().expect("poisoned");
        if cached.len() >= MAX_CACHED_MANIFESTS {
            cached.clear();
        }
        cached.insert(key.to_string(), manifest.to_vec());
    }

    /// The raw manifest whose own digest is `digest`: the memory tier, the
    /// disk tier (verified against the digest on read), then fetch, whose
    /// bytes are kept in both. A failed fetch is kept nowhere, so the next
    /// request tries again.
    fn manifest_by_digest(
        &self,
        digest: &str,
        fetch: impl FnOnce() -> Result<Vec<u8>, String>,
    ) -> Result<Vec<u8>, String> {
        if let Some(m) = self.remembered(digest) {
            return Ok(m);
        }
        if let Some(store) = &self.manifests
            && let Some(m) = store.get(digest)
        {
            self.remember(digest, &m);
            return Ok(m);
        }
        let m = fetch()?;
        if let Some(store) = &self.manifests {
            // Best effort, as for blobs: a full store costs the next
            // process one fetch.
            let _ = store.put(digest, &m);
        }
        self.remember(digest, &m);
        Ok(m)
    }

    /// Resolves a concrete source - one whose from was materialised.
    /// Resolving does no I/O: the digest comes from the Input (for OCI, the
    /// manifest digest that pins the layer), or for a served file from the
    /// file; fetching is deferred. auth authenticates OCI pulls; None falls
    /// back to the local Docker config and anonymous access.
    pub fn resolve(&self, src: &ModuleSource, auth: Option<Auth>) -> Result<Resolved, String> {
        match src.r#type.as_str() {
            "OCI" => {
                let oci = src
                    .oci
                    .as_ref()
                    .expect("validated: an OCI source has its object");
                let reference = crate::location::parse_oci_reference(&oci.r#ref)?;
                let auth = crate::oci::pull_auth(&reference.registry, auth);
                Ok(Resolved {
                    digest: reference.digest.clone(),
                    description: format!("oci {}", crate::location::canonical_oci_ref(&oci.r#ref)?),
                    source: Source::Oci {
                        reference: reference.clone(),
                        auth: auth.clone(),
                    },
                    manifest: ManifestSource::OciLayer { reference, auth },
                })
            }
            "HTTP" => {
                let http = src
                    .http
                    .as_ref()
                    .expect("validated: an HTTP source has its object");
                let manifest = if http.manifest_url.is_empty() {
                    ManifestSource::None
                } else {
                    ManifestSource::Http {
                        url: http.manifest_url.clone(),
                        digest: http.manifest_digest.clone(),
                    }
                };
                Ok(Resolved {
                    digest: http.digest.clone(),
                    description: format!("http {}", http.url),
                    source: Source::Http {
                        url: http.url.clone(),
                    },
                    manifest,
                })
            }
            _ => self.resolve_path(&src.path, &src.manifest_path),
        }
    }

    fn resolve_path(&self, rel: &str, manifest_rel: &str) -> Result<Resolved, String> {
        let full = self.confined_path("module.path", rel)?;
        let digest = self.file_digest(&full)?;
        let manifest = if manifest_rel.is_empty() {
            ManifestSource::None
        } else {
            ManifestSource::PathFile {
                full: self.confined_path("module.manifestPath", manifest_rel)?,
            }
        };
        Ok(Resolved {
            digest,
            description: format!("module file {rel}"),
            source: Source::Path { full },
            manifest,
        })
    }

    /// Returns the module bytes, verified along the chain the digest pins:
    /// a served file is re-verified against the digest resolve stamped, a
    /// download against the Input's stated digest (through the blob store
    /// when one is configured).
    pub fn fetch(&self, resolved: &Resolved) -> Result<Vec<u8>, String> {
        let source = match &resolved.source {
            Source::Path { .. } => "path",
            Source::Http { .. } => "http",
            Source::Oci { .. } => "oci",
        };
        let start = std::time::Instant::now();
        let result = self.fetch_inner(resolved);
        guestfn_engine::metrics::FETCH_DURATION
            .with_label_values(&[source])
            .observe(start.elapsed().as_secs_f64());
        result
    }

    fn fetch_inner(&self, resolved: &Resolved) -> Result<Vec<u8>, String> {
        match &resolved.source {
            Source::Path { full } => self.fetch_path(full, &resolved.digest),
            Source::Http { url } => self.verified("module", &resolved.digest, || {
                self.http_get(url, "module", self.max_size)
            }),
            Source::Oci { reference, auth } => {
                if self.serves == crate::manifest::TYPE_OCI {
                    return self.fetch_image(reference, auth.clone());
                }
                // The manifest is verified against the pinned digest, the
                // layer against its own through the blob store, so a module
                // whose compiled artifact is gone costs one manifest read
                // and no download.
                let client = RegistryClient::new(reference, auth.clone());
                let m = client.manifest(reference)?;
                let layer = crate::oci::wasm_layer(&m)
                    .map_err(|e| format!("{}/{} {e}", reference.registry, reference.repository))?;
                let b = self.verified("module layer", &layer.digest, || {
                    client.blob(&layer.digest, self.max_size, "module layer")
                })?;
                if crate::oci::is_tar_layer(&layer.media_type) {
                    return crate::oci::extract_wasm(&b, self.max_size);
                }
                Ok(b)
            }
        }
    }

    /// The OCI runtime's fetch: the pinned reference is an image or an
    /// index, whose manifest for this platform, config and layers come
    /// down through the blob store, verified against their digests and
    /// bounded together by --module-size-limit, and go to the engine as one
    /// image layout. An image with the annotated manifest layer `guestfn
    /// push --image` appends is an oci@0.1 module: its manifest is read
    /// out of that one layer first and remembered under the pinning digest
    /// for the read that follows; a plain image runs as a manifest-less
    /// module. A WebAssembly artifact is refused by name.
    fn fetch_image(&self, reference: &OciReference, auth: Option<Auth>) -> Result<Vec<u8>, String> {
        use guestfn_engine::layout;
        let image_client = RegistryClient::new(reference, auth);
        let top =
            image_client.manifest_by_digest(&reference.digest, crate::oci::MAX_MANIFEST_SIZE)?;
        if !(top.is_image() || top.is_index()) {
            return Err(
                "is a WebAssembly artifact, which function-oci does not serve; function-wasm serves it"
                    .to_string(),
            );
        }
        let (manifest, raw, digest) = self.platform_manifest(&image_client, &reference.digest)?;
        if !manifest.is_image() {
            return Err(format!(
                "image manifest {digest} has no image config (its config is {})",
                manifest
                    .config
                    .as_ref()
                    .map(|c| c.media_type.as_str())
                    .unwrap_or("absent")
            ));
        }
        match crate::oci::manifest_layer(&manifest) {
            Some(layer) => {
                let raw_manifest = self.manifest_from_layer(&image_client, &layer)?;
                let mf = crate::manifest::Manifest::parse(&raw_manifest)
                    .map_err(|e| format!("has an invalid manifest: {e}"))?;
                if mf.kind() != crate::manifest::TYPE_OCI {
                    return Err(
                        "is a WebAssembly module (wasm@1.0) by its manifest, which function-oci does not serve; function-wasm serves it"
                            .to_string(),
                    );
                }
                self.remember(&reference.digest, &raw_manifest);
            }
            None => self.remember(&reference.digest, &[]),
        }
        let config_desc = manifest.config.clone().expect("checked above");
        let mut total = raw.len() as u64;
        let mut w = layout::Writer::new();
        let config = self.verified("image config", &config_desc.digest, || {
            image_client.blob(&config_desc.digest, self.max_size, "image config")
        })?;
        total += config.len() as u64;
        w.blob(&config);
        for (i, l) in manifest.layers.iter().enumerate() {
            let bytes = self.verified("image layer", &l.digest, || {
                image_client.blob(&l.digest, self.max_size, &format!("image layer {i}"))
            })?;
            total += bytes.len() as u64;
            if total > self.max_size {
                return Err(format!(
                    "image exceeds the size limit of {} bytes (--module-size-limit)",
                    self.max_size
                ));
            }
            w.blob(&bytes);
        }
        let media_type = if manifest.media_type.is_empty() {
            crate::oci::OCI_MANIFEST_TYPE
        } else {
            manifest.media_type.as_str()
        };
        w.manifest(&raw, media_type);
        w.finish().map_err(|e| e.to_string())
    }

    /// The image manifest for this host out of the manifest at digest:
    /// the manifest itself, or the linux entry of an index for this
    /// architecture, with its raw bytes and its own digest.
    fn platform_manifest(
        &self,
        client: &RegistryClient,
        digest: &str,
    ) -> Result<(crate::oci::OciManifest, Vec<u8>, String), String> {
        let (raw, got) = client.raw_manifest(digest)?;
        if got != digest {
            return Err(format!("image manifest content is {got}, want {digest}"));
        }
        let manifest: crate::oci::OciManifest = serde_json::from_slice(&raw)
            .map_err(|e| format!("cannot parse image manifest {digest}: {e}"))?;
        if !manifest.is_index() {
            return Ok((manifest, raw, digest.to_string()));
        }
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => other,
        };
        let entry = manifest
            .index_entries()
            .into_iter()
            .find(|e| {
                e.platform
                    .as_ref()
                    .is_some_and(|p| p.architecture == arch && p.os == "linux")
            })
            .ok_or_else(|| format!("image index {digest} has no manifest for linux/{arch}"))?;
        let (entry_raw, entry_got) = client.raw_manifest(&entry.digest)?;
        if entry_got != entry.digest {
            return Err(format!(
                "image manifest content is {entry_got}, want {}",
                entry.digest
            ));
        }
        let entry_manifest = serde_json::from_slice(&entry_raw)
            .map_err(|e| format!("cannot parse image manifest {}: {e}", entry.digest))?;
        Ok((entry_manifest, entry_raw, entry.digest))
    }

    /// The module manifest out of its layer: the layer's blob through the
    /// manifest store under its own digest (verified on read), then the
    /// JSON as it is or the manifest file out of an image's annotated tar
    /// layer.
    fn manifest_from_layer(
        &self,
        client: &RegistryClient,
        layer: &crate::oci::Descriptor,
    ) -> Result<Vec<u8>, String> {
        let limit = crate::manifest::MAX_SIZE as u64;
        let blob = self.manifest_by_digest(&layer.digest, || {
            self.verified("manifest layer", &layer.digest, || {
                // A tar layer around the manifest carries the archive's
                // headers too, so the blob may be a little over the
                // manifest's own cap.
                client.blob(&layer.digest, 2 * limit, "manifest layer")
            })
        })?;
        crate::oci::manifest_from_layer(layer, &blob, limit)
    }

    fn fetch_path(&self, full: &Path, digest: &str) -> Result<Vec<u8>, String> {
        let f = std::fs::File::open(full)
            .map_err(|e| format!("cannot read module file: {}", go_io_error("open", full, &e)))?;
        let b = read_capped(f, self.max_size)?;
        let got = digest_of(&b);
        if got != digest {
            // The stamp lied (a same-size rewrite within the mtime
            // granularity): forget it so the next request re-hashes.
            self.files.lock().expect("poisoned").remove(full);
            return Err(format!(
                "module file changed while being read: content is {got}, expected {digest}"
            ));
        }
        Ok(b)
    }

    /// The module's manifest as JSON bytes, empty when the source names
    /// none: a guestfn.yaml under --module-dir read fresh each request, or
    /// one served over HTTP, verified against its own digest.
    pub fn manifest(&self, resolved: &Resolved) -> Result<Vec<u8>, String> {
        match &resolved.manifest {
            ManifestSource::None => Ok(Vec::new()),
            ManifestSource::PathFile { full } => {
                let f = std::fs::File::open(full).map_err(|e| {
                    format!(
                        "cannot read manifest file: {}",
                        go_io_error("open", full, &e)
                    )
                })?;
                let b = read_capped(f, crate::manifest::MAX_SIZE as u64)?;
                manifest_json(&b)
            }
            ManifestSource::Http { url, digest } => {
                // The stated manifestDigest is the raw bytes' own digest, so
                // it keys both tiers directly.
                let raw = self.manifest_by_digest(digest, || {
                    self.verified("manifest", digest, || {
                        self.http_get(url, "manifest", crate::manifest::MAX_SIZE as u64)
                    })
                })?;
                manifest_json(&raw)
            }
            ManifestSource::OciLayer { reference, auth } => {
                // The pinning digest is the artifact's, not the layer's, so
                // it keys the memory tier alone; the disk tier is keyed by
                // the layer's own digest, learned from the artifact's
                // manifest. A process that lost the memo pays one manifest
                // GET per digest, with the credential, and no blob GET when
                // the disk tier has the layer.
                if let Some(m) = self.remembered(&reference.digest) {
                    return Ok(m);
                }
                let client = RegistryClient::new(reference, auth.clone());
                // An image may be pinned by an index, which pins one
                // manifest per platform: the manifest layer is in this
                // host's, read as the fetch reads it, so a process that
                // lost the memo while it kept the compiled image answers
                // as one that just fetched. A WebAssembly artifact is one
                // manifest, and an index in its place is refused.
                let m = if self.serves == crate::manifest::TYPE_OCI {
                    self.platform_manifest(&client, &reference.digest)?.0
                } else {
                    client.manifest(reference)?
                };
                let Some(layer) = crate::oci::manifest_layer(&m) else {
                    self.remember(&reference.digest, &[]);
                    return Ok(Vec::new());
                };
                let bytes = self.manifest_from_layer(&client, &layer)?;
                self.remember(&reference.digest, &bytes);
                Ok(bytes)
            }
        }
    }

    /// The blob with the given content digest: from the blob store when one
    /// holds it, otherwise fetched, checked against the digest and saved.
    fn verified(
        &self,
        what: &str,
        digest: &str,
        fetch: impl FnOnce() -> Result<Vec<u8>, String>,
    ) -> Result<Vec<u8>, String> {
        use guestfn_engine::metrics::{self, CACHE_EVENTS};
        if let Some(blobs) = &self.blobs {
            if let Some(b) = blobs.get(digest) {
                CACHE_EVENTS
                    .with_label_values(&[metrics::CACHE_BLOB, metrics::EVENT_HIT])
                    .inc();
                return Ok(b);
            }
            CACHE_EVENTS
                .with_label_values(&[metrics::CACHE_BLOB, metrics::EVENT_MISS])
                .inc();
        }
        let b = fetch()?;
        let got = digest_of(&b);
        if got != digest {
            return Err(format!("{what} content is {got}, want {digest}"));
        }
        if let Some(blobs) = &self.blobs {
            // A full cache is not a reason to fail the request; the blob is
            // simply fetched again next time.
            let _ = blobs.put(digest, &b);
        }
        Ok(b)
    }

    /// Fetches what names the resource in the errors (module, manifest)
    /// from url, bounded to limit.
    fn http_get(&self, url: &str, what: &str, limit: u64) -> Result<Vec<u8>, String> {
        let client = self.http.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("the resolver client's configuration is static")
        });
        let failed = |e: &dyn std::fmt::Display| format!("cannot download {what}: {e}");
        let mut url: reqwest::Url = url.parse().map_err(|e| failed(&e))?;
        let origin = url.origin();
        // Redirects are followed here, one hop at a time: within the origin
        // the Input named as that origin is fetched, and to another host
        // only through the guarded client.
        for _ in 0..=MAX_REDIRECTS {
            let rsp = if url.origin() == origin {
                client.get(url.clone()).send().map_err(|e| failed(&e))?
            } else {
                self.redirects.get(&url, FETCH_TIMEOUT).map_err(|e| {
                    format!("cannot download {what}: the redirect to {url} is not followed: {e}")
                })?
            };
            let status = rsp.status();
            if status.is_redirection()
                && let Some(location) = rsp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
            {
                let next = url.join(location).map_err(|e| failed(&e))?;
                if !matches!(next.scheme(), "http" | "https") {
                    return Err(format!(
                        "cannot download {what}: the redirect to {next} is not followed: not an http or https URL"
                    ));
                }
                url = next;
                continue;
            }
            if status != reqwest::StatusCode::OK {
                return Err(format!("cannot download {what}: {status}"));
            }
            return read_capped(rsp, limit);
        }
        Err(format!(
            "cannot download {what}: more than {MAX_REDIRECTS} redirects"
        ))
    }

    /// Resolves rel under the module directory, refusing an absolute path or
    /// one that escapes the directory; field names it in the errors.
    fn confined_path(&self, field: &str, rel: &str) -> Result<PathBuf, String> {
        let Some(dir) = &self.dir else {
            return Err(format!(
                "{field} is refused: the function was started without --module-dir"
            ));
        };
        let rel_path = Path::new(rel);
        if rel_path.is_absolute() {
            return Err(format!(
                "{field} {rel:?} must be relative to the module directory"
            ));
        }
        // A lexical check: normal components only.
        let escapes = {
            let mut depth: i64 = 0;
            let mut escaped = false;
            for c in rel_path.components() {
                match c {
                    Component::Normal(_) => depth += 1,
                    Component::ParentDir => {
                        depth -= 1;
                        if depth < 0 {
                            escaped = true;
                        }
                    }
                    Component::CurDir => {}
                    Component::RootDir | Component::Prefix(_) => escaped = true,
                }
            }
            escaped
        };
        if escapes {
            return Err(format!("{field} {rel:?} escapes the module directory"));
        }
        Ok(dir.join(rel_path))
    }

    fn file_digest(&self, full: &Path) -> Result<String, String> {
        let st = std::fs::metadata(full)
            .map_err(|e| format!("cannot stat module file: {}", go_io_error("stat", full, &e)))?;
        if st.is_dir() {
            return Err(format!(
                "module file {:?} is a directory",
                full.display().to_string()
            ));
        }
        if st.len() > self.max_size {
            return Err(format!(
                "module file is {} bytes, the limit is {}",
                st.len(),
                self.max_size
            ));
        }
        let mtime = st
            .modified()
            .map_err(|e| format!("cannot stat module file: {e}"))?;
        if let Some(s) = self.files.lock().expect("poisoned").get(full)
            && s.size == st.len()
            && s.mtime == mtime
        {
            return Ok(s.digest.clone());
        }
        let mut f = std::fs::File::open(full)
            .map_err(|e| format!("cannot read module file: {}", go_io_error("open", full, &e)))?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = f
                .read(&mut buf)
                .map_err(|e| format!("cannot hash module file: {e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let digest = format!("sha256:{}", hex::encode(hasher.finalize()));
        self.files.lock().expect("poisoned").insert(
            full.to_owned(),
            FileStamp {
                size: st.len(),
                mtime,
                digest: digest.clone(),
            },
        );
        Ok(digest)
    }
}

/// Normalizes a guestfn.yaml manifest (YAML) to the JSON bytes
/// manifest::parse decodes - the format an OCI manifest layer already
/// carries. JSON is valid YAML, so an already-JSON manifest passes through.
fn manifest_json(raw: &[u8]) -> Result<Vec<u8>, String> {
    let value: serde_json::Value =
        serde_saphyr::from_slice(raw).map_err(|e| format!("manifest is not valid YAML: {e}"))?;
    serde_json::to_vec(&value).map_err(|e| format!("manifest is not valid YAML: {e}"))
}

/// Renders an I/O failure as "<op> <path>: <detail>" ("open <path>: no such
/// file or directory", "permission denied"): these strings reach refusal
/// messages the conformance goldens pin.
pub(crate) fn go_io_error(op: &str, path: &Path, e: &std::io::Error) -> String {
    let detail = match e.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => e.to_string(),
    };
    format!("{op} {}: {detail}", path.display())
}

fn read_capped(f: impl Read, limit: u64) -> Result<Vec<u8>, String> {
    let mut b = Vec::new();
    f.take(limit + 1)
        .read_to_end(&mut b)
        .map_err(|e| format!("cannot read module file: {e}"))?;
    if b.len() as u64 > limit {
        return Err(format!("module exceeds the size limit of {limit} bytes"));
    }
    Ok(b)
}

fn digest_of(b: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_source(rel: &str) -> ModuleSource {
        ModuleSource {
            r#type: "Path".to_string(),
            path: rel.to_string(),
            ..Default::default()
        }
    }

    /// Serves /module with body, and a redirect from every other path:
    /// /hop to /module, /away to the same server under another host name.
    fn redirecting_server(body: &'static str) -> std::net::SocketAddr {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut buf = [0u8; 1024];
                let n = conn.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let reply = match head.split_whitespace().nth(1).unwrap_or_default() {
                    "/module" => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    "/hop" => "HTTP/1.1 302 Found\r\nLocation: /module\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    "/loop" => "HTTP/1.1 302 Found\r\nLocation: /loop\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    "/meta" => "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    "/file" => "HTTP/1.1 302 Found\r\nLocation: file:///etc/passwd\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    _ => format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://localhost:{}/module\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        addr.port()
                    ),
                };
                let _ = conn.write_all(reply.as_bytes());
            }
        });
        addr
    }

    #[test]
    fn a_download_follows_a_redirect_within_its_origin_alone() {
        let addr = redirecting_server("module bytes");
        let r = Resolver::new(None, 1 << 20, None);
        let get = |path: &str| {
            r.http_get(&format!("http://{addr}{path}"), "module", 1 << 20)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
        };
        let port = addr.port();
        let cases = [
            ("/module", Ok("module bytes".to_string())),
            ("/hop", Ok("module bytes".to_string())),
            // The same server under another name is another host, and a
            // loopback one: the block list refuses it.
            (
                "/away",
                Err(format!(
                    "cannot download module: the redirect to http://localhost:{port}/module is not followed: localhost resolves to an address the default block list refuses"
                )),
            ),
            (
                "/meta",
                Err("cannot download module: the redirect to http://169.254.169.254/latest is not followed: 169.254.169.254 resolves to an address the default block list refuses".to_string()),
            ),
            (
                "/file",
                Err("cannot download module: the redirect to file:///etc/passwd is not followed: not an http or https URL".to_string()),
            ),
            (
                "/loop",
                Err("cannot download module: more than 10 redirects".to_string()),
            ),
        ];
        for (path, want) in cases {
            assert_eq!(get(path), want, "{path}");
        }
    }

    #[test]
    fn resolves_and_fetches_a_module_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fn.wasm"), b"not really wasm").expect("write");
        let r = Resolver::new(Some(dir.path().to_owned()), 1 << 20, None);
        let resolved = r.resolve(&path_source("fn.wasm"), None).expect("resolve");
        assert_eq!(resolved.description, "module file fn.wasm");
        assert!(resolved.digest.starts_with("sha256:"));
        assert_eq!(r.fetch(&resolved).expect("fetch"), b"not really wasm");
        // The second resolve hits the stamp.
        assert_eq!(
            r.resolve(&path_source("fn.wasm"), None)
                .expect("resolve")
                .digest,
            resolved.digest
        );
    }

    #[test]
    fn caches_an_oci_manifest_by_its_layer_digest() {
        // The 1.0 audit's F-5 rule for the manifests store: verify-on-read,
        // keyed by the manifest's own digest, never by the artifact's.
        let wasm = b"not really wasm";
        let module_manifest = br#"{"contract":"wasm@1.0"}"#;
        let (pinning, addr) =
            crate::oci::testregistry::wasm_artifact(wasm, Some(module_manifest), false);
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(Store::open_dir(dir.path(), true).expect("store"));
        let r = Resolver::new(None, 1 << 20, None).with_manifest_store(Arc::clone(&store));
        let src = ModuleSource {
            r#type: "OCI".to_string(),
            oci: Some(crate::input::OciSource {
                r#ref: format!("{addr}/example/greeter@{pinning}"),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = r.resolve(&src, None).expect("resolve");
        assert!(
            r.cached_manifest(&resolved).is_none(),
            "a cold manifest is I/O"
        );
        assert_eq!(r.manifest(&resolved).expect("manifest"), module_manifest);
        let layer = crate::oci::testregistry::digest_of(module_manifest);
        assert_eq!(store.get(&layer).as_deref(), Some(&module_manifest[..]));
        assert!(
            store.get(&pinning).is_none(),
            "nothing is stored under the pinning digest"
        );
        assert_eq!(
            r.cached_manifest(&resolved).as_deref(),
            Some(&module_manifest[..]),
            "a warm manifest costs no I/O"
        );
        // A fresh resolver over the same store learns the layer digest from
        // the artifact's manifest and takes the layer from disk.
        let again = Resolver::new(None, 1 << 20, None).with_manifest_store(store);
        assert_eq!(
            again.manifest(&resolved).expect("manifest"),
            module_manifest
        );
    }

    /// The OCI runtime's fetch of a module image: the manifest comes out
    /// of the annotated layer and is remembered under the pinning digest,
    /// the layout carries every layer, and a plain image has no manifest.
    /// A WebAssembly artifact is refused before any blob is pulled.
    #[test]
    fn a_module_image_yields_its_manifest_and_a_plain_image_none() {
        use crate::testing::registry::Registry;
        let module_manifest = br#"{"contract":"oci@0.1","name":"probe","version":"0.1.0"}"#;
        let mut registry = Registry::new();
        let module = registry.module_image(module_manifest, "module");
        let plain = registry.plain_image("plain");
        let wasm = registry.module(b"not wasm", Some(br#"{"contract":"wasm@1.0"}"#), "wasm");
        let addr = registry.serve();
        let r = Resolver::new(None, 1 << 20, None).serving(crate::manifest::TYPE_OCI);
        let source = |digest: &str| ModuleSource {
            r#type: "OCI".to_string(),
            oci: Some(crate::input::OciSource {
                r#ref: format!("{addr}/example/image@{digest}"),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resolved = r.resolve(&source(&module), None).expect("resolve");
        let layout = r.fetch(&resolved).expect("the module image as a layout");
        assert!(!layout.is_empty());
        assert_eq!(
            r.cached_manifest(&resolved).as_deref(),
            Some(&module_manifest[..]),
            "the fetch remembered the manifest layer's content"
        );
        assert_eq!(r.manifest(&resolved).expect("manifest"), module_manifest);
        // A resolver that lost the memo reads the layer again through the
        // image's manifest.
        let cold = Resolver::new(None, 1 << 20, None).serving(crate::manifest::TYPE_OCI);
        assert_eq!(cold.manifest(&resolved).expect("manifest"), module_manifest);

        let resolved = r.resolve(&source(&plain), None).expect("resolve");
        r.fetch(&resolved).expect("the plain image as a layout");
        assert_eq!(
            r.cached_manifest(&resolved).as_deref(),
            Some(&[][..]),
            "a plain image runs in the default sandbox"
        );

        let resolved = r.resolve(&source(&wasm), None).expect("resolve");
        assert_eq!(
            r.fetch(&resolved).unwrap_err(),
            "is a WebAssembly artifact, which function-oci does not serve; function-wasm serves it"
        );
    }

    /// A module pinned by an image index, what a multi-platform push
    /// publishes: the manifest is this platform's whether the fetch left
    /// it in memory or a resolver that never fetched reads it, which is
    /// where a process stands once the memory tier started over while the
    /// compiled image stayed on disk.
    #[test]
    fn an_image_index_yields_its_manifest_warm_and_cold() {
        use crate::testing::registry::Registry;
        let module_manifest = br#"{"contract":"oci@0.1","name":"probe","version":"0.1.0"}"#;
        let mut registry = Registry::new();
        let module = registry.module_index(module_manifest, "module");
        let plain = registry.plain_index("plain");
        let addr = registry.serve();
        let source = |digest: &str| ModuleSource {
            r#type: "OCI".to_string(),
            oci: Some(crate::input::OciSource {
                r#ref: format!("{addr}/example/image@{digest}"),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cases: [(&str, &[u8]); 2] = [(&module, module_manifest), (&plain, b"")];
        for (digest, want) in cases {
            let warm = Resolver::new(None, 1 << 20, None).serving(crate::manifest::TYPE_OCI);
            let resolved = warm.resolve(&source(digest), None).expect("resolve");
            warm.fetch(&resolved).expect("the image as a layout");
            assert_eq!(warm.cached_manifest(&resolved).as_deref(), Some(want));
            assert_eq!(warm.manifest(&resolved).as_deref(), Ok(want));

            let cold = Resolver::new(None, 1 << 20, None).serving(crate::manifest::TYPE_OCI);
            assert_eq!(cold.cached_manifest(&resolved), None);
            assert_eq!(cold.manifest(&resolved).as_deref(), Ok(want));
            assert_eq!(
                cold.cached_manifest(&resolved).as_deref(),
                Some(want),
                "the cold read is remembered as the fetch's is"
            );
        }
    }

    /// A WebAssembly artifact is one manifest: an index in its place is
    /// refused by the fetch and by the manifest read alike.
    #[test]
    fn an_image_index_is_no_webassembly_module() {
        use crate::testing::registry::Registry;
        let mut registry = Registry::new();
        let index = registry.plain_index("plain");
        let addr = registry.serve();
        let r = Resolver::new(None, 1 << 20, None);
        let resolved = r
            .resolve(
                &ModuleSource {
                    r#type: "OCI".to_string(),
                    oci: Some(crate::input::OciSource {
                        r#ref: format!("{addr}/example/image@{index}"),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                None,
            )
            .expect("resolve");
        let want = format!(
            "{addr}/example/image@{index} is an image index; reference the manifest holding the module"
        );
        assert_eq!(r.fetch(&resolved).unwrap_err(), want);
        assert_eq!(r.manifest(&resolved).unwrap_err(), want);
    }

    #[test]
    fn refusals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = Resolver::new(Some(dir.path().to_owned()), 1 << 20, None);
        let cases: &[(&str, &str)] = &[
            (
                "../escape.wasm",
                r#"module.path "../escape.wasm" escapes the module directory"#,
            ),
            (
                "/abs.wasm",
                r#"module.path "/abs.wasm" must be relative to the module directory"#,
            ),
        ];
        for (rel, want) in cases {
            assert_eq!(&r.resolve(&path_source(rel), None).expect_err(rel), want);
        }

        // Inside the directory: a directory, a file that is not there, and
        // a manifest named outside it beside a module that is fine.
        std::fs::create_dir(dir.path().join("modules")).expect("mkdir");
        std::fs::write(dir.path().join("fn.wasm"), b"not really wasm").expect("write");
        let inside = |rel: &str| dir.path().join(rel).display().to_string();
        assert_eq!(
            r.resolve(&path_source("modules"), None).unwrap_err(),
            format!("module file {:?} is a directory", inside("modules"))
        );
        assert_eq!(
            r.resolve(&path_source("missing.wasm"), None).unwrap_err(),
            format!(
                "cannot stat module file: stat {}: no such file or directory",
                inside("missing.wasm")
            )
        );
        let with_manifest = |manifest: &str| ModuleSource {
            manifest_path: manifest.to_string(),
            ..path_source("fn.wasm")
        };
        assert_eq!(
            r.resolve(&with_manifest("../guestfn.yaml"), None)
                .unwrap_err(),
            r#"module.manifestPath "../guestfn.yaml" escapes the module directory"#
        );
        assert_eq!(
            r.resolve(&with_manifest("/etc/guestfn.yaml"), None)
                .unwrap_err(),
            r#"module.manifestPath "/etc/guestfn.yaml" must be relative to the module directory"#
        );

        let none = Resolver::new(None, 1 << 20, None);
        assert_eq!(
            none.resolve(&path_source("fn.wasm"), None)
                .expect_err("no dir"),
            "module.path is refused: the function was started without --module-dir"
        );
    }

    /// The resolver's size limit (--module-size-limit) holds for every
    /// source: a module of exactly the limit is read, one a byte over it
    /// is refused.
    #[test]
    fn the_size_limit_bounds_a_module_from_every_source() {
        use crate::testing::registry::Registry;
        const LIMIT: u64 = 16;
        const AT: &str = "sixteen bytes ok";
        const OVER: &str = "seventeen bytes!!";
        assert_eq!((AT.len() as u64, OVER.len() as u64), (LIMIT, LIMIT + 1));

        // A file is refused by its size before it is hashed.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("at.wasm"), AT).expect("write");
        std::fs::write(dir.path().join("over.wasm"), OVER).expect("write");
        let r = Resolver::new(Some(dir.path().to_owned()), LIMIT, None);
        let resolved = r.resolve(&path_source("at.wasm"), None).expect("resolve");
        assert_eq!(r.fetch(&resolved), Ok(AT.as_bytes().to_vec()));
        assert_eq!(
            r.resolve(&path_source("over.wasm"), None).unwrap_err(),
            "module file is 17 bytes, the limit is 16"
        );
        // One that grew after it was resolved is cut at the read.
        std::fs::write(dir.path().join("at.wasm"), OVER).expect("write");
        assert_eq!(
            r.fetch(&resolved).unwrap_err(),
            "module exceeds the size limit of 16 bytes"
        );

        let fetch = |source: ModuleSource| {
            let resolved = r.resolve(&source, None).expect("resolve");
            r.fetch(&resolved)
        };
        let http = |body: &'static str| {
            let addr = redirecting_server(body);
            fetch(ModuleSource {
                r#type: "HTTP".to_string(),
                http: Some(crate::input::HttpSource {
                    url: format!("http://{addr}/module"),
                    digest: digest_of(body.as_bytes()),
                    ..Default::default()
                }),
                ..Default::default()
            })
        };
        assert_eq!(http(AT), Ok(AT.as_bytes().to_vec()));
        assert_eq!(
            http(OVER).unwrap_err(),
            "module exceeds the size limit of 16 bytes"
        );

        let mut registry = Registry::new();
        let at = registry.module(AT.as_bytes(), None, "at");
        let over = registry.module(OVER.as_bytes(), None, "over");
        let addr = registry.serve();
        let oci = |digest: &str| {
            fetch(ModuleSource {
                r#type: "OCI".to_string(),
                oci: Some(crate::input::OciSource {
                    r#ref: format!("{addr}/example/module@{digest}"),
                    ..Default::default()
                }),
                ..Default::default()
            })
        };
        assert_eq!(oci(&at), Ok(AT.as_bytes().to_vec()));
        assert_eq!(
            oci(&over).unwrap_err(),
            "cannot fetch module layer: module exceeds the size limit of 16 bytes"
        );
    }

    /// The OCI runtime holds an image to the limit as a whole: the manifest,
    /// the config and the layers together, each of which fits by itself.
    #[test]
    fn the_size_limit_bounds_an_image_as_a_whole() {
        use crate::testing::registry::{Registry, plain_image_blobs};
        let (manifest, blobs) = plain_image_blobs("plain");
        let total = (manifest.len() + blobs.iter().map(Vec::len).sum::<usize>()) as u64;
        let largest = blobs.iter().map(Vec::len).max().expect("blobs") as u64;
        assert!(largest < total - 1, "no blob is the whole image");

        let mut registry = Registry::new();
        let digest = registry.plain_image("plain");
        let addr = registry.serve();
        let fetch = |limit: u64| {
            let r = Resolver::new(None, limit, None).serving(crate::manifest::TYPE_OCI);
            let resolved = r
                .resolve(
                    &ModuleSource {
                        r#type: "OCI".to_string(),
                        oci: Some(crate::input::OciSource {
                            r#ref: format!("{addr}/example/image@{digest}"),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    None,
                )
                .expect("resolve");
            r.fetch(&resolved).map(|_| ())
        };
        assert_eq!(fetch(total), Ok(()));
        assert_eq!(
            fetch(total - 1),
            Err(format!(
                "image exceeds the size limit of {} bytes (--module-size-limit)",
                total - 1
            ))
        );
    }
}
