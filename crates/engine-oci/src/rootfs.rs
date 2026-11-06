//! The image's root filesystem: its layers applied in order, whiteouts
//! and all, into a tree held in memory, then written as a squashfs the VM
//! mounts read-only under its tmpfs overlay. Ownership and modes come from
//! the layers' tar headers, so no root is needed to keep them, and the
//! engine's own config goes in beside the image's files, under a path no
//! image uses, so a stored root filesystem describes itself.

use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};
use std::path::Path;

use function_wasm_engine_api::Error;

/// Where the engine keeps the image config inside the root filesystem.
pub const CONFIG_PATH: &str = ".guestfn/config.json";

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
    File(Vec<u8>),
    Dir,
    Symlink(String),
    Char(u32, u32),
    Block(u32, u32),
    Fifo,
}

#[derive(Debug, Default)]
pub struct Tree {
    nodes: BTreeMap<String, Node>,
}

/// A layer's media type says how it is packed; a gzip magic says so too,
/// whatever the type.
pub fn is_gzip(media_type: &str, bytes: &[u8]) -> bool {
    media_type.ends_with("+gzip")
        || media_type.ends_with(".gzip")
        || bytes.starts_with(&[0x1f, 0x8b])
}

impl Tree {
    pub fn new() -> Tree {
        Tree::default()
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
    /// directory of what the layers below put there. A hard link copies
    /// its target's bytes.
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
        let mut archive = tar::Archive::new(reader);
        let entries = archive
            .entries()
            .map_err(|e| Error(format!("cannot read layer: {e}")))?;
        for entry in entries {
            let mut entry = entry.map_err(|e| Error(format!("cannot read layer: {e}")))?;
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
                    let mut data = Vec::with_capacity(entry.size() as usize);
                    entry
                        .read_to_end(&mut data)
                        .map_err(|e| Error(format!("cannot read layer entry {path}: {e}")))?;
                    NodeKind::File(data)
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
                    .push_file(std::io::Cursor::new(data.clone()), &at, header)
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
                std::io::Cursor::new(data.to_vec()),
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
    use crate::testing::tar_layer;

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
