//! guestfn push: publish a module as an OCI artifact (the module layer
//! and, when the project has a guestfn.yaml, the manifest layer beside it;
//! for an `oci@0.1` manifest, the image named by --image with the manifest
//! appended to it as one more layer, docs/one-pager-oci-runtime.md) and
//! print what a Composition needs: the reference pinned to the OCI manifest
//! digest, and the requires block the module's manifest declares.

use std::path::PathBuf;

use guestfn_runtime::location::{AnyReference, parse_any_reference, parse_oci_reference};
use guestfn_runtime::manifest::{Manifest, TYPE_OCI};
use guestfn_runtime::oci::{self, RegistryClient};
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
    /// second layer; by default the guestfn.yaml next to the module file,
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

    /// For an oci@0.1 manifest: the image the module is built from, pinned
    /// by digest (registry/repository@sha256:...). guestfn push appends the
    /// manifest to it as one more layer and pushes the result into the
    /// image's repository under the reference given; no module file is
    /// read, and the image's digest goes into the published manifest's
    /// image field.
    #[arg(long)]
    image: Option<String>,
}

impl PushCmd {
    pub fn run(&self) -> Result<(), String> {
        if self.image.is_some() || self.manifest_is_oci()? {
            return self.push_module_image();
        }
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
        let mut blobs: Vec<&[u8]> = vec![&config, &wasm];
        if let Some(mj) = &manifest_json {
            blobs.push(mj);
        }
        push(&reference, &blobs, &raw_manifest)?;
        let pinned = reference.pinned(&digest);
        println!("Pushed {pinned}\n\nmodule:\n  type: OCI\n  oci:\n    ref: {pinned}");
        if let Some(m) = &m
            && let Some(block) = requires_block(m)?
        {
            print!("{block}");
        }
        Ok(())
    }

    /// Whether the project's manifest (--manifest, or the guestfn.yaml next
    /// to the module file) declares an oci@0.1 module.
    fn manifest_is_oci(&self) -> Result<bool, String> {
        Ok(self.load_manifest()?.is_some_and(|m| m.kind() == TYPE_OCI))
    }

    /// Publishes an oci@0.1 module: the image named by --image (or the
    /// manifest's image field) with the manifest appended as one more
    /// layer (docs/one-pager-oci-runtime.md, "The artifact"): a gzip tar
    /// holding /.guestfn/manifest.json, its diff_id in the image config
    /// like any other layer's, its descriptor annotated so the runtime
    /// reads it before the rest. The image keeps running under every
    /// container runtime; the Input pins the result and one signature
    /// covers image and manifest. An index gets the layer in each of its
    /// platform manifests. The result goes into the image's repository,
    /// where its layers already are, under the reference given.
    fn push_module_image(&self) -> Result<(), String> {
        let Some(mut m) = self.load_manifest()? else {
            return Err(format!(
                "--image needs an oci@0.1 manifest: no {} next to {} and no --manifest",
                guestfn_runtime::manifest::FILE_NAME,
                self.file.display()
            ));
        };
        if m.kind() != TYPE_OCI {
            return Err(
                "--image is for an oci@0.1 manifest; this manifest describes a WebAssembly module, which guestfn push publishes from its file".to_string(),
            );
        }
        if let Some(image) = &self.image {
            m.oci.get_or_insert_with(Default::default).image = image.clone();
        }
        if m.image().is_empty() {
            return Err(format!(
                "an oci@0.1 manifest names the image it is built from: set oci.image in {} or pass --image registry/repository@sha256:...",
                guestfn_runtime::manifest::FILE_NAME
            ));
        }
        // The pin is checked here, where the author can fix it, with the
        // runtime's own rule.
        m.validate()?;
        let image =
            parse_oci_reference(m.image()).map_err(|e| format!("image {:?}: {e}", m.image()))?;
        let target = parse_any_reference(&self.r#ref)?;
        if target.registry != image.registry || target.repository != image.repository {
            return Err(format!(
                "{} is not in the image's repository {}/{}: the module is the image with its manifest layer appended, and is pushed where the image's layers already are",
                self.r#ref, image.registry, image.repository
            ));
        }
        let client = RegistryClient::new(&image, oci::keychain_auth(&image.registry));
        let created = creation_time();
        let layer = ManifestLayer::new(&m.json()?, created);
        let (top_raw, got) = client.raw_manifest(&image.digest)?;
        if got != image.digest {
            return Err(format!(
                "image manifest content is {got}, want {}",
                image.digest
            ));
        }
        let mut top: serde_json::Value = serde_json::from_slice(&top_raw)
            .map_err(|e| format!("cannot parse the image manifest: {e}"))?;
        let (raw, media_type) =
            if let Some(entries) = top.get_mut("manifests").and_then(|v| v.as_array_mut()) {
                // An index: every platform manifest gets the layer, pushed by
                // digest, and a new index names the results.
                for entry in entries.iter_mut() {
                    let digest = entry["digest"]
                        .as_str()
                        .ok_or("the image index has an entry without a digest")?
                        .to_string();
                    let (entry_raw, entry_got) = client.raw_manifest(&digest)?;
                    if entry_got != digest {
                        return Err(format!(
                            "image manifest content is {entry_got}, want {digest}"
                        ));
                    }
                    let (new_raw, entry_type) = append_layer(&client, &entry_raw, &layer, created)?;
                    let new_digest = digest_of(&new_raw);
                    client.push_manifest(&new_digest, &entry_type, &new_raw)?;
                    entry["digest"] = serde_json::Value::String(new_digest);
                    entry["size"] = serde_json::Value::from(new_raw.len());
                }
                let media_type = top["mediaType"]
                    .as_str()
                    .unwrap_or(oci::OCI_INDEX_TYPE)
                    .to_string();
                (
                    serde_json::to_vec(&top).map_err(|e| e.to_string())?,
                    media_type,
                )
            } else {
                append_layer(&client, &top_raw, &layer, created)?
            };
        let digest = digest_of(&raw);
        client
            .push_manifest(&target.manifest_ref(), &media_type, &raw)
            .map_err(|e| format!("cannot push {}: {e}", target.pinned(&digest)))?;
        let pinned = target.pinned(&digest);
        println!(
            "Pushed {pinned} (the image {} with its manifest layer)\n\nmodule:\n  type: OCI\n  oci:\n    ref: {pinned}",
            m.image()
        );
        if let Some(block) = requires_block(&m)? {
            print!("{block}");
        }
        Ok(())
    }

    /// The manifest to publish: --manifest, or the guestfn.yaml next to the
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
                    .join(guestfn_runtime::manifest::FILE_NAME);
                if !candidate.is_file() {
                    if self.module_version.is_some() {
                        return Err(format!(
                            "--module-version needs a manifest: no {} next to {} and no --manifest",
                            guestfn_runtime::manifest::FILE_NAME,
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

/// Uploads the artifact's blobs (its config and layers) and manifest
/// through the runtime's own registry client, authenticated with the local
/// Docker config.
fn push(reference: &AnyReference, blobs: &[&[u8]], raw_manifest: &[u8]) -> Result<(), String> {
    let pinned = guestfn_runtime::location::OciReference {
        registry: reference.registry.clone(),
        repository: reference.repository.clone(),
        digest: digest_of(raw_manifest),
    };
    let auth = oci::keychain_auth(&reference.registry);
    let client = RegistryClient::new(&pinned, auth);
    for blob in blobs {
        client.push_blob(&digest_of(blob), blob)?;
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
    #[serde(rename = "artifactType", skip_serializing_if = "Option::is_none")]
    artifact_type: Option<String>,
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
        artifact_type: None,
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

/// The layer `guestfn push --image` appends to an image: a gzip tar with
/// the module manifest at /.guestfn/manifest.json, owned by root, read
/// only, stamped with the creation time, so the same manifest and time
/// give the same bytes. diff_id is the uncompressed tar's digest, what the
/// image config lists.
struct ManifestLayer {
    compressed: Vec<u8>,
    diff_id: String,
}

impl ManifestLayer {
    fn new(manifest_json: &[u8], created: std::time::SystemTime) -> ManifestLayer {
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o444);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(
            created
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        header.set_size(manifest_json.len() as u64);
        header.set_cksum();
        tar.append_data(
            &mut header,
            oci::MANIFEST_LAYER_PATH.trim_start_matches('/'),
            manifest_json,
        )
        .expect("a tar in memory");
        let plain = tar.into_inner().expect("a tar in memory");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &plain).expect("gzip in memory");
        ManifestLayer {
            compressed: gz.finish().expect("gzip in memory"),
            diff_id: digest_of(&plain),
        }
    }

    fn digest(&self) -> String {
        digest_of(&self.compressed)
    }
}

const OCI_LAYER_GZIP_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const DOCKER_LAYER_GZIP_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// Appends the manifest layer to one image manifest in the registry: the
/// config comes down, gains the layer's diff_id and a history entry and
/// goes back up under its new digest with the layer blob, and the new
/// manifest (its bytes and media type) is returned for the caller to push
/// under the reference it chooses.
fn append_layer(
    client: &RegistryClient,
    raw_manifest: &[u8],
    layer: &ManifestLayer,
    created: std::time::SystemTime,
) -> Result<(Vec<u8>, String), String> {
    let manifest: serde_json::Value = serde_json::from_slice(raw_manifest)
        .map_err(|e| format!("cannot parse the image manifest: {e}"))?;
    let config_type = manifest["config"]["mediaType"].as_str().unwrap_or_default();
    if !oci::IMAGE_CONFIG_TYPES.contains(&config_type) {
        return Err(format!(
            "the manifest's config is {config_type:?}, not an image config: --image names an OCI image, which a WebAssembly artifact is not"
        ));
    }
    let config_digest = manifest["config"]["digest"]
        .as_str()
        .ok_or("the image manifest's config has no digest")?;
    let raw_config = client.blob(config_digest, 16 << 20, "image config")?;
    let (new_manifest, new_config) = with_manifest_layer(&manifest, &raw_config, layer, created)?;
    client.push_blob(&digest_of(&new_config), &new_config)?;
    client.push_blob(&layer.digest(), &layer.compressed)?;
    let media_type = manifest["mediaType"]
        .as_str()
        .unwrap_or(OCI_MANIFEST_MEDIA_TYPE)
        .to_string();
    Ok((new_manifest, media_type))
}

/// The image manifest and config with the manifest layer appended: one
/// more diff_id and history entry in the config, one more layer descriptor
/// (annotated, in the manifest's own media type family) and the new config
/// descriptor in the manifest. Pure: what append_layer pushes.
fn with_manifest_layer(
    manifest: &serde_json::Value,
    raw_config: &[u8],
    layer: &ManifestLayer,
    created: std::time::SystemTime,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut config: serde_json::Value = serde_json::from_slice(raw_config)
        .map_err(|e| format!("cannot parse the image config: {e}"))?;
    let diff_ids = config
        .pointer_mut("/rootfs/diff_ids")
        .and_then(|v| v.as_array_mut())
        .ok_or("the image config has no rootfs.diff_ids")?;
    diff_ids.push(serde_json::Value::String(layer.diff_id.clone()));
    let history = config
        .as_object_mut()
        .ok_or("the image config is not an object")?
        .entry("history")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    history
        .as_array_mut()
        .ok_or("the image config's history is not a list")?
        .push(serde_json::json!({
            "created": rfc3339(created),
            "created_by": "guestfn push --image",
            "comment": format!("the module manifest at {}", oci::MANIFEST_LAYER_PATH),
        }));
    let new_config = serde_json::to_vec(&config).map_err(|e| e.to_string())?;
    let mut manifest = manifest.clone();
    let layer_type = if manifest["mediaType"]
        .as_str()
        .is_some_and(|t| t.contains("docker"))
    {
        DOCKER_LAYER_GZIP_TYPE
    } else {
        OCI_LAYER_GZIP_TYPE
    };
    manifest["config"]["digest"] = serde_json::Value::String(digest_of(&new_config));
    manifest["config"]["size"] = serde_json::Value::from(new_config.len());
    manifest["layers"]
        .as_array_mut()
        .ok_or("the image manifest has no layers")?
        .push(serde_json::json!({
            "mediaType": layer_type,
            "digest": layer.digest(),
            "size": layer.compressed.len(),
            "annotations": {
                oci::MANIFEST_LAYER_ANNOTATION: oci::MANIFEST_LAYER_ANNOTATION_VALUE,
                ANNOTATION_TITLE: oci::MANIFEST_LAYER_PATH.trim_start_matches('/'),
            },
        }));
    Ok((
        serde_json::to_vec(&manifest).map_err(|e| e.to_string())?,
        new_config,
    ))
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
        && r.env.is_empty()
        && r.credentials.is_empty();
    if empty {
        return Ok(None);
    }
    #[derive(Serialize)]
    struct Block<'a> {
        requires: &'a guestfn_runtime::manifest::Requires,
    }
    serde_saphyr::to_string(&Block { requires: r })
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

    /// The manifest layer is reproducible, and the runtime reads the
    /// manifest back out of it the way it reads an image's.
    #[test]
    fn the_manifest_layer_is_reproducible_and_readable() {
        let manifest = br#"{"contract":"oci@0.1","name":"tagger"}"#;
        let a = ManifestLayer::new(manifest, std::time::UNIX_EPOCH);
        let b = ManifestLayer::new(manifest, std::time::UNIX_EPOCH);
        assert_eq!(a.compressed, b.compressed);
        assert_eq!(a.diff_id, b.diff_id);
        assert_ne!(a.digest(), a.diff_id);
        let descriptor: oci::Descriptor = serde_json::from_value(serde_json::json!({
            "mediaType": OCI_LAYER_GZIP_TYPE,
            "digest": a.digest(),
            "size": a.compressed.len(),
            "annotations": {oci::MANIFEST_LAYER_ANNOTATION: oci::MANIFEST_LAYER_ANNOTATION_VALUE},
        }))
        .expect("descriptor");
        assert!(oci::is_manifest_layer(&descriptor));
        assert_eq!(
            oci::manifest_from_layer(&descriptor, &a.compressed, 1 << 16).expect("manifest"),
            manifest
        );
    }

    /// Appending the layer keeps the image an image: one more diff_id and
    /// history entry in the config, one more annotated layer in the
    /// manifest, the config descriptor moved to the new config, the layer
    /// in the manifest's own media type family.
    #[test]
    fn the_appended_layer_moves_the_config_with_it() {
        let layer = ManifestLayer::new(b"{}", std::time::UNIX_EPOCH);
        let config = serde_json::to_vec(&serde_json::json!({
            "architecture": "amd64", "os": "linux",
            "config": {"Entrypoint": ["/tagger"]},
            "rootfs": {"type": "layers", "diff_ids": ["sha256:aaaa"]},
        }))
        .expect("config");
        for (manifest_type, want_layer_type) in [
            (OCI_MANIFEST_MEDIA_TYPE, OCI_LAYER_GZIP_TYPE),
            (
                "application/vnd.docker.distribution.manifest.v2+json",
                DOCKER_LAYER_GZIP_TYPE,
            ),
        ] {
            let manifest = serde_json::json!({
                "schemaVersion": 2,
                "mediaType": manifest_type,
                "config": {"mediaType": oci::IMAGE_CONFIG_TYPES[0], "digest": digest_of(&config), "size": config.len()},
                "layers": [{"mediaType": OCI_LAYER_GZIP_TYPE, "digest": "sha256:bbbb", "size": 4}],
            });
            let (new_manifest, new_config) =
                with_manifest_layer(&manifest, &config, &layer, std::time::UNIX_EPOCH)
                    .expect("appended");
            let c: serde_json::Value = serde_json::from_slice(&new_config).expect("config json");
            assert_eq!(
                c["rootfs"]["diff_ids"].as_array().expect("diff_ids").len(),
                2
            );
            assert_eq!(c["rootfs"]["diff_ids"][1], layer.diff_id);
            assert_eq!(c["history"][0]["created_by"], "guestfn push --image");
            assert_eq!(c["history"][0]["created"], "1970-01-01T00:00:00Z");
            assert_eq!(c["config"]["Entrypoint"][0], "/tagger");
            let m: oci::OciManifest = serde_json::from_slice(&new_manifest).expect("manifest json");
            assert!(oci::is_module_image(&m));
            assert_eq!(m.layers.len(), 2);
            assert_eq!(m.layers[1].media_type, want_layer_type);
            assert_eq!(m.layers[1].digest, layer.digest());
            assert_eq!(oci::wasm_layer(&m).unwrap_err(), oci::MODULE_IMAGE_REFUSAL);
            assert_eq!(m.config.expect("config").digest, digest_of(&new_config));
        }
        let not_an_image = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_MEDIA_TYPE,
            "config": {"mediaType": WASM_CONFIG_MEDIA_TYPE, "digest": digest_of(&config), "size": config.len()},
            "layers": [],
        });
        // with_manifest_layer trusts its caller; append_layer refuses a
        // non-image before it gets here, which the CLI test covers.
        assert!(with_manifest_layer(&not_an_image, &config, &layer, std::time::UNIX_EPOCH).is_ok());
    }

    #[test]
    fn rfc3339_renders_the_epoch() {
        assert_eq!(rfc3339(std::time::UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_756_166_400);
        assert_eq!(rfc3339(t), "2025-08-26T00:00:00Z");
    }
}
