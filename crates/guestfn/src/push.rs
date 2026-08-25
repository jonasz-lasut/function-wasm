//! guestfn push: publish a module as an OCI artifact - the module layer
//! and, when the project has a wasmfn.yaml, the manifest layer beside it -
//! and print what a Composition needs: the reference pinned to the OCI
//! manifest digest, and the requires block the module's manifest declares.

use std::path::PathBuf;

use function_wasm::location::{AnyReference, parse_any_reference};
use function_wasm::manifest::Manifest;
use function_wasm::oci::{self, RegistryClient};
use serde::Serialize;
use sha2::Digest as _;

/// The CNCF wasm OCI artifact layout: a wasm config and one raw wasm layer.
const WASM_CONFIG_MEDIA_TYPE: &str = "application/vnd.wasm.config.v0+json";
const WASM_LAYER_MEDIA_TYPE: &str = "application/wasm";
const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

/// OCI annotation keys mirrored from the manifest onto the artifact.
const ANNOTATION_TITLE: &str = "org.opencontainers.image.title";
const ANNOTATION_VERSION: &str = "org.opencontainers.image.version";
const ANNOTATION_SOURCE: &str = "org.opencontainers.image.source";
const ANNOTATION_DESCRIPTION: &str = "org.opencontainers.image.description";
const ANNOTATION_REVISION: &str = "org.opencontainers.image.revision";

#[derive(clap::Args, Debug)]
pub struct PushCmd {
    /// Reference to push to, e.g. ghcr.io/me/my-fn:v0.1.0.
    r#ref: String,

    /// Module to push.
    #[arg(short, long, default_value = "fn.wasm")]
    file: PathBuf,

    /// The module manifest to publish beside the module as the artifact's
    /// second layer; by default the wasmfn.yaml next to the module file,
    /// when there is one.
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Module version to record in the published manifest, overriding the
    /// file's version.
    #[arg(long)]
    module_version: Option<String>,

    /// Source revision to record as org.opencontainers.image.revision on
    /// the artifact.
    #[arg(long)]
    revision: Option<String>,
}

impl PushCmd {
    pub fn run(&self) -> Result<(), String> {
        let wasm = std::fs::read(&self.file)
            .map_err(|e| format!("cannot read {}: {e}", self.file.display()))?;
        // A module the runtime would refuse at load is not published:
        // pushing it only moves the refusal into an XR condition. Modules
        // for other hosts are oras push's business.
        crate::check_module(&wasm).map_err(|e| {
            format!(
                "{} would be refused by the runtime and is not pushed: {e} (guestfn push publishes function-wasm modules; use oras push for other artifacts)",
                self.file.display()
            )
        })?;
        let m = self.load_manifest()?;
        let reference = parse_any_reference(&self.r#ref)?;
        let manifest_json = match &m {
            Some(m) => Some(m.json()?),
            None => None,
        };
        // A fixed creation time makes the artifact - and so the manifest
        // digest a Composition pins and the caches key on - a function of
        // the module bytes, the manifest and the annotations alone: pushing
        // the same fn.wasm twice yields the same reference.
        // SOURCE_DATE_EPOCH overrides it, as for any reproducible build.
        let (raw_manifest, digest, config) = artifact(
            &wasm,
            creation_time(),
            manifest_json.as_deref(),
            annotations(m.as_ref(), self.revision.as_deref()),
        )?;
        push(
            &reference,
            &wasm,
            manifest_json.as_deref(),
            &raw_manifest,
            &config,
        )?;
        let pinned = reference.pinned(&digest);
        println!("Pushed {pinned}\n\nmodule:\n  type: OCI\n  oci:\n    ref: {pinned}");
        if let Some(m) = &m
            && let Some(block) = requires_block(m)?
        {
            print!("{block}");
        }
        Ok(())
    }

    /// The manifest to publish: --manifest, or the wasmfn.yaml next to the
    /// module file when there is one; None without either. The version
    /// override applies before it is published.
    fn load_manifest(&self) -> Result<Option<Manifest>, String> {
        let path = match &self.manifest {
            Some(p) => p.clone(),
            None => {
                let candidate = self
                    .file
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join(function_wasm::manifest::FILE_NAME);
                if !candidate.is_file() {
                    if self.module_version.is_some() {
                        return Err(format!(
                            "--module-version needs a manifest: no {} next to {} and no --manifest",
                            function_wasm::manifest::FILE_NAME,
                            self.file.display()
                        ));
                    }
                    return Ok(None);
                }
                candidate
            }
        };
        let mut m = Manifest::load(&path)?;
        if let Some(v) = &self.module_version {
            m.version = v.clone();
        }
        Ok(Some(m))
    }
}

/// Uploads the artifact's blobs and manifest through the runtime's own
/// registry client, authenticated with the local Docker config.
fn push(
    reference: &AnyReference,
    wasm: &[u8],
    manifest_json: Option<&[u8]>,
    raw_manifest: &[u8],
    config: &[u8],
) -> Result<(), String> {
    let pinned = function_wasm::location::OciReference {
        registry: reference.registry.clone(),
        repository: reference.repository.clone(),
        digest: digest_of(raw_manifest),
    };
    let auth = oci::keychain_auth(&reference.registry);
    let client = RegistryClient::new(&pinned, auth);
    client.push_blob(&digest_of(config), config)?;
    client.push_blob(&digest_of(wasm), wasm)?;
    if let Some(mj) = manifest_json {
        client.push_blob(&digest_of(mj), mj)?;
    }
    let target = reference.manifest_ref();
    client
        .push_manifest(&target, OCI_MANIFEST_MEDIA_TYPE, raw_manifest)
        .map_err(|e| {
            format!(
                "cannot push {}: {e}",
                reference.pinned(&digest_of(raw_manifest))
            )
        })
}

/// The artifact's config as the CNCF wasm OCI artifact specification lists
/// it: created, architecture, os and the digests of the layers.
#[derive(Serialize)]
struct WasmConfig {
    created: String,
    architecture: String,
    os: String,
    #[serde(rename = "layerDigests")]
    layer_digests: Vec<String>,
}

#[derive(Serialize)]
struct DescriptorOut {
    #[serde(rename = "mediaType")]
    media_type: String,
    size: usize,
    digest: String,
}

#[derive(Serialize)]
struct ManifestOut {
    #[serde(rename = "schemaVersion")]
    schema_version: i64,
    #[serde(rename = "mediaType")]
    media_type: String,
    config: DescriptorOut,
    layers: Vec<DescriptorOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<std::collections::BTreeMap<String, String>>,
}

/// Wraps a module in the CNCF wasm OCI artifact layout: the
/// application/wasm layer, then - when manifest_json is set - the module
/// manifest as a second layer, a wasm config naming every layer in
/// layerDigests, and the OCI annotations. Returns the raw OCI manifest and
/// its digest - a function of the module bytes, the manifest, created and
/// annotations alone.
fn artifact(
    wasm: &[u8],
    created: std::time::SystemTime,
    manifest_json: Option<&[u8]>,
    annotations: Option<std::collections::BTreeMap<String, String>>,
) -> Result<(Vec<u8>, String, Vec<u8>), String> {
    let mut layers = vec![(WASM_LAYER_MEDIA_TYPE.to_string(), wasm.to_vec())];
    if let Some(mj) = manifest_json {
        layers.push((oci::MANIFEST_LAYER_TYPE.to_string(), mj.to_vec()));
    }
    let descriptors: Vec<DescriptorOut> = layers
        .iter()
        .map(|(mt, b)| DescriptorOut {
            media_type: mt.clone(),
            size: b.len(),
            digest: digest_of(b),
        })
        .collect();
    let config = serde_json::to_vec(&WasmConfig {
        created: rfc3339(created),
        architecture: "wasm".to_string(),
        os: "wasip1".to_string(),
        layer_digests: descriptors.iter().map(|d| d.digest.clone()).collect(),
    })
    .map_err(|e| e.to_string())?;
    let manifest = ManifestOut {
        schema_version: 2,
        media_type: OCI_MANIFEST_MEDIA_TYPE.to_string(),
        config: DescriptorOut {
            media_type: WASM_CONFIG_MEDIA_TYPE.to_string(),
            size: config.len(),
            digest: digest_of(&config),
        },
        layers: descriptors,
        annotations,
    };
    let raw = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    let digest = digest_of(&raw);
    Ok((raw, digest, config))
}

/// Maps the manifest onto the standard OCI image annotations of the
/// artifact; revision comes from the flag. None when there is nothing to
/// record. The manifest itself is a layer, not an annotation.
fn annotations(
    m: Option<&Manifest>,
    revision: Option<&str>,
) -> Option<std::collections::BTreeMap<String, String>> {
    let mut out = std::collections::BTreeMap::new();
    if let Some(m) = m {
        for (key, value) in [
            (ANNOTATION_TITLE, &m.name),
            (ANNOTATION_VERSION, &m.version),
            (ANNOTATION_SOURCE, &m.source),
            (ANNOTATION_DESCRIPTION, &m.description),
        ] {
            if !value.is_empty() {
                out.insert(key.to_string(), value.clone());
            }
        }
    }
    if let Some(r) = revision
        && !r.is_empty()
    {
        out.insert(ANNOTATION_REVISION.to_string(), r.to_string());
    }
    (!out.is_empty()).then_some(out)
}

/// Renders the module's requirements as YAML - informational: the runtime
/// grants them where the operator's --sandbox-policy-file (and any
/// compositionPolicy) permits, nothing is copied into a Composition. None
/// when the module requires nothing.
fn requires_block(m: &Manifest) -> Result<Option<String>, String> {
    let Some(r) = &m.requires else {
        return Ok(None);
    };
    let empty = r.egress.as_ref().is_none_or(|e| e.http.is_empty())
        && r.filesystem.as_ref().is_none_or(|f| !f.private_tmp)
        && r.env.is_empty();
    if empty {
        return Ok(None);
    }
    #[derive(Serialize)]
    struct Block<'a> {
        requires: &'a function_wasm::manifest::Requires,
    }
    serde_yaml::to_string(&Block { requires: r })
        .map(Some)
        .map_err(|e| e.to_string())
}

/// SOURCE_DATE_EPOCH when set and valid, the Unix epoch otherwise.
fn creation_time() -> std::time::SystemTime {
    if let Ok(v) = std::env::var("SOURCE_DATE_EPOCH")
        && let Ok(secs) = v.parse::<u64>()
    {
        return std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    }
    std::time::UNIX_EPOCH
}

fn rfc3339(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Days-to-date without a chrono dependency: the artifact's created
    // stamp is the epoch (or SOURCE_DATE_EPOCH), whole seconds only.
    let days = secs / 86_400;
    let (mut year, mut remaining) = (1970u64, days);
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let len = if leap { 366 } else { 365 };
        if remaining < len {
            break;
        }
        remaining -= len;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while remaining >= months[month] {
        remaining -= months[month];
        month += 1;
    }
    let (h, m_, s_) = (secs % 86_400 / 3_600, secs % 3_600 / 60, secs % 60);
    format!(
        "{year:04}-{:02}-{:02}T{h:02}:{m_:02}:{s_:02}Z",
        month + 1,
        remaining + 1
    )
}

pub(crate) fn digest_of(b: &[u8]) -> String {
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_artifact_is_reproducible() {
        let (a, da, _) = artifact(b"wasm", std::time::UNIX_EPOCH, Some(b"{}"), None).expect("a");
        let (b, db, _) = artifact(b"wasm", std::time::UNIX_EPOCH, Some(b"{}"), None).expect("b");
        assert_eq!(a, b);
        assert_eq!(da, db);
        let v: serde_json::Value = serde_json::from_slice(&a).expect("json");
        assert_eq!(v["schemaVersion"], 2);
        assert_eq!(v["layers"][0]["mediaType"], "application/wasm");
        assert_eq!(v["layers"][1]["mediaType"], oci::MANIFEST_LAYER_TYPE);
        assert_eq!(v["config"]["mediaType"], WASM_CONFIG_MEDIA_TYPE);
    }

    #[test]
    fn rfc3339_renders_the_epoch() {
        assert_eq!(rfc3339(std::time::UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_756_166_400);
        assert_eq!(rfc3339(t), "2025-08-26T00:00:00Z");
    }
}
