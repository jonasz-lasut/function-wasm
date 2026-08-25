//! The Path module source: a file under --module-dir, resolved to a content
//! digest and fetched with the digest re-verified - the Rust port of
//! `internal/module`'s resolvePath. Refusal strings match the Go runtime.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// Remembers the digest of a served file by size and modification time, so
/// an unchanged module is not re-hashed on every request.
#[derive(Clone)]
struct FileStamp {
    size: u64,
    mtime: SystemTime,
    digest: String,
}

/// A resolved module: its content digest, the description refusals and logs
/// name it by, and where to fetch it.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub digest: String,
    pub description: String,
    full: PathBuf,
}

pub struct Resolver {
    dir: Option<PathBuf>,
    max_size: u64,
    files: Mutex<HashMap<PathBuf, FileStamp>>,
}

impl Resolver {
    pub fn new(dir: Option<PathBuf>, max_size: u64) -> Self {
        Resolver {
            dir,
            max_size,
            files: Mutex::new(HashMap::new()),
        }
    }

    pub fn resolve(&self, rel: &str) -> Result<Resolved, String> {
        let full = self.confined_path("module.path", rel)?;
        let digest = self.file_digest(&full)?;
        Ok(Resolved {
            digest,
            description: format!("module file {rel}"),
            full,
        })
    }

    /// Reads the resolved module, re-verified against the digest resolve
    /// stamped: a same-size rewrite within the mtime granularity is caught
    /// here and forgotten, so the next request re-hashes.
    pub fn fetch(&self, resolved: &Resolved) -> Result<Vec<u8>, String> {
        let f = std::fs::File::open(&resolved.full).map_err(|e| {
            format!(
                "cannot read module file: {}",
                go_io_error("open", &resolved.full, &e)
            )
        })?;
        let b = read_capped(f, self.max_size)?;
        let got = digest_of(&b);
        if got != resolved.digest {
            self.files.lock().expect("poisoned").remove(&resolved.full);
            return Err(format!(
                "module file changed while being read: content is {got}, expected {}",
                resolved.digest
            ));
        }
        Ok(b)
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

impl Resolver {
    /// Reads a wasmfn.yaml manifest named by module.manifestPath, confined
    /// under --module-dir like the module and normalized to JSON the way an
    /// OCI manifest layer arrives - read fresh each request (a path file may
    /// change), the directory being the operator's.
    pub fn read_manifest(&self, rel: &str) -> Result<Vec<u8>, String> {
        let full = self.confined_path("module.manifestPath", rel)?;
        let f = std::fs::File::open(&full).map_err(|e| {
            format!(
                "cannot read manifest file: {}",
                go_io_error("open", &full, &e)
            )
        })?;
        let b = read_capped(f, crate::manifest::MAX_SIZE as u64)?;
        let value: serde_json::Value =
            serde_yaml::from_slice(&b).map_err(|e| format!("manifest is not valid YAML: {e}"))?;
        serde_json::to_vec(&value).map_err(|e| format!("manifest is not valid YAML: {e}"))
    }
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

fn read_capped(f: std::fs::File, limit: u64) -> Result<Vec<u8>, String> {
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

    #[test]
    fn resolves_and_fetches_a_module_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fn.wasm"), b"not really wasm").expect("write");
        let r = Resolver::new(Some(dir.path().to_owned()), 1 << 20);
        let resolved = r.resolve("fn.wasm").expect("resolve");
        assert_eq!(resolved.description, "module file fn.wasm");
        assert!(resolved.digest.starts_with("sha256:"));
        assert_eq!(r.fetch(&resolved).expect("fetch"), b"not really wasm");
        // The second resolve hits the stamp.
        assert_eq!(
            r.resolve("fn.wasm").expect("resolve").digest,
            resolved.digest
        );
    }

    #[test]
    fn refusals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = Resolver::new(Some(dir.path().to_owned()), 1 << 20);
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
            assert_eq!(&r.resolve(rel).expect_err(rel), want);
        }

        let none = Resolver::new(None, 1 << 20);
        assert_eq!(
            none.resolve("fn.wasm").expect_err("no dir"),
            "module.path is refused: the function was started without --module-dir"
        );
    }
}
