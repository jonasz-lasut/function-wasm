//! The OCI image layout as the module's bytes: what the runtime's resolver
//! assembles from a registry (or a `module.path` names, as `docker buildx
//! build -o type=oci,dest=image.tar` writes it) and what `compile` and
//! `inspect` read. A tar with `oci-layout`, `index.json` and the blobs by
//! digest; the index names one manifest, or an index of manifests the
//! engine resolves to this host's platform. Every blob is verified against
//! the digest that names it, so a tampered layout is refused before any of
//! it is unpacked.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use crate::Error;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

pub const OCI_MANIFEST_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
pub const OCI_INDEX_TYPE: &str = "application/vnd.oci.image.index.v1+json";
pub const DOCKER_MANIFEST_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";
pub const DOCKER_LIST_TYPE: &str = "application/vnd.docker.distribution.manifest.list.v2+json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Descriptor {
    #[serde(default, rename = "mediaType")]
    pub media_type: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Platform {
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub os: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default, rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(
        default,
        rename = "mediaType",
        skip_serializing_if = "String::is_empty"
    )]
    pub media_type: String,
    #[serde(default)]
    pub config: Descriptor,
    #[serde(default)]
    pub layers: Vec<Descriptor>,
    #[serde(default)]
    pub manifests: Vec<Descriptor>,
}

/// One layer's descriptor and bytes, as the layout holds them.
#[derive(Debug)]
pub struct Layer {
    pub descriptor: Descriptor,
    pub bytes: Vec<u8>,
}

/// A parsed, verified layout: the image's manifest for this platform, its
/// config and its layers.
#[derive(Debug)]
pub struct Layout {
    pub manifest_digest: String,
    pub manifest: Manifest,
    pub config: Vec<u8>,
    pub layers: Vec<Layer>,
}

pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes)))
}

/// Reads a layout tar for this host's platform.
pub fn parse(bytes: &[u8]) -> Result<Layout, Error> {
    parse_for(bytes, crate::oci::image::host_architecture(), "linux")
}

pub fn parse_for(bytes: &[u8], architecture: &str, os: &str) -> Result<Layout, Error> {
    let files = read_tar(bytes)?;
    let index = files
        .get("index.json")
        .ok_or_else(|| Error("image layout has no index.json".to_string()))?;
    let index: Manifest = serde_json::from_slice(index)
        .map_err(|e| Error(format!("cannot parse the image layout's index.json: {e}")))?;
    let first = index
        .manifests
        .first()
        .ok_or_else(|| Error("image layout's index names no manifest".to_string()))?;
    let (manifest_digest, manifest) = resolve(&files, first, architecture, os)?;
    let config = blob(&files, &manifest.config, "image config")?;
    let mut layers = Vec::with_capacity(manifest.layers.len());
    for (i, d) in manifest.layers.iter().enumerate() {
        let bytes = blob(&files, d, &format!("layer {i}"))?;
        layers.push(Layer {
            descriptor: d.clone(),
            bytes,
        });
    }
    Ok(Layout {
        manifest_digest,
        manifest,
        config,
        layers,
    })
}

/// Follows an index to the platform's manifest; a manifest is taken as it
/// is.
fn resolve(
    files: &BTreeMap<String, Vec<u8>>,
    d: &Descriptor,
    architecture: &str,
    os: &str,
) -> Result<(String, Manifest), Error> {
    let raw = blob(files, d, "image manifest")?;
    let m: Manifest = serde_json::from_slice(&raw)
        .map_err(|e| Error(format!("cannot parse image manifest {}: {e}", d.digest)))?;
    let is_index = m.media_type == OCI_INDEX_TYPE
        || m.media_type == DOCKER_LIST_TYPE
        || (m.media_type.is_empty() && !m.manifests.is_empty());
    if !is_index {
        return Ok((d.digest.clone(), m));
    }
    let entry = m
        .manifests
        .iter()
        .find(|e| {
            e.platform
                .as_ref()
                .is_some_and(|p| p.architecture == architecture && p.os == os)
        })
        .ok_or_else(|| {
            let have: Vec<String> = m
                .manifests
                .iter()
                .filter_map(|e| e.platform.as_ref())
                .map(|p| format!("{}/{}", p.os, p.architecture))
                .collect();
            Error(format!(
                "image index {} has no manifest for {os}/{architecture} (it has {})",
                d.digest,
                if have.is_empty() {
                    "none with a platform".to_string()
                } else {
                    have.join(", ")
                }
            ))
        })?;
    let raw = blob(files, entry, "image manifest")?;
    let m: Manifest = serde_json::from_slice(&raw)
        .map_err(|e| Error(format!("cannot parse image manifest {}: {e}", entry.digest)))?;
    Ok((entry.digest.clone(), m))
}

fn blob(files: &BTreeMap<String, Vec<u8>>, d: &Descriptor, what: &str) -> Result<Vec<u8>, Error> {
    let Some(hex) = d.digest.strip_prefix("sha256:") else {
        return Err(Error(format!(
            "{what} digest {:?} is not a sha256 digest",
            d.digest
        )));
    };
    let path = format!("blobs/sha256/{hex}");
    let bytes = files
        .get(&path)
        .ok_or_else(|| Error(format!("image layout has no blob for {what} {}", d.digest)))?;
    let got = digest_of(bytes);
    if got != d.digest {
        return Err(Error(format!(
            "{what} content is {got}, the layout names {}",
            d.digest
        )));
    }
    Ok(bytes.clone())
}

fn read_tar(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, Error> {
    let mut files = BTreeMap::new();
    let mut archive = tar::Archive::new(bytes);
    let entries = archive
        .entries()
        .map_err(|e| Error(format!("cannot read the image layout: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| Error(format!("cannot read the image layout: {e}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = name.trim_start_matches("./").to_string();
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut data)
            .map_err(|e| Error(format!("cannot read the image layout's {name}: {e}")))?;
        files.insert(name, data);
    }
    Ok(files)
}

/// Assembles a layout tar: blobs by digest under blobs/sha256, the
/// manifest among them, index.json naming it, and oci-layout. The order
/// and the headers are fixed, so the same image gives the same bytes.
pub struct Writer {
    blobs: BTreeMap<String, Vec<u8>>,
    manifest: Option<Descriptor>,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    pub fn new() -> Writer {
        Writer {
            blobs: BTreeMap::new(),
            manifest: None,
        }
    }

    /// Adds a blob; returns its digest.
    pub fn blob(&mut self, bytes: &[u8]) -> String {
        let digest = digest_of(bytes);
        self.blobs.insert(digest.clone(), bytes.to_vec());
        digest
    }

    /// Adds the manifest the index names, with its media type.
    pub fn manifest(&mut self, bytes: &[u8], media_type: &str) -> String {
        let digest = self.blob(bytes);
        self.manifest = Some(Descriptor {
            media_type: media_type.to_string(),
            digest: digest.clone(),
            size: bytes.len() as u64,
            platform: None,
        });
        digest
    }

    pub fn finish(self) -> Result<Vec<u8>, Error> {
        let manifest = self
            .manifest
            .ok_or_else(|| Error("image layout has no manifest".to_string()))?;
        let index = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_INDEX_TYPE,
            "manifests": [manifest],
        });
        let index = serde_json::to_vec(&index).map_err(|e| Error(e.to_string()))?;
        let mut tar = tar::Builder::new(Vec::new());
        let mut put = |name: &str, data: &[u8]| -> Result<(), Error> {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            tar.append_data(&mut header, name, data)
                .map_err(|e| Error(format!("cannot write the image layout's {name}: {e}")))
        };
        put("oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#)?;
        put("index.json", &index)?;
        for (digest, bytes) in &self.blobs {
            let hex = digest.trim_start_matches("sha256:");
            put(&format!("blobs/sha256/{hex}"), bytes)?;
        }
        let mut out = tar
            .into_inner()
            .map_err(|e| Error(format!("cannot finish the image layout: {e}")))?;
        out.flush().map_err(|e| Error(e.to_string()))?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_a_layout_with_an_index() {
        let config = br#"{"architecture":"amd64","os":"linux","config":{"Cmd":["/bin/true"]}}"#;
        let layer = b"not really a tar";
        let mut w = Writer::new();
        let cd = w.blob(config);
        let ld = w.blob(layer);
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2, "mediaType": OCI_MANIFEST_TYPE,
            "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": cd, "size": config.len()},
            "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar", "digest": ld, "size": layer.len()}],
        }))
        .unwrap();
        let md = w.blob(&manifest);
        let other = w.blob(b"an arm64 manifest that is not read");
        let index = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2, "mediaType": OCI_INDEX_TYPE,
            "manifests": [
                {"mediaType": OCI_MANIFEST_TYPE, "digest": other, "size": 34, "platform": {"architecture": "arm64", "os": "linux"}},
                {"mediaType": OCI_MANIFEST_TYPE, "digest": md, "size": manifest.len(), "platform": {"architecture": "amd64", "os": "linux"}},
            ],
        }))
        .unwrap();
        w.manifest(&index, OCI_INDEX_TYPE);
        let bytes = w.finish().unwrap();
        let layout = parse_for(&bytes, "amd64", "linux").unwrap();
        assert_eq!(layout.manifest_digest, md);
        assert_eq!(layout.config, config);
        assert_eq!(layout.layers.len(), 1);
        assert_eq!(layout.layers[0].bytes, layer);
        let err = parse_for(&bytes, "s390x", "linux").unwrap_err().0;
        assert!(
            err.contains("has no manifest for linux/s390x (it has linux/arm64, linux/amd64)"),
            "{err}"
        );
        // The same input gives the same bytes.
        let mut again = Writer::new();
        again.blob(config);
        again.blob(layer);
        again.blob(&manifest);
        again.blob(b"an arm64 manifest that is not read");
        again.manifest(&index, OCI_INDEX_TYPE);
        assert_eq!(again.finish().unwrap(), bytes);
    }

    #[test]
    fn a_tampered_blob_is_refused() {
        let mut w = Writer::new();
        let cd = w.blob(b"{}");
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2, "mediaType": OCI_MANIFEST_TYPE,
            "config": {"mediaType": "application/vnd.oci.image.config.v1+json", "digest": cd, "size": 2},
            "layers": [],
        }))
        .unwrap();
        w.manifest(&manifest, OCI_MANIFEST_TYPE);
        let mut bytes = w.finish().unwrap();
        let at = bytes.windows(2).position(|x| x == b"{}").unwrap();
        bytes[at] = b'[';
        bytes[at + 1] = b']';
        let err = parse_for(&bytes, "amd64", "linux").unwrap_err().0;
        assert!(err.starts_with("image config content is sha256:"), "{err}");
        assert!(parse_for(b"not a tar", "amd64", "linux").is_err());
    }
}
