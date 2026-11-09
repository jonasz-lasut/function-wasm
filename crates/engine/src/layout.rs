//! The writing half of the OCI image layout, the module's bytes under the
//! OCI runtime: a tar with `oci-layout`, `index.json` and the blobs by
//! digest. The runtime's resolver assembles one from a registry and the
//! OCI engine's fixtures write one, so it lives here, where both reach it;
//! the reading half is the OCI engine's own.

use std::collections::BTreeMap;
use std::io::Write;

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

pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(bytes)))
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
