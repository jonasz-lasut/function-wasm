//! Module sources resolved to content digests - the Rust port of
//! `internal/module`'s resolvePath and resolveHTTP: a Path source is a file
//! under --module-dir, an HTTP source a URL whose download is verified
//! against the Input's stated digest (digests are stated, never
//! discovered), with fetched blobs kept in the content-addressed store.
//! Refusal strings match the Go runtime.

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
    /// A wasmfn.yaml under --module-dir, read fresh each request.
    PathFile {
        full: PathBuf,
    },
    /// A wasmfn.yaml served beside the module, pinned by its own digest.
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
    /// as empty bytes, for the same reason. A wasmfn.yaml under --module-dir
    /// is never cached: it is read fresh each request, by design.
    cached_manifests: Mutex<HashMap<String, Vec<u8>>>,
}

/// Most manifests the memory tier holds before it starts over; each is at
/// most manifest::MAX_SIZE, so the tier is bounded to a few megabytes.
const MAX_CACHED_MANIFESTS: usize = 4096;

impl Resolver {
    pub fn new(dir: Option<PathBuf>, max_size: u64, blobs: Option<Arc<Store>>) -> Self {
        Resolver {
            dir,
            max_size,
            blobs,
            files: Mutex::new(HashMap::new()),
            http: std::sync::OnceLock::new(),
            manifests: None,
            cached_manifests: Mutex::new(HashMap::new()),
        }
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
                    description: format!("oci {}", oci.r#ref),
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
        function_wasm_engine::metrics::FETCH_DURATION
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
    /// none: a wasmfn.yaml under --module-dir read fresh each request, or
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
                let m = client.manifest(reference)?;
                let Some(layer) = crate::oci::manifest_layer(&m) else {
                    self.remember(&reference.digest, &[]);
                    return Ok(Vec::new());
                };
                let bytes = self.manifest_by_digest(&layer.digest, || {
                    self.verified("manifest layer", &layer.digest, || {
                        client.blob(
                            &layer.digest,
                            crate::manifest::MAX_SIZE as u64,
                            "manifest layer",
                        )
                    })
                })?;
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
        use function_wasm_engine::metrics::{self, CACHE_EVENTS};
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
                .timeout(Duration::from_secs(5 * 60))
                .build()
                .expect("the resolver client's configuration is static")
        });
        let rsp = client
            .get(url)
            .send()
            .map_err(|e| format!("cannot download {what}: {e}"))?;
        if rsp.status() != reqwest::StatusCode::OK {
            return Err(format!("cannot download {what}: {}", rsp.status()));
        }
        read_capped(rsp, limit)
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
        // A lexical check like Go's filepath.Rel: normal components only.
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

/// Normalizes a wasmfn.yaml manifest (YAML) to the JSON bytes
/// manifest::parse decodes - the format an OCI manifest layer already
/// carries. JSON is valid YAML, so an already-JSON manifest passes through.
fn manifest_json(raw: &[u8]) -> Result<Vec<u8>, String> {
    let value: serde_json::Value =
        serde_yaml::from_slice(raw).map_err(|e| format!("manifest is not valid YAML: {e}"))?;
    serde_json::to_vec(&value).map_err(|e| format!("manifest is not valid YAML: {e}"))
}

/// Renders an I/O failure the way Go's os package wraps it ("open <path>:
/// no such file or directory"): these strings reach refusal messages the Go
/// runtime pins, so the Rust runtime prints the same words.
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
        let module_manifest = br#"{"abi":2}"#;
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

        let none = Resolver::new(None, 1 << 20, None);
        assert_eq!(
            none.resolve(&path_source("fn.wasm"), None)
                .expect_err("no dir"),
            "module.path is refused: the function was started without --module-dir"
        );
    }
}
