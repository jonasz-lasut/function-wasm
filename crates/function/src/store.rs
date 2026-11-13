//! The on-disk side of the caches: a
//! content-addressed store of one kind of artifact under the fixed cache
//! directory. Entries are written through a temporary file and a rename so
//! a crash never leaves a partial entry a later get would serve; a
//! verifying store recomputes the digest of what it reads and drops entries
//! that do not match.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use hmac::{KeyInit as _, Mac as _};
use sha2::{Digest, Sha256};

/// Where the runtime keeps its caches - a fixed location: a pod that must
/// survive restarts backs it with a volume, one with a read-only root
/// filesystem mounts an emptyDir there.
pub const DEFAULT_DIR: &str = "/tmp/guestfn-cache";

/// Subdirectories of DEFAULT_DIR.
pub const MODULES_DIR: &str = "modules";
pub const COMPILED_DIR: &str = "compiled";
pub const MANIFESTS_DIR: &str = "manifests";
/// The Sigstore trusted root fetched over TUF, with the verified metadata
/// (trustroot.rs): tuf/<instance>, beside the stores and outside their
/// sweep.
pub const TUF_DIR: &str = "tuf";

/// Compiled-cache directories of other engine versions are removed at
/// startup once this old, so a wasmtime bump does not strand artifacts
/// forever while a rollback within a day still finds its cache warm.
pub const STALE_VERSION_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

const TMP_MARKER: &str = ".put-";
/// The suffix of a keyed entry's sidecar, the HMAC of the entry's bytes.
const MAC_SUFFIX: &str = ".mac";

/// A content-addressed store of one kind of artifact.
pub struct Store {
    dir: PathBuf,
    verify: bool,
    /// With a key, put writes an HMAC-SHA256 sidecar beside each entry and
    /// path hands out only an entry whose sidecar verifies under it: the
    /// compiled store's entries are native code wasmtime loads without a
    /// check of its own, so whatever this process didn't write is refused.
    key: Option<[u8; 32]>,
    /// The entries an engine holds, by path, with how many holds each has:
    /// the sweep leaves them. The lock is kept across the sweep's removal
    /// of an entry, so a hold either precedes the removal and prevents it
    /// or follows it and finds no file.
    held: Mutex<HashMap<PathBuf, usize>>,
}

/// What a keyed lookup found.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// The entry, verified where the store verifies.
    Hit(PathBuf),
    /// No entry.
    Miss,
    /// An entry the store refused and removed: its MAC was missing or wrong,
    /// so it was written by another process or tampered with.
    Stale,
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
        Ok(Store {
            dir,
            verify,
            key: None,
            held: Mutex::default(),
        })
    }

    /// Keys the store: entries put from now on carry an HMAC-SHA256
    /// sidecar under key, and path hands out only entries whose sidecar
    /// verifies. The runtime draws the key at startup and never writes it
    /// anywhere, so a persisted volume's artifacts are refused by the next
    /// process and compiled again, and a planted artifact is never loaded.
    pub fn with_key(mut self, key: [u8; 32]) -> Store {
        self.key = Some(key);
        self
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

    /// Stores b under digest, through a temporary file and a rename, and
    /// returns the entry's path.
    pub fn put(&self, digest: &str, b: &[u8]) -> Result<PathBuf, String> {
        if self.verify && digest_of(b) != digest {
            return Err(format!("content is {}, not {digest}", digest_of(b)));
        }
        let target = self.entry_path(digest);
        write_atomic(&target, b)?;
        if let Some(key) = &self.key {
            // The sidecar lands after the entry: a crash between the two
            // leaves an entry with no MAC, which is a stale miss and a
            // rewrite, never a served entry.
            write_atomic(&mac_path(&target), hex::encode(mac(key, b)).as_bytes())?;
        }
        Ok(target)
    }

    /// Removes what an interrupted write left: a file under the marker of
    /// an unfinished put is no entry, and the sweep never sees it. Call it
    /// before anything writes, as the runtime does at startup.
    pub fn remove_unfinished(&self) {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in dir.flatten() {
            if entry.file_name().to_string_lossy().contains(TMP_MARKER) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// The host filesystem path of the entry stored under digest - for
    /// callers that map the file instead of reading it. An unkeyed store
    /// trusts the entry by name (wasmtime validates what it loads); a keyed
    /// store recomputes the entry's MAC and refuses, removing both files,
    /// an entry whose sidecar is missing or wrong.
    pub fn path(&self, digest: &str) -> Lookup {
        let path = self.entry_path(digest);
        let Ok(st) = std::fs::metadata(&path) else {
            return Lookup::Miss;
        };
        if st.is_dir() {
            return Lookup::Miss;
        }
        if let Some(key) = &self.key {
            let verified = std::fs::read(&path).ok().is_some_and(|b| {
                std::fs::read(mac_path(&path))
                    .ok()
                    .and_then(|tag| hex::decode(tag).ok())
                    .is_some_and(|tag| verify_mac(key, &b, &tag))
            });
            if !verified {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(mac_path(&path));
                return Lookup::Stale;
            }
        }
        touch(&path);
        Lookup::Hit(path)
    }

    fn entry_path(&self, digest: &str) -> PathBuf {
        self.dir.join(file_name(digest))
    }

    /// Removes the entry at path, with its sidecar, unless an engine holds
    /// it. Reports whether the entry went.
    fn remove_unless_held(&self, path: &Path) -> bool {
        let held = self.held.lock().expect("poisoned");
        if held.contains_key(path) || std::fs::remove_file(path).is_err() {
            return false;
        }
        let _ = std::fs::remove_file(mac_path(path));
        true
    }
}

/// One stored blob, for the sweep.
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub last_used: SystemTime,
}

impl Store {
    /// Lists the store's blobs, temporary files and MAC sidecars excluded:
    /// a sidecar goes wherever its entry goes.
    pub fn entries(&self) -> Vec<Entry> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                let name = e.file_name();
                let name = name.to_string_lossy();
                if meta.is_dir() || name.contains(TMP_MARKER) || name.ends_with(MAC_SUFFIX) {
                    return None;
                }
                Some(Entry {
                    path: e.path(),
                    size: disk_size(&meta),
                    last_used: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                })
            })
            .collect()
    }
}

/// What an entry costs the disk: its length, or what the filesystem
/// allocated for it where that is less. A VM's memory file is sparse - as
/// long as the VM's memory, with only the pages the guest touched on disk -
/// and the bound and the gauge are about the disk. A filesystem that keeps
/// a small file inside its inode reports no blocks for it; such a file
/// counts by its length.
fn disk_size(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match meta.blocks() {
            0 => meta.len(),
            blocks => meta.len().min(blocks * 512),
        }
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

/// The store as the place an engine keeps what it derives from a compiled
/// module (the OCI engine's VM snapshots): files written in place, signed
/// under the store's key when sealed, and verified when asked for again,
/// read in pieces since a memory file is as long as a VM's memory.
impl guestfn_engine::ArtifactStore for Store {
    fn begin(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
            return Err(format!("{name:?} is not an entry name"));
        }
        let unfinished = unfinished_path(&self.entry_path(name));
        let _ = std::fs::remove_file(&unfinished);
        Ok(unfinished)
    }

    fn seal(&self, name: &str) -> Result<PathBuf, String> {
        let target = self.entry_path(name);
        let unfinished = unfinished_path(&target);
        let tag = match &self.key {
            Some(key) => Some(
                mac_of_file(key, &unfinished)
                    .map_err(|e| format!("cannot sign cache entry {name}: {e}"))?
                    .finalize()
                    .into_bytes()
                    .to_vec(),
            ),
            None => None,
        };
        // The entry first, its sidecar after, as put does: a crash between
        // the two leaves an entry no lookup serves.
        let _ = std::fs::remove_file(mac_path(&target));
        std::fs::rename(&unfinished, &target).map_err(|e| {
            let _ = std::fs::remove_file(&unfinished);
            format!("cannot store cache entry {name}: {e}")
        })?;
        if let Some(tag) = tag {
            write_atomic(&mac_path(&target), hex::encode(tag).as_bytes())?;
        }
        Ok(target)
    }

    fn sealed(&self, name: &str) -> Option<PathBuf> {
        let path = self.entry_path(name);
        if !std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            return None;
        }
        if let Some(key) = &self.key {
            let verified = std::fs::read(mac_path(&path))
                .ok()
                .and_then(|tag| hex::decode(tag).ok())
                .is_some_and(|tag| {
                    mac_of_file(key, &path).is_ok_and(|m| m.verify_slice(&tag).is_ok())
                });
            if !verified {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(mac_path(&path));
                return None;
            }
        }
        touch(&path);
        Some(path)
    }

    fn used(&self, path: &Path) {
        touch(path);
    }

    fn hold(&self, path: &Path) {
        *self
            .held
            .lock()
            .expect("poisoned")
            .entry(path.to_path_buf())
            .or_default() += 1;
    }

    fn release(&self, path: &Path) {
        let mut held = self.held.lock().expect("poisoned");
        if let Some(holds) = held.get_mut(path) {
            *holds -= 1;
            if *holds == 0 {
                held.remove(path);
            }
        }
    }
}

/// Where an entry is written before it is one.
fn unfinished_path(entry: &Path) -> PathBuf {
    PathBuf::from(format!("{}{TMP_MARKER}keep", entry.display()))
}

/// The HMAC over a file's content, read in pieces: finalized for a
/// sidecar, or verified against one.
fn mac_of_file(key: &[u8; 32], path: &Path) -> std::io::Result<hmac::Hmac<Sha256>> {
    use std::io::Read as _;
    let mut m = hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        m.update(&buf[..n]);
    }
    Ok(m)
}

/// The bytes a store holds right now.
pub fn bytes(store: &Store) -> u64 {
    store.entries().iter().map(|e| e.size).sum()
}
/// Removes least recently used blobs across stores until they hold at most
/// max_bytes together - down to nine tenths of it, so consecutive sweeps do
/// not each remove one entry. Entries are immutable and reproducible, so
/// removal is always safe: the next request fetches or compiles again.
/// The exception is an entry an engine holds (the root filesystem of an
/// image in memory, which every run opens by its path): it counts toward
/// the total and stays, so the stores end above the bound while held
/// entries alone exceed it.
/// Reports the bytes freed; max_bytes 0 sweeps nothing.
pub fn sweep(stores: &[&Store], max_bytes: u64) -> u64 {
    if max_bytes == 0 {
        return 0;
    }
    let mut all: Vec<(&Store, Entry)> = stores
        .iter()
        .flat_map(|s| s.entries().into_iter().map(move |e| (*s, e)))
        .collect();
    let total: u64 = all.iter().map(|(_, e)| e.size).sum();
    if total <= max_bytes {
        return 0;
    }
    let target = max_bytes / 10 * 9;
    all.sort_by_key(|(_, e)| e.last_used);
    let mut remaining = total;
    let mut freed = 0;
    for (store, e) in all {
        if remaining <= target {
            break;
        }
        if store.remove_unless_held(&e.path) {
            remaining -= e.size;
            freed += e.size;
        }
    }
    freed
}

/// Removes every subdirectory of the compiled cache other than the current
/// engine version's, once it has gone unused for STALE_VERSION_AGE - run at
/// startup.
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

/// The sidecar beside a keyed entry.
fn mac_path(entry: &Path) -> PathBuf {
    PathBuf::from(format!("{}{MAC_SUFFIX}", entry.display()))
}

fn mac(key: &[u8; 32], b: &[u8]) -> Vec<u8> {
    let mut m = hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(b);
    m.finalize().into_bytes().to_vec()
}

fn verify_mac(key: &[u8; 32], b: &[u8], tag: &[u8]) -> bool {
    let mut m = hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(b);
    m.verify_slice(tag).is_ok()
}

/// Writes b at target through a temporary file and a rename, so a crash
/// never leaves a partial file a later read would serve.
fn write_atomic(target: &Path, b: &[u8]) -> Result<(), String> {
    let mut nonce = [0u8; 8];
    getrandom(&mut nonce);
    let tmp = PathBuf::from(format!(
        "{}{TMP_MARKER}{}",
        target.display(),
        hex::encode(nonce)
    ));
    std::fs::write(&tmp, b).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write cache entry: {e}")
    })?;
    std::fs::rename(&tmp, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot store cache entry: {e}")
    })
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

    /// A file an engine keeps is written in place, signed when sealed and
    /// verified when asked for again; a changed file or another process's
    /// key is refused and removed, as a planted compiled artifact is.
    #[test]
    fn a_kept_file_is_signed_and_verified() {
        use guestfn_engine::ArtifactStore as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Store::open_dir(dir.path(), false)
            .expect("open")
            .with_key([7; 32]);
        let name = "sha256-abc.vm-576m1c-0011223344ff.mem";
        assert_eq!(s.sealed(name), None);
        let unfinished = s.begin(name).expect("begin");
        std::fs::write(&unfinished, b"a VM's memory").expect("write");
        // Unfinished: no entry for a lookup or for the sweep.
        assert_eq!(s.sealed(name), None);
        assert!(s.entries().is_empty());
        let path = s.seal(name).expect("seal");
        assert_eq!(path, dir.path().join(name));
        assert!(!unfinished.exists());
        assert_eq!(s.sealed(name), Some(path.clone()));
        assert_eq!(std::fs::read(&path).expect("read"), b"a VM's memory");
        assert_eq!(s.entries().len(), 1, "the sidecar is no entry");

        // Another process's key refuses it, and removes it.
        let other = Store::open_dir(dir.path(), false)
            .expect("open")
            .with_key([8; 32]);
        assert_eq!(other.sealed(name), None);
        assert!(!path.exists() && !mac_path(&path).exists());

        // So does a change after the seal.
        std::fs::write(s.begin(name).expect("begin"), b"a VM's memory").expect("write");
        let path = s.seal(name).expect("seal");
        std::fs::write(&path, b"another memory").expect("tamper");
        assert_eq!(s.sealed(name), None);
        assert!(!path.exists());

        for bad in ["", "a/b", "..", "a\\b"] {
            assert!(s.begin(bad).is_err(), "{bad:?}");
        }
    }

    /// What an interrupted write left goes at startup: nothing else would
    /// remove it, since it is no entry.
    #[test]
    fn an_unfinished_write_is_removed_at_startup() {
        use guestfn_engine::ArtifactStore as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Store::open_dir(dir.path(), false).expect("open");
        s.put("sha256:kept", b"entry").expect("put");
        let unfinished = s.begin("left.mem").expect("begin");
        std::fs::write(&unfinished, b"half").expect("write");
        s.remove_unfinished();
        assert!(!unfinished.exists());
        assert_eq!(s.entries().len(), 1);
    }

    /// A sparse entry counts by what the disk holds, not by its length: a
    /// VM's memory file is as long as the VM's memory.
    #[test]
    fn a_sparse_entry_costs_what_the_disk_holds() {
        use guestfn_engine::ArtifactStore as _;
        use std::io::{Seek as _, SeekFrom, Write as _};
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Store::open_dir(dir.path(), false).expect("open");
        let mut f = std::fs::File::create(s.begin("sparse.mem").expect("begin")).expect("create");
        f.write_all(&[1u8; 4096]).expect("write");
        f.seek(SeekFrom::Start((64 << 20) - 4096)).expect("seek");
        f.write_all(&[1u8; 4096]).expect("write");
        drop(f);
        let path = s.seal("sparse.mem").expect("seal");
        let meta = std::fs::metadata(&path).expect("metadata");
        assert_eq!(meta.len(), 64 << 20);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if meta.blocks() * 512 >= meta.len() {
                // A filesystem without holes: nothing to tell apart.
                return;
            }
        }
        let size = bytes(&s);
        assert!((8192..(1 << 20)).contains(&size), "{size}");
        // A dense entry still counts by its length.
        let dense = Store::open_dir(dir.path().join("dense"), false).expect("open");
        dense.put("sha256:d", b"hello").expect("put");
        assert_eq!(bytes(&dense), 5);
    }

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

    /// An entry an engine holds stays whatever its age: the OCI engine
    /// opens a loaded image's root filesystem by its path at every run.
    /// The sweep takes the next entries in its place, and the held one
    /// once every hold on it is released.
    #[test]
    fn the_sweep_leaves_a_held_entry() {
        use guestfn_engine::ArtifactStore as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Store::open_dir(dir.path(), false)
            .expect("open")
            .with_key([7; 32]);
        let age = |path: &Path, secs: u64| {
            let past = SystemTime::now() - std::time::Duration::from_secs(secs);
            let times = std::fs::FileTimes::new()
                .set_accessed(past)
                .set_modified(past);
            std::fs::File::options()
                .append(true)
                .open(path)
                .expect("open")
                .set_times(times)
                .expect("times");
        };
        let oldest = s.put("sha256:oldest", &[0u8; 400]).expect("put");
        let older = s.put("sha256:older", &[0u8; 400]).expect("put");
        let newest = s.put("sha256:newest", &[0u8; 400]).expect("put");
        age(&oldest, 7200);
        age(&older, 3600);

        // Held twice, as an entry two loads share.
        s.hold(&oldest);
        s.hold(&oldest);
        assert_eq!(sweep(&[&s], 1000), 400, "the next least recently used");
        assert!(oldest.exists() && mac_path(&oldest).exists());
        assert!(!older.exists() && newest.exists());

        // Held entries alone above the bound: everything else goes, and
        // the held entry still counts.
        assert_eq!(sweep(&[&s], 300), 400);
        assert!(oldest.exists() && !newest.exists());
        assert_eq!(sweep(&[&s], 300), 0);
        assert_eq!(bytes(&s), 400);

        s.release(&oldest);
        assert_eq!(sweep(&[&s], 300), 0, "one hold is left");
        s.release(&oldest);
        assert_eq!(sweep(&[&s], 300), 400);
        assert!(!oldest.exists() && !mac_path(&oldest).exists());
    }

    #[test]
    fn hands_out_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = Store::open_dir(dir.path(), false).expect("open");
        assert_eq!(s.path("sha256:missing"), Lookup::Miss);
        s.put("sha256:x", b"artifact").expect("put");
        let Lookup::Hit(p) = s.path("sha256:x") else {
            panic!("an unkeyed entry is served by name");
        };
        assert_eq!(std::fs::read(p).expect("read"), b"artifact");
    }

    #[test]
    fn a_keyed_store_serves_only_what_it_wrote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = [7u8; 32];
        let s = Store::open_dir(dir.path(), false)
            .expect("open")
            .with_key(key);
        s.put("sha256:x", b"artifact").expect("put");
        let Lookup::Hit(p) = s.path("sha256:x") else {
            panic!("an entry this store wrote verifies");
        };
        assert_eq!(std::fs::read(&p).expect("read"), b"artifact");
        // The sidecar is not an entry: the sweep and the gauge skip it.
        assert_eq!(s.entries().len(), 1);

        // Another process's key refuses the entry and removes both files.
        let other = Store::open_dir(dir.path(), false)
            .expect("open")
            .with_key([8u8; 32]);
        assert_eq!(other.path("sha256:x"), Lookup::Stale);
        assert_eq!(s.path("sha256:x"), Lookup::Miss);

        // A planted entry has no sidecar; a tampered one has the wrong MAC.
        std::fs::write(dir.path().join(file_name("sha256:planted")), b"planted").expect("write");
        assert_eq!(s.path("sha256:planted"), Lookup::Stale);
        s.put("sha256:y", b"artifact").expect("put");
        std::fs::write(dir.path().join(file_name("sha256:y")), b"tampered").expect("write");
        assert_eq!(s.path("sha256:y"), Lookup::Stale);
        assert!(!dir.path().join("sha256-y.mac").exists());
    }
}
