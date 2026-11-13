//! Fixtures for the engine's tests and the end-to-end suite: layers and
//! images assembled in memory, and a sandbox that runs nothing.

use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::Error;
use crate::protocol::Description;

use crate::layout::{OCI_MANIFEST_TYPE, Writer};
use crate::sandbox::{Outcome, Sandbox, VmSpec};

pub const CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";
pub const LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar";
pub const LAYER_GZIP_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// A tar of the entries given: (path, bytes or None for a directory, mode),
/// owned by root, except that a path under `home/` is owned by 1000.
pub fn tar_layer(entries: &[(&str, Option<&[u8]>, u32)]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (path, data, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(*mode);
        header.set_mtime(0);
        let (uid, gid) = if path.starts_with("home/") {
            (1000, 1000)
        } else {
            (0, 0)
        };
        header.set_uid(uid);
        header.set_gid(gid);
        match data {
            Some(d) => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(d.len() as u64);
                header.set_cksum();
                tar.append_data(&mut header, path, *d).expect("tar");
            }
            None => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_cksum();
                tar.append_data(&mut header, path, std::io::empty())
                    .expect("tar");
            }
        }
    }
    tar.into_inner().expect("tar")
}

/// A layer written header by header, so a test chooses every entry's
/// type, path and place in the archive: nothing is checked on the way in,
/// which is what a path with `..` or a size the data does not back needs.
/// Entries are owned by root, their paths at most 100 bytes.
#[derive(Default)]
pub struct Layer {
    bytes: Vec<u8>,
}

impl Layer {
    pub fn new() -> Layer {
        Layer::default()
    }

    fn entry(
        mut self,
        kind: tar::EntryType,
        path: &str,
        mode: u32,
        size: u64,
        data: &[u8],
        more: impl FnOnce(&mut tar::Header),
    ) -> Layer {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(size);
        more(&mut header);
        header.set_cksum();
        self.bytes.extend_from_slice(header.as_bytes());
        self.bytes.extend_from_slice(data);
        self.bytes.resize(self.bytes.len().next_multiple_of(512), 0);
        self
    }

    pub fn file(self, path: &str, data: &[u8], mode: u32) -> Layer {
        self.entry(
            tar::EntryType::Regular,
            path,
            mode,
            data.len() as u64,
            data,
            |_| {},
        )
    }

    /// A regular file whose header states `size`, whatever `data` holds.
    pub fn declaring(self, path: &str, size: u64, data: &[u8]) -> Layer {
        self.entry(tar::EntryType::Regular, path, 0o644, size, data, |_| {})
    }

    pub fn dir(self, path: &str, mode: u32) -> Layer {
        self.entry(tar::EntryType::Directory, path, mode, 0, b"", |_| {})
    }

    pub fn symlink(self, path: &str, target: &str) -> Layer {
        self.entry(tar::EntryType::Symlink, path, 0o777, 0, b"", |h| {
            h.as_old_mut().linkname[..target.len()].copy_from_slice(target.as_bytes());
        })
    }

    /// A hard link to `target`.
    pub fn link(self, path: &str, target: &str) -> Layer {
        self.entry(tar::EntryType::Link, path, 0o644, 0, b"", |h| {
            h.as_old_mut().linkname[..target.len()].copy_from_slice(target.as_bytes());
        })
    }

    pub fn char_device(self, path: &str, major: u32, minor: u32) -> Layer {
        self.entry(tar::EntryType::Char, path, 0o666, 0, b"", |h| {
            h.set_device_major(major).expect("major");
            h.set_device_minor(minor).expect("minor");
        })
    }

    pub fn block_device(self, path: &str, major: u32, minor: u32) -> Layer {
        self.entry(tar::EntryType::Block, path, 0o660, 0, b"", |h| {
            h.set_device_major(major).expect("major");
            h.set_device_minor(minor).expect("minor");
        })
    }

    pub fn fifo(self, path: &str) -> Layer {
        self.entry(tar::EntryType::Fifo, path, 0o644, 0, b"", |_| {})
    }

    /// The whiteout of `path`: `.wh.<name>` beside it.
    pub fn whiteout(self, path: &str) -> Layer {
        let at = match path.rsplit_once('/') {
            Some((dir, name)) => format!("{dir}/.wh.{name}"),
            None => format!(".wh.{path}"),
        };
        self.file(&at, b"", 0o644)
    }

    /// The opaque marker of `dir`.
    pub fn opaque(self, dir: &str) -> Layer {
        self.file(&format!("{dir}/.wh..wh..opq"), b"", 0o644)
    }

    /// The archive, with its end-of-archive blocks.
    pub fn finish(mut self) -> Vec<u8> {
        self.bytes.resize(self.bytes.len() + 1024, 0);
        self.bytes
    }
}

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(bytes).expect("gzip");
    enc.finish().expect("gzip")
}

/// Where fixture_image puts the program.
pub const FIXTURE_PATH: &str = "/fixture";

/// An image layout around one layer (gzip) holding `/etc/passwd` and
/// `/etc/group` with root and `app` (1000:1000), and `program` at
/// FIXTURE_PATH; the config for this host's platform with the entrypoint,
/// user and env given (an entrypoint elsewhere than FIXTURE_PATH is the
/// missing-command case).
pub fn fixture_image(program: &[u8], entrypoint: &[&str], user: &str, env: &[&str]) -> Vec<u8> {
    let at = FIXTURE_PATH.trim_start_matches('/').to_string();
    let layer = gzip(&tar_layer(&[
        ("etc/", None, 0o755),
        (
            "etc/passwd",
            Some(b"root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000:app:/home/app:/bin/sh\n"),
            0o644,
        ),
        ("etc/group", Some(b"root:x:0:\napp:x:1000:\n"), 0o644),
        ("home/", None, 0o755),
        ("home/app/", None, 0o755),
        ("tmp/", None, 0o1777),
        (&at, Some(program), 0o755),
    ]));
    let config = serde_json::json!({
        "architecture": crate::image::host_architecture(),
        "os": "linux",
        "config": {"Entrypoint": entrypoint, "User": user, "Env": env, "WorkingDir": "/home/app"},
        "rootfs": {"type": "layers", "diff_ids": []},
    });
    image_layout(
        &serde_json::to_vec(&config).expect("json"),
        &[(LAYER_GZIP_MEDIA_TYPE, &layer)],
    )
    .expect("layout")
}

/// An image layout from a config and its layers.
pub fn image_layout(config: &[u8], layers: &[(&str, &[u8])]) -> Result<Vec<u8>, Error> {
    let mut w = Writer::new();
    let cd = w.blob(config);
    let mut descriptors = Vec::new();
    for (media_type, bytes) in layers {
        let d = w.blob(bytes);
        descriptors
            .push(serde_json::json!({"mediaType": media_type, "digest": d, "size": bytes.len()}));
    }
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST_TYPE,
        "config": {"mediaType": CONFIG_MEDIA_TYPE, "digest": cd, "size": config.len()},
        "layers": descriptors,
    }))
    .map_err(|e| Error(e.to_string()))?;
    w.manifest(&manifest, OCI_MANIFEST_TYPE);
    w.finish()
}

/// A sandbox that records every run's description and answers with what
/// the closure says.
pub struct FakeSandbox {
    seen: Arc<Mutex<Vec<Description>>>,
    answer: Box<dyn Fn(&VmSpec<'_>) -> Outcome + Send + Sync>,
    prepared: Arc<Mutex<Vec<u64>>>,
}

impl FakeSandbox {
    pub fn new(
        seen: Arc<Mutex<Vec<Description>>>,
        answer: impl Fn(&VmSpec<'_>) -> Outcome + Send + Sync + 'static,
    ) -> FakeSandbox {
        FakeSandbox {
            seen,
            answer: Box::new(answer),
            prepared: Arc::default(),
        }
    }

    /// The memory in MiB of every VM the sandbox was asked to prepare.
    pub fn prepared(&self) -> Arc<Mutex<Vec<u64>>> {
        Arc::clone(&self.prepared)
    }
}

impl Sandbox for FakeSandbox {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn run(&self, spec: VmSpec<'_>) -> Result<Outcome, Error> {
        self.seen
            .lock()
            .expect("poisoned")
            .push(spec.description.clone());
        Ok((self.answer)(&spec))
    }

    fn prepare(&self, spec: VmSpec<'_>) -> Result<(), Error> {
        self.prepared
            .lock()
            .expect("poisoned")
            .push(spec.memory_mib);
        Ok(())
    }
}
