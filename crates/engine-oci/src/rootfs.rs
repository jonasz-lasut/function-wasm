//! The image's root filesystem: its layers applied in order, whiteouts
//! and all, into a tree held in memory, then written as a squashfs the VM
//! mounts read-only under its tmpfs overlay. Ownership and modes come from
//! the layers' tar headers, so no root is needed to keep them, and the
//! engine's own config goes in beside the image's files, under a path no
//! image uses, so a stored root filesystem describes itself.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::Arc;

use crate::Error;

/// Where the engine keeps the image config inside the root filesystem.
pub const CONFIG_PATH: &str = ".guestfn/config.json";

/// An image may unpack to this many times the size limit its layers were
/// fetched under: the rule the WebAssembly runtime's tar layers follow.
pub const EXPANSION_FACTOR: u64 = 8;

/// The most tar entries an image's layers may hold between them.
pub const MAX_ENTRIES: u64 = 1_000_000;

/// One entry of the tree, keyed by its path with no leading slash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    /// The permission bits.
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// Shared, so a hard link costs no second copy of the bytes.
    File(Arc<[u8]>),
    Dir,
    Symlink(String),
    Char(u32, u32),
    Block(u32, u32),
    Fifo,
}

#[derive(Debug)]
pub struct Tree {
    nodes: BTreeMap<String, Node>,
    /// The bytes the image may unpack to, and how many of them are left.
    max: u64,
    left: u64,
    /// The tar entries the image may hold, and how many were read so far,
    /// over every layer.
    max_entries: u64,
    entries: u64,
}

impl Default for Tree {
    fn default() -> Tree {
        Tree::with_size_limit(crate::DEFAULT_MODULE_SIZE_LIMIT)
    }
}

/// Counts what a layer unpacks to against the bytes the image has left,
/// and ends the read where they run out.
struct Capped<'a, R> {
    inner: R,
    left: &'a Cell<u64>,
    exceeded: &'a Cell<bool>,
}

impl<R: Read> Read for Capped<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.left.get();
        // One byte past the bound tells an image over it from one that
        // ends on it.
        let want = usize::try_from(left.saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let n = self.inner.read(&mut buf[..want])?;
        if n as u64 > left {
            self.exceeded.set(true);
            return Err(std::io::Error::other("the image's size bound"));
        }
        self.left.set(left - n as u64);
        Ok(n)
    }
}

/// A layer's media type says how it is packed; a gzip magic says so too,
/// whatever the type.
pub fn is_gzip(media_type: &str, bytes: &[u8]) -> bool {
    media_type.ends_with("+gzip")
        || media_type.ends_with(".gzip")
        || bytes.starts_with(&[0x1f, 0x8b])
}

impl Tree {
    /// A tree bounded by the default size limit.
    pub fn new() -> Tree {
        Tree::default()
    }

    /// A tree for an image fetched under `limit` bytes
    /// (--module-size-limit): its layers may unpack to EXPANSION_FACTOR
    /// times that between them.
    pub fn with_size_limit(limit: u64) -> Tree {
        let max = limit.saturating_mul(EXPANSION_FACTOR);
        Tree {
            nodes: BTreeMap::new(),
            max,
            left: max,
            max_entries: MAX_ENTRIES,
            entries: 0,
        }
    }

    fn over_the_bound(&self) -> Error {
        Error(format!(
            "image unpacks to more than {} bytes, eight times the size limit (--module-size-limit)",
            self.max
        ))
    }

    pub fn get(&self, path: &str) -> Option<&Node> {
        self.nodes.get(path.trim_matches('/'))
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// A regular file's bytes as text, if the path is one.
    pub fn text(&self, path: &str) -> Option<String> {
        match &self.get(path)?.kind {
            NodeKind::File(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }

    /// Puts an entry, making the directories above it as root 0755 where
    /// a layer left them out.
    pub fn insert(&mut self, path: &str, node: Node) {
        let path = path.trim_matches('/').to_string();
        if path.is_empty() {
            return;
        }
        let mut prefix = String::new();
        for part in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if prefix == path {
                break;
            }
            if !self.nodes.contains_key(&prefix) {
                self.nodes.insert(
                    prefix.clone(),
                    Node {
                        kind: NodeKind::Dir,
                        mode: 0o755,
                        uid: 0,
                        gid: 0,
                        mtime: 0,
                    },
                );
            }
        }
        // A file replacing a directory takes the subtree with it.
        if !matches!(node.kind, NodeKind::Dir) {
            self.remove_children(&path);
        }
        self.nodes.insert(path, node);
    }

    fn remove(&mut self, path: &str) {
        self.remove_children(path);
        self.nodes.remove(path);
    }

    fn remove_children(&mut self, path: &str) {
        let prefix = format!("{path}/");
        let doomed: Vec<String> = self
            .nodes
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, _)| k.clone())
            .collect();
        for k in doomed {
            self.nodes.remove(&k);
        }
    }

    /// Applies one layer: its entries over the tree, a `.wh.<name>` entry
    /// removing `<name>` and an opaque marker (`.wh..wh..opq`) emptying its
    /// directory of what the layers below put there. A hard link shares
    /// its target's bytes. Every byte the layer unpacks to counts against
    /// the image's bound, and the read stops where the bound is passed.
    pub fn apply_layer(&mut self, media_type: &str, bytes: &[u8]) -> Result<(), Error> {
        if media_type.ends_with("+zstd") || bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
            return Err(Error(
                "layer is compressed with zstd, which the OCI runtime does not unpack yet; build the image with gzip layers (the default)".to_string(),
            ));
        }
        let reader: Box<dyn Read> = if is_gzip(media_type, bytes) {
            Box::new(flate2::read::GzDecoder::new(bytes))
        } else {
            Box::new(bytes)
        };
        let over = self.over_the_bound().0;
        let left = Cell::new(self.left);
        let exceeded = Cell::new(false);
        // A read the bound ended says so, whatever the tar reader made of
        // the failure.
        let failed = |what: String, e: std::io::Error| {
            if exceeded.get() {
                Error(over.clone())
            } else {
                Error(format!("{what}: {e}"))
            }
        };
        let mut archive = tar::Archive::new(Capped {
            inner: reader,
            left: &left,
            exceeded: &exceeded,
        });
        let entries = archive
            .entries()
            .map_err(|e| failed("cannot read layer".to_string(), e))?;
        for entry in entries {
            let mut entry = entry.map_err(|e| failed("cannot read layer".to_string(), e))?;
            self.entries += 1;
            if self.entries > self.max_entries {
                return Err(Error(format!(
                    "image holds more than {} entries, the most the OCI runtime unpacks",
                    self.max_entries
                )));
            }
            let raw = entry
                .path()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let path = normalize(&raw);
            if path.is_empty() {
                continue;
            }
            let (dir, name) = match path.rsplit_once('/') {
                Some((d, n)) => (d.to_string(), n.to_string()),
                None => (String::new(), path.clone()),
            };
            if name == ".wh..wh..opq" {
                if !dir.is_empty() {
                    self.remove_children(&dir);
                }
                continue;
            }
            if let Some(target) = name.strip_prefix(".wh.") {
                let victim = if dir.is_empty() {
                    target.to_string()
                } else {
                    format!("{dir}/{target}")
                };
                self.remove(&victim);
                continue;
            }
            let header = entry.header();
            let mode = (header.mode().unwrap_or(0o644) & 0o7777) as u16;
            let uid = header.uid().unwrap_or(0) as u32;
            let gid = header.gid().unwrap_or(0) as u32;
            let mtime = header.mtime().unwrap_or(0).min(u32::MAX as u64) as u32;
            let kind = match header.entry_type() {
                tar::EntryType::Directory => NodeKind::Dir,
                tar::EntryType::Symlink => NodeKind::Symlink(
                    entry
                        .link_name()
                        .ok()
                        .flatten()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ),
                tar::EntryType::Link => {
                    let target = entry
                        .link_name()
                        .ok()
                        .flatten()
                        .map(|p| normalize(&p.to_string_lossy()))
                        .unwrap_or_default();
                    match self.nodes.get(&target).map(|n| n.kind.clone()) {
                        Some(k) => k,
                        None => {
                            return Err(Error(format!(
                                "layer links {path} to {target}, which is not in the image"
                            )));
                        }
                    }
                }
                tar::EntryType::Char => NodeKind::Char(
                    header.device_major().ok().flatten().unwrap_or(0),
                    header.device_minor().ok().flatten().unwrap_or(0),
                ),
                tar::EntryType::Block => NodeKind::Block(
                    header.device_major().ok().flatten().unwrap_or(0),
                    header.device_minor().ok().flatten().unwrap_or(0),
                ),
                tar::EntryType::Fifo => NodeKind::Fifo,
                tar::EntryType::Regular
                | tar::EntryType::Continuous
                | tar::EntryType::GNUSparse => {
                    // The header's size is a claim: the buffer grows with
                    // what is read, and the bound ends the read.
                    let before = left.get();
                    let mut data = Vec::new();
                    (&mut entry)
                        .take(before.saturating_add(1))
                        .read_to_end(&mut data)
                        .map_err(|e| failed(format!("cannot read layer entry {path}"), e))?;
                    // A sparse entry reads longer than the layer stores
                    // it: its holes count too.
                    let stored = before - left.get();
                    let holes = (data.len() as u64).saturating_sub(stored);
                    if holes > left.get() {
                        return Err(Error(over));
                    }
                    left.set(left.get() - holes);
                    NodeKind::File(data.into())
                }
                // Extended headers, long names and the rest are the tar
                // crate's to fold into the entries above.
                _ => continue,
            };
            self.insert(
                &path,
                Node {
                    kind,
                    mode,
                    uid,
                    gid,
                    mtime,
                },
            );
        }
        self.left = left.get();
        Ok(())
    }

    /// Writes the tree as a squashfs, with `extra` files put in beside it
    /// (the engine's config), gzip-compressed, times zeroed so the same
    /// image gives the same bytes.
    pub fn write_squashfs(
        &self,
        extra: &[(&str, &[u8])],
        out: &mut (impl Write + Seek),
    ) -> Result<(), Error> {
        use backhand::{
            FilesystemCompressor, FilesystemWriter, NodeHeader, compression::Compressor,
        };
        let mut fs = FilesystemWriter::default();
        fs.set_time(0);
        fs.set_root_mode(0o755);
        fs.set_compressor(
            FilesystemCompressor::new(Compressor::Gzip, None)
                .map_err(|e| Error(format!("cannot configure the root filesystem: {e}")))?,
        );
        let fail = |path: &str, e: backhand::BackhandError| {
            Error(format!("cannot write {path} into the root filesystem: {e}"))
        };
        for (path, node) in &self.nodes {
            let header = NodeHeader::new(node.mode, node.uid, node.gid, node.mtime);
            let at = format!("/{path}");
            match &node.kind {
                NodeKind::Dir => fs.push_dir(&at, header).map_err(|e| fail(path, e))?,
                NodeKind::File(data) => fs
                    .push_file(std::io::Cursor::new(&data[..]), &at, header)
                    .map_err(|e| fail(path, e))?,
                NodeKind::Symlink(target) => fs
                    .push_symlink(target.as_str(), &at, header)
                    .map_err(|e| fail(path, e))?,
                NodeKind::Char(major, minor) => fs
                    .push_char_device(device_number(*major, *minor), &at, header)
                    .map_err(|e| fail(path, e))?,
                NodeKind::Block(major, minor) => fs
                    .push_block_device(device_number(*major, *minor), &at, header)
                    .map_err(|e| fail(path, e))?,
                NodeKind::Fifo => fs.push_fifo(&at, header).map_err(|e| fail(path, e))?,
            }
        }
        let root_header = NodeHeader::new(0o700, 0, 0, 0);
        let mut made = std::collections::BTreeSet::new();
        for (path, data) in extra {
            let mut prefix = String::new();
            for part in path.trim_matches('/').split('/') {
                let parent = prefix.clone();
                prefix = if parent.is_empty() {
                    part.to_string()
                } else {
                    format!("{parent}/{part}")
                };
                if prefix == path.trim_matches('/') {
                    break;
                }
                if made.insert(prefix.clone()) && !self.nodes.contains_key(&prefix) {
                    fs.push_dir(format!("/{prefix}"), root_header)
                        .map_err(|e| fail(&prefix, e))?;
                }
            }
            fs.push_file(
                std::io::Cursor::new(*data),
                format!("/{}", path.trim_matches('/')),
                NodeHeader::new(0o600, 0, 0, 0),
            )
            .map_err(|e| fail(path, e))?;
        }
        fs.write(out)
            .map_err(|e| Error(format!("cannot write the root filesystem: {e}")))?;
        Ok(())
    }
}

/// A squashfs device number in the kernel's encoding.
fn device_number(major: u32, minor: u32) -> u32 {
    ((major & 0xfff) << 8) | (minor & 0xff) | ((minor & 0xfff00) << 12)
}

/// A tar entry's path as a tree key: no leading `./` or `/`, no trailing
/// slash, no `.` segments.
pub fn normalize(raw: &str) -> String {
    raw.split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Reads one file out of a squashfs on disk: how a stored root filesystem
/// gives its config back.
pub fn read_file(image: &Path, path: &str) -> Result<Vec<u8>, Error> {
    use backhand::{FilesystemReader, InnerNode};
    let f = std::fs::File::open(image)
        .map_err(|e| Error(format!("cannot open {}: {e}", image.display())))?;
    let fs = FilesystemReader::from_reader(std::io::BufReader::new(f))
        .map_err(|e| Error(format!("cannot read {}: {e}", image.display())))?;
    let want = format!("/{}", path.trim_matches('/'));
    for node in fs.files() {
        if node.fullpath.to_string_lossy() != want {
            continue;
        }
        if let InnerNode::File(file) = &node.inner {
            let mut data = Vec::new();
            fs.file(file)
                .reader()
                .read_to_end(&mut data)
                .map_err(|e| Error(format!("cannot read {want} from {}: {e}", image.display())))?;
            return Ok(data);
        }
    }
    Err(Error(format!(
        "{} holds no {want}: not a root filesystem this engine wrote",
        image.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Layer, gzip, tar_layer};

    const TAR: &str = "application/vnd.oci.image.layer.v1.tar";
    const TAR_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

    fn refusal(t: &mut Tree, layer: &[u8]) -> String {
        t.apply_layer(TAR, layer).unwrap_err().0
    }

    /// The bound is on what the layers unpack to between them, eight times
    /// the size limit, and a layer that ends on it is admitted.
    #[test]
    fn an_image_over_the_expanded_size_bound_is_refused() {
        const OVER: &str = "image unpacks to more than 8192 bytes, eight times the size limit (--module-size-limit)";
        // A gzip layer far smaller than what it unpacks to.
        let zeros = vec![0u8; 1 << 20];
        let packed = gzip(&Layer::new().file("zeros", &zeros, 0o644).finish());
        assert!(packed.len() < zeros.len() / 64);
        let mut t = Tree::with_size_limit(1024);
        assert_eq!(t.apply_layer(TAR_GZIP, &packed).unwrap_err().0, OVER);

        // Header, data and the block that ends the archive: 512 + 2560 +
        // 512 bytes a layer, so two fit and the third passes the bound.
        let layer = |name: &str| Layer::new().file(name, &[1u8; 2560], 0o644).finish();
        let mut t = Tree::with_size_limit(1024);
        t.apply_layer(TAR, &layer("a")).unwrap();
        t.apply_layer(TAR, &layer("b")).unwrap();
        assert_eq!(refusal(&mut t, &layer("c")), OVER);
        assert_eq!(t.get("b").unwrap().kind, NodeKind::File([1u8; 2560].into()));

        // 512 + 7168 + 512 = 8192: on the bound.
        let mut t = Tree::with_size_limit(1024);
        t.apply_layer(TAR, &Layer::new().file("a", &[1u8; 7168], 0o644).finish())
            .unwrap();
        assert_eq!(t.left, 0);
    }

    /// A header may state any size: nothing is allocated from it, and the
    /// read ends with the layer or at the bound.
    #[test]
    fn a_declared_size_is_never_allocated() {
        let layer = Layer::new().declaring("big", 1 << 50, b"tiny").finish();
        assert_eq!(
            refusal(&mut Tree::new(), &layer),
            "cannot read layer: unexpected EOF during skip"
        );
        let mut data = vec![0u8; 16 << 10];
        data.extend_from_slice(&[0u8; 1024]);
        let layer = gzip(&Layer::new().declaring("big", 1 << 50, &data).finish());
        assert_eq!(
            Tree::with_size_limit(1024)
                .apply_layer(TAR_GZIP, &layer)
                .unwrap_err()
                .0,
            "image unpacks to more than 8192 bytes, eight times the size limit (--module-size-limit)"
        );
    }

    #[test]
    fn an_image_with_too_many_entries_is_refused() {
        let layer = Layer::new()
            .dir("a", 0o755)
            .file("a/b", b"", 0o644)
            .whiteout("c")
            .finish();
        let mut t = Tree::new();
        t.max_entries = 5;
        t.apply_layer(TAR, &layer).unwrap();
        assert_eq!(
            refusal(&mut t, &layer),
            "image holds more than 5 entries, the most the OCI runtime unpacks"
        );
    }

    /// A hard link is its target's bytes, not a copy of them, with the
    /// link's own header for the rest.
    #[test]
    fn a_hard_link_shares_its_targets_bytes() {
        let mut t = Tree::new();
        t.apply_layer(
            TAR,
            &Layer::new()
                .file("bin/tool", b"#!", 0o4755)
                .link("bin/alias", "bin/tool")
                .finish(),
        )
        .unwrap();
        let (NodeKind::File(tool), NodeKind::File(alias)) = (
            &t.get("bin/tool").unwrap().kind,
            &t.get("bin/alias").unwrap().kind,
        ) else {
            panic!("two files");
        };
        assert!(Arc::ptr_eq(tool, alias));
        assert_eq!(t.get("bin/tool").unwrap().mode, 0o4755);
        assert_eq!(t.get("bin/alias").unwrap().mode, 0o644);
    }

    #[test]
    fn a_hard_link_to_nothing_is_refused() {
        let layer = Layer::new().link("bin/alias", "bin/tool").finish();
        assert_eq!(
            refusal(&mut Tree::new(), &layer),
            "layer links bin/alias to bin/tool, which is not in the image"
        );
    }

    #[test]
    fn layers_apply_in_order_with_whiteouts() {
        let mut t = Tree::new();
        t.apply_layer(
            "application/vnd.oci.image.layer.v1.tar",
            &tar_layer(&[
                ("etc/", None, 0o755),
                ("etc/passwd", Some(b"root:x:0:0:::\n".as_slice()), 0o644),
                ("etc/old", Some(b"gone".as_slice()), 0o644),
                ("opt/a/x", Some(b"a".as_slice()), 0o600),
                ("opt/a/y", Some(b"a".as_slice()), 0o600),
                ("bin/", None, 0o755),
                ("bin/tool", Some(b"#!".as_slice()), 0o755),
            ]),
        )
        .unwrap();
        t.apply_layer(
            "application/vnd.oci.image.layer.v1.tar+gzip",
            &crate::testing::gzip(&tar_layer(&[
                ("etc/.wh.old", Some(b"".as_slice()), 0o644),
                ("opt/a/.wh..wh..opq", Some(b"".as_slice()), 0o644),
                ("opt/a/z", Some(b"b".as_slice()), 0o600),
                ("bin/tool", Some(b"#!v2".as_slice()), 0o755),
            ])),
        )
        .unwrap();
        assert!(t.get("etc/old").is_none());
        assert_eq!(t.text("etc/passwd").unwrap(), "root:x:0:0:::\n");
        assert!(t.get("opt/a/x").is_none() && t.get("opt/a/y").is_none());
        assert_eq!(t.text("opt/a/z").unwrap(), "b");
        assert_eq!(t.text("bin/tool").unwrap(), "#!v2");
        assert_eq!(t.get("bin/tool").unwrap().mode, 0o755);
        // The directory a layer left out exists as root 0755.
        assert_eq!(t.get("opt").unwrap().kind, NodeKind::Dir);
        assert!(
            t.apply_layer("application/vnd.oci.image.layer.v1.tar+zstd", b"xx")
                .unwrap_err()
                .0
                .contains("zstd")
        );
    }

    #[test]
    fn a_squashfs_round_trips_with_the_config_beside_the_files() {
        let mut t = Tree::new();
        t.apply_layer(
            "application/vnd.oci.image.layer.v1.tar",
            &tar_layer(&[
                ("app/", None, 0o755),
                ("app/run", Some(b"binary".as_slice()), 0o755),
                ("app/link", None, 0o777),
            ]),
        )
        .unwrap();
        t.insert(
            "app/link",
            Node {
                kind: NodeKind::Symlink("run".into()),
                mode: 0o777,
                uid: 1000,
                gid: 1000,
                mtime: 7,
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rootfs.sqsh");
        let mut f = std::fs::File::create(&path).unwrap();
        t.write_squashfs(&[(CONFIG_PATH, b"{\"os\":\"linux\"}")], &mut f)
            .unwrap();
        drop(f);
        assert_eq!(
            read_file(&path, CONFIG_PATH).unwrap(),
            b"{\"os\":\"linux\"}"
        );
        assert_eq!(read_file(&path, "app/run").unwrap(), b"binary");
        assert!(
            read_file(&path, "nope")
                .unwrap_err()
                .0
                .contains("holds no /nope")
        );
        // Deterministic.
        let mut again = Vec::new();
        t.write_squashfs(
            &[(CONFIG_PATH, b"{\"os\":\"linux\"}")],
            &mut std::io::Cursor::new(&mut again),
        )
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), again);
    }

    #[test]
    fn paths_normalize() {
        assert_eq!(normalize("./etc//passwd/"), "etc/passwd");
        assert_eq!(normalize("/"), "");
        assert_eq!(normalize("."), "");
    }
}
