//! The on-disk side of the caches - the Rust port of `internal/cache`: a
//! content-addressed store of one kind of artifact under the fixed cache
//! directory. Entries are written through a temporary file and a rename so
//! a crash never leaves a partial entry a later get would serve; a
//! verifying store recomputes the digest of what it reads and drops entries
//! that do not match.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// Where the runtime keeps its caches - a fixed location: a pod that must
/// survive restarts backs it with a volume, one with a read-only root
/// filesystem mounts an emptyDir there.
pub const DEFAULT_DIR: &str = "/tmp/function-wasm-cache";

/// Subdirectories of DEFAULT_DIR.
pub const MODULES_DIR: &str = "modules";
pub const COMPILED_DIR: &str = "compiled";
pub const MANIFESTS_DIR: &str = "manifests";

/// Compiled-cache directories of other engine versions are removed at
/// startup once this old, so a wasmtime bump does not strand artifacts
/// forever while a rollback within a day still finds its cache warm.
pub const STALE_VERSION_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

const TMP_MARKER: &str = ".put-";

/// A content-addressed store of one kind of artifact.
pub struct Store {
    dir: PathBuf,
    verify: bool,
}

impl Store {
    /// Opens a store on the host filesystem directory dir, creating it.
    /// With verify, get recomputes the digest of what it read - for fetched
    /// modules, whose digest is their address; serialized code is addressed
    /// by the module's digest and wasmtime validates it on load instead.
    pub fn open_dir(dir: impl Into<PathBuf>, verify: bool) -> Result<Store, String> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create cache directory {}: {e}", dir.display()))?;
        Ok(Store { dir, verify })
    }

    /// Returns the artifact stored under digest. (The blob and manifest
    /// stores' read path, consumed once the OCI/HTTP sources land.)
    #[allow(dead_code)]
    pub fn get(&self, digest: &str) -> Option<Vec<u8>> {
        let path = self.entry_path(digest);
        let b = std::fs::read(&path).ok()?;
        if self.verify && digest_of(&b) != digest {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        touch(&path);
        Some(b)
    }

    /// Stores b under digest, through a temporary file and a rename.
    pub fn put(&self, digest: &str, b: &[u8]) -> Result<(), String> {
        if self.verify && digest_of(b) != digest {
            return Err(format!("content is {}, not {digest}", digest_of(b)));
        }
        let mut nonce = [0u8; 8];
        getrandom(&mut nonce);
        let target = self.entry_path(digest);
        let tmp = PathBuf::from(format!(
            "{}{TMP_MARKER}{}",
            target.display(),
            hex::encode(nonce)
        ));
        std::fs::write(&tmp, b).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write cache entry: {e}")
        })?;
        std::fs::rename(&tmp, &target).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot store cache entry: {e}")
        })
    }

    /// The host filesystem path of the entry stored under digest, when it
    /// exists - for callers that map the file instead of reading it
    /// (verification is theirs; the compiled store does not verify, wasmtime
    /// validates what it loads).
    pub fn path(&self, digest: &str) -> Option<PathBuf> {
        let path = self.entry_path(digest);
        let st = std::fs::metadata(&path).ok()?;
        if st.is_dir() {
            return None;
        }
        touch(&path);
        Some(path)
    }

    fn entry_path(&self, digest: &str) -> PathBuf {
        self.dir.join(file_name(digest))
    }
}

/// One stored blob, for the sweep.
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub last_used: SystemTime,
}

impl Store {
    /// Lists the store's blobs, temporary files excluded.
    pub fn entries(&self) -> Vec<Entry> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                if meta.is_dir() || e.file_name().to_string_lossy().contains(TMP_MARKER) {
                    return None;
                }
                Some(Entry {
                    path: e.path(),
                    size: meta.len(),
                    last_used: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                })
            })
            .collect()
    }
}

/// The bytes a store holds right now.
pub fn bytes(store: &Store) -> u64 {
    store.entries().iter().map(|e| e.size).sum()
}
/// Removes least recently used blobs across stores until they hold at most
/// max_bytes together - down to nine tenths of it, so consecutive sweeps do
/// not each remove one entry. Entries are immutable and reproducible, so
/// removal is always safe: the next request fetches or compiles again.
/// Reports the bytes freed; max_bytes 0 sweeps nothing.
pub fn sweep(stores: &[&Store], max_bytes: u64) -> u64 {
    if max_bytes == 0 {
        return 0;
    }
    let mut all: Vec<Entry> = stores.iter().flat_map(|s| s.entries()).collect();
    let total: u64 = all.iter().map(|e| e.size).sum();
    if total <= max_bytes {
        return 0;
    }
    let target = max_bytes / 10 * 9;
    all.sort_by_key(|e| e.last_used);
    let mut remaining = total;
    let mut freed = 0;
    for e in all {
        if remaining <= target {
            break;
        }
        if std::fs::remove_file(&e.path).is_ok() {
            remaining -= e.size;
            freed += e.size;
        }
    }
    freed
}

/// Removes every subdirectory of the compiled cache other than the current
/// engine version's, once it has gone unused for STALE_VERSION_AGE - run at
/// startup, as the Go runtime does.
pub fn remove_stale_versions(compiled_dir: &Path, current: &str) {
    let Ok(entries) = std::fs::read_dir(compiled_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy() == current {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > STALE_VERSION_AGE);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// A read is a use: the (future) sweep drops least recently used entries.
/// Best effort - a read-only volume still serves.
fn touch(path: &Path) {
    let now = std::fs::FileTimes::new()
        .set_accessed(SystemTime::now())
        .set_modified(SystemTime::now());
    if let Ok(f) = std::fs::File::options().append(true).open(path) {
        let _ = f.set_times(now);
    }
}

/// The digest as a file name: "sha256:<hex>" stored as "sha256-<hex>".
fn file_name(digest: &str) -> String {
    digest.replace(':', "-")
}

fn digest_of(b: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(b)))
}

fn getrandom(buf: &mut [u8]) {
    // A nonce for a temporary file name needs uniqueness, not
    // cryptographic strength; hash a few points of process entropy.
    let seed = format!(
        "{:?}-{}-{:?}",
        SystemTime::now(),
        std::process::id(),
        std::thread::current().id()
    );
    let h = Sha256::digest(seed.as_bytes());
    buf.copy_from_slice(&h[..buf.len()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_verifies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Store::open_dir(dir.path().join("modules"), true).expect("open");
        let digest = digest_of(b"hello");
        s.put(&digest, b"hello").expect("put");
        assert_eq!(s.get(&digest), Some(b"hello".to_vec()));
        // A corrupted entry is dropped rather than served.
        std::fs::write(
            dir.path().join("modules").join(file_name(&digest)),
            b"tampered",
        )
        .expect("write");
        assert_eq!(s.get(&digest), None);
        // Content that does not match its address is refused.
        assert!(s.put(&digest, b"other").is_err());
    }

    #[test]
    fn sweeps_least_recently_used_entries_across_stores() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = Store::open_dir(dir.path().join("a"), false).expect("open");
        let b = Store::open_dir(dir.path().join("b"), false).expect("open");
        a.put("sha256:old", &[0u8; 400]).expect("put");
        b.put("sha256:new", &[0u8; 400]).expect("put");
        // Age the first entry so the sweep picks it.
        let old = dir.path().join("a").join("sha256-old");
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        let times = std::fs::FileTimes::new()
            .set_accessed(past)
            .set_modified(past);
        std::fs::File::options()
            .append(true)
            .open(&old)
            .expect("open")
            .set_times(times)
            .expect("times");

        assert_eq!(sweep(&[&a, &b], 0), 0, "0 sweeps nothing");
        assert_eq!(sweep(&[&a, &b], 2000), 0, "under the bound sweeps nothing");
        let freed = sweep(&[&a, &b], 700);
        assert_eq!(freed, 400, "the least recently used entry goes");
        assert!(a.get("sha256:old").is_none());
        assert!(b.get("sha256:new").is_some());
    }

    #[test]
    fn hands_out_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Store::open_dir(dir.path(), false).expect("open");
        assert!(s.path("sha256:missing").is_none());
        s.put("sha256:x", b"artifact").expect("put");
        let p = s.path("sha256:x").expect("path");
        assert_eq!(std::fs::read(p).expect("read"), b"artifact");
    }
}
