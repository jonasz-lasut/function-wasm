//! guestfn inspect: what a module (or an artifact in a registry) is made
//! of, as the runtime sees it - read by the runtime's own engine, so a
//! verdict printed on a laptop is the verdict a load reaches.

use guestfn_engine::Inspection;
use guestfn_runtime::location::parse_any_reference;
use guestfn_runtime::manifest::Manifest;
use guestfn_runtime::oci::{self, Descriptor, OciManifest, RegistryClient};
use serde::Serialize;

#[derive(clap::Args, Debug)]
pub struct InspectCmd {
    /// A module file, compiled with wasmtime for the runtime's own view
    /// (seconds for a large Go module); or an OCI reference (tag or digest)
    /// described from its manifest - media types, layer size, annotations -
    /// without pulling. An image index is described platform by platform.
    target: String,

    /// For a reference: also pull the module layer and read the module, as
    /// for a file.
    #[arg(long)]
    pull: bool,

    /// text or json.
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    output: String,

    /// Largest module to read in MB.
    #[arg(long, default_value_t = 128)]
    max_size: u64,
}

/// Everything inspect prints, in JSON field order.
#[derive(Serialize)]
struct InspectionOut {
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<ReferenceInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    module: Option<ModuleInfo>,
}

#[derive(Serialize)]
struct ReferenceInfo {
    digest: String,
    #[serde(rename = "mediaType")]
    media_type: String,
    size: i64,
    /// Absent for an image index and for an index entry that names no
    /// platform: neither is read as a manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<DescriptorInfo>,
    layers: Vec<DescriptorInfo>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    annotations: std::collections::BTreeMap<String, String>,
    /// The layer the runtime would take as the module, by the resolver's
    /// own rule.
    #[serde(rename = "moduleLayer", skip_serializing_if = "Option::is_none")]
    module_layer: Option<DescriptorInfo>,
    #[serde(rename = "moduleLayerError", skip_serializing_if = "String::is_empty")]
    module_layer_error: String,
    /// For an OCI module image: the image its manifest says it was built
    /// from (the manifest's image field).
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    /// The module manifest the artifact carries as its manifest layer, read
    /// without pulling the module.
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<Manifest>,
    #[serde(rename = "manifestError", skip_serializing_if = "String::is_empty")]
    manifest_error: String,
    /// For an image index: every entry, each platform's manifest described
    /// as a manifest of its own is. The runtime runs the entry of its
    /// node's platform.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    platforms: Vec<PlatformInfo>,
    #[serde(skip)]
    index: bool,
}

/// One entry of an image index.
#[derive(Serialize)]
struct PlatformInfo {
    /// `os/architecture`; empty for an entry that names no platform (an
    /// attestation a builder attached), which the runtime never runs.
    platform: String,
    #[serde(flatten)]
    reference: ReferenceInfo,
}

#[derive(Serialize, Clone)]
struct DescriptorInfo {
    #[serde(rename = "mediaType")]
    media_type: String,
    digest: String,
    size: i64,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    annotations: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ModuleInfo {
    size: usize,
    /// "v2" when the component passes the runtime's check; otherwise empty,
    /// with abiError saying what the runtime says at load.
    #[serde(skip_serializing_if = "String::is_empty")]
    abi: String,
    #[serde(rename = "abiError", skip_serializing_if = "String::is_empty")]
    abi_error: String,
    exports: Vec<ExternInfo>,
    imports: Vec<ExternInfo>,
}

/// One top-level item of the component: the world's `run` and `log`, a
/// WASI interface, or whatever else the toolchain put there.
#[derive(Serialize)]
struct ExternInfo {
    name: String,
    kind: String,
}

impl InspectCmd {
    pub fn run(&self) -> Result<(), String> {
        let mut out = InspectionOut {
            target: self.target.clone(),
            reference: None,
            module: None,
        };
        let path = std::path::Path::new(&self.target);
        if path.is_file() {
            let wasm = std::fs::read(path).map_err(|e| e.to_string())?;
            out.module = Some(describe_module(&wasm).map_err(|e| format!("{}: {e}", self.target))?);
            return self.print(&out);
        }
        let reference = parse_any_reference(&self.target).map_err(|e| {
            format!(
                "{} is neither a file nor an OCI reference: {e}",
                self.target
            )
        })?;
        let client = client_for(&reference);
        let (raw, digest) = client.raw_manifest(&reference.manifest_ref())?;
        let m: OciManifest = serde_json::from_slice(&raw)
            .map_err(|e| format!("cannot parse manifest {digest}: {e}"))?;
        let mut module_image = false;
        let mut layer = None;
        let info = if m.is_index() {
            let mut info = ReferenceInfo::bare(digest, m.media_type.clone(), raw.len() as i64);
            info.annotations = m.annotations.clone();
            info.index = true;
            for entry in index_entries(&client, &m)? {
                let platform = platform_text(&entry.descriptor);
                let reference = match &entry.manifest {
                    Some((em, entry_raw)) => {
                        let d = describe_manifest(
                            &client,
                            entry.descriptor.digest.clone(),
                            entry_raw.len() as i64,
                            em,
                        );
                        module_image |= d.module_image;
                        d.info
                    }
                    None => {
                        let mut bare = ReferenceInfo::bare(
                            entry.descriptor.digest.clone(),
                            entry.descriptor.media_type.clone(),
                            entry.descriptor.size,
                        );
                        bare.annotations = entry.descriptor.annotations.clone();
                        bare
                    }
                };
                info.platforms.push(PlatformInfo {
                    platform,
                    reference,
                });
            }
            info
        } else {
            let d = describe_manifest(&client, digest, raw.len() as i64, &m);
            module_image = d.module_image;
            layer = d.layer;
            d.info
        };
        let index = info.index;
        out.reference = Some(info);
        if self.pull && module_image {
            return Err(format!(
                "{} is an OCI image (an oci@0.1 module) and has no module file to pull; the OCI runtime, function-oci, pulls and boots it",
                self.target
            ));
        }
        if self.pull && index {
            return Err(format!(
                "{} is an image index and has no module file to pull",
                self.target
            ));
        }
        if self.pull
            && let Some(layer) = layer
        {
            let wasm = pull_layer(&client, &layer, self.max_size << 20)?;
            out.module = Some(describe_module(&wasm).map_err(|e| format!("{}: {e}", self.target))?);
        }
        self.print(&out)
    }

    fn print(&self, out: &InspectionOut) -> Result<(), String> {
        if self.output == "json" {
            println!(
                "{}",
                serde_json::to_string_pretty(out).map_err(|e| e.to_string())?
            );
            return Ok(());
        }
        if let Some(r) = &out.reference {
            println!(
                "{}: {} {} ({}, {})",
                out.target,
                if r.index { "index" } else { "manifest" },
                r.digest,
                r.media_type,
                crate::human_bytes(r.size as u64)
            );
            if r.index {
                if !r.annotations.is_empty() {
                    println!("  annotations:{}", annotations_text(&r.annotations));
                }
                for p in &r.platforms {
                    let e = &p.reference;
                    if e.config.is_none() {
                        println!(
                            "  no platform: manifest {} ({}, {}){} - the runtime never runs it",
                            e.digest,
                            e.media_type,
                            crate::human_bytes(e.size as u64),
                            annotations_text(&e.annotations)
                        );
                        continue;
                    }
                    println!(
                        "  {}: manifest {} ({}, {})",
                        p.platform,
                        e.digest,
                        e.media_type,
                        crate::human_bytes(e.size as u64)
                    );
                    print_manifest(e, "    ");
                }
            } else {
                print_manifest(r, "  ");
            }
        }
        if let Some(m) = &out.module {
            let verdict = if m.abi.is_empty() {
                format!("not ABI v2: {}", m.abi_error)
            } else {
                format!("ABI {}", m.abi)
            };
            if out.reference.is_none() {
                println!(
                    "{}: {}, {verdict}",
                    out.target,
                    crate::human_bytes(m.size as u64)
                );
            } else {
                println!("  module: {}, {verdict}", crate::human_bytes(m.size as u64));
            }
            println!("  exports: {}", externs_text(&m.exports));
            println!("  imports: {}", imports_text(&m.imports));
        }
        Ok(())
    }
}

/// What a manifest says of itself, each line under pad: its config and
/// layers, the module by the resolver's rule, and the module manifest.
fn print_manifest(r: &ReferenceInfo, pad: &str) {
    if let Some(config) = &r.config {
        println!(
            "{pad}config: {} {} ({})",
            config.media_type,
            config.digest,
            crate::human_bytes(config.size as u64)
        );
    }
    for l in &r.layers {
        println!(
            "{pad}layer: {} {} ({}){}",
            l.media_type,
            l.digest,
            crate::human_bytes(l.size as u64),
            annotations_text(&l.annotations)
        );
    }
    if !r.annotations.is_empty() {
        println!("{pad}annotations:{}", annotations_text(&r.annotations));
    }
    match (&r.module_layer, &r.image) {
        (Some(l), _) => println!(
            "{pad}module layer: {} {} ({})",
            l.media_type,
            l.digest,
            crate::human_bytes(l.size as u64)
        ),
        (None, Some(image)) => println!(
            "{pad}module: an OCI image (oci@0.1), which the OCI runtime, function-oci, boots; built from {image}"
        ),
        (None, None) => println!(
            "{pad}module layer: none - the runtime would refuse it: {}",
            r.module_layer_error
        ),
    }
    match (&r.manifest, r.manifest_error.as_str()) {
        (Some(m), _) => println!("{pad}manifest: {}", manifest_text(m)),
        (None, "") => println!("{pad}manifest: none"),
        (None, e) => {
            println!("{pad}manifest: invalid - the runtime would refuse the module: {e}")
        }
    }
}

impl ReferenceInfo {
    /// A manifest known by its descriptor alone: an index, or an entry
    /// nothing reads further.
    fn bare(digest: String, media_type: String, size: i64) -> ReferenceInfo {
        ReferenceInfo {
            digest,
            media_type,
            size,
            config: None,
            layers: Vec::new(),
            annotations: std::collections::BTreeMap::new(),
            module_layer: None,
            module_layer_error: String::new(),
            image: None,
            manifest: None,
            manifest_error: String::new(),
            platforms: Vec::new(),
            index: false,
        }
    }
}

/// A manifest described, with what inspect needs beyond the description.
struct Described {
    info: ReferenceInfo,
    /// Whether it is an OCI module image, which has no module file.
    module_image: bool,
    /// The module layer, when the resolver's rule finds one.
    layer: Option<Descriptor>,
}

/// Describes one manifest as the runtime reads it: the module layer by
/// the resolver's own rule, and the module manifest out of its layer.
fn describe_manifest(
    client: &RegistryClient,
    digest: String,
    size: i64,
    m: &OciManifest,
) -> Described {
    let mut info = ReferenceInfo::bare(digest, m.media_type.clone(), size);
    info.config = Some(
        m.config
            .as_ref()
            .map(descriptor)
            .unwrap_or_else(|| descriptor(&Descriptor::default())),
    );
    info.layers = m.layers.iter().map(descriptor).collect();
    info.annotations = m.annotations.clone();
    let module_image = oci::is_module_image(m);
    let layer = if module_image {
        info.module_layer_error = oci::MODULE_IMAGE_REFUSAL.to_string();
        None
    } else {
        match oci::wasm_layer(m) {
            Ok(l) => {
                info.module_layer = Some(descriptor(&l));
                Some(l)
            }
            Err(e) => {
                info.module_layer_error = e;
                None
            }
        }
    };
    if let Some(ml) = oci::manifest_layer(m) {
        match fetch_manifest(client, &ml) {
            Ok(parsed) => {
                if module_image && !parsed.image().is_empty() {
                    info.image = Some(parsed.image().to_string());
                }
                info.manifest = Some(parsed);
            }
            Err(e) => info.manifest_error = e,
        }
    }
    Described {
        info,
        module_image,
        layer,
    }
}

/// One entry of an image index: its descriptor and, where it names a
/// platform, its manifest with the raw bytes, fetched and held to the
/// entry's digest. An entry with no platform (an attestation a builder
/// attached under `unknown/unknown`) is listed and never read.
pub(crate) struct IndexEntry {
    pub descriptor: Descriptor,
    pub manifest: Option<(OciManifest, Vec<u8>)>,
}

pub(crate) fn index_entries(
    client: &RegistryClient,
    index: &OciManifest,
) -> Result<Vec<IndexEntry>, String> {
    let mut entries = Vec::new();
    for descriptor in index.index_entries() {
        if platform_text(&descriptor).is_empty() {
            entries.push(IndexEntry {
                descriptor,
                manifest: None,
            });
            continue;
        }
        let (raw, got) = client.raw_manifest(&descriptor.digest)?;
        if got != descriptor.digest {
            return Err(format!(
                "image manifest content is {got}, want {}",
                descriptor.digest
            ));
        }
        let manifest: OciManifest = serde_json::from_slice(&raw)
            .map_err(|e| format!("cannot parse manifest {}: {e}", descriptor.digest))?;
        entries.push(IndexEntry {
            descriptor,
            manifest: Some((manifest, raw)),
        });
    }
    Ok(entries)
}

/// An index entry's platform as `os/architecture`, or empty when it names
/// none.
fn platform_text(entry: &Descriptor) -> String {
    match &entry.platform {
        Some(p) if !p.os.is_empty() && p.os != "unknown" => format!("{}/{}", p.os, p.architecture),
        _ => String::new(),
    }
}

/// A registry client for any reference, authenticated with the local Docker
/// config.
pub(crate) fn client_for(reference: &guestfn_runtime::location::AnyReference) -> RegistryClient {
    let pinned = guestfn_runtime::location::OciReference {
        registry: reference.registry.clone(),
        repository: reference.repository.clone(),
        digest: reference.digest.clone().unwrap_or_default(),
    };
    RegistryClient::new(&pinned, oci::keychain_auth(&reference.registry))
}

/// Pulls the manifest layer - kilobytes - and parses it as the runtime
/// would.
pub(crate) fn fetch_manifest(
    client: &RegistryClient,
    layer: &Descriptor,
) -> Result<Manifest, String> {
    let limit = guestfn_runtime::manifest::MAX_SIZE as u64;
    let blob = client
        .blob(&layer.digest, 2 * limit, "manifest layer")
        .map_err(|e| format!("cannot fetch the manifest layer: {e}"))?;
    let raw = oci::manifest_from_layer(layer, &blob, limit)
        .map_err(|e| format!("the manifest layer is invalid: {e}"))?;
    Manifest::parse(&raw).map_err(|e| format!("the manifest layer is invalid: {e}"))
}

/// Fetches one layer's bytes - a raw layer, or /fn.wasm out of a tar layer
/// - bounded by limit.
pub(crate) fn pull_layer(
    client: &RegistryClient,
    layer: &Descriptor,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let b = client
        .blob(&layer.digest, limit, "module layer")
        .map_err(|e| format!("cannot fetch layer: {e}"))?;
    if oci::is_tar_layer(&layer.media_type) {
        return oci::extract_wasm(&b, limit);
    }
    Ok(b)
}

fn descriptor(d: &Descriptor) -> DescriptorInfo {
    DescriptorInfo {
        media_type: d.media_type.clone(),
        digest: d.digest.clone(),
        size: d.size,
        annotations: d.annotations.clone(),
    }
}

/// Compiles a module with the runtime's engine and reports what it sees.
/// A core module never gets here: the engine refuses it by name, and the
/// refusal is the command's error.
fn describe_module(wasm: &[u8]) -> Result<ModuleInfo, String> {
    let shape: Inspection = crate::inspect_module(wasm)?;
    let (abi, abi_error) = match &shape.abi_error {
        None => ("v2".to_string(), String::new()),
        Some(e) => (String::new(), e.clone()),
    };
    let item = |e: &guestfn_engine::Extern| ExternInfo {
        name: e.name.clone(),
        kind: e.kind.clone(),
    };
    Ok(ModuleInfo {
        size: wasm.len(),
        abi,
        abi_error,
        exports: shape.exports.iter().map(item).collect(),
        imports: shape.imports.iter().map(item).collect(),
    })
}

/// A manifest's one-line summary, or "declares nothing" for an empty one.
pub(crate) fn manifest_text(m: &Manifest) -> String {
    let s = m.summary();
    if s.is_empty() {
        "declares nothing".to_string()
    } else {
        s
    }
}

fn externs_text(xs: &[ExternInfo]) -> String {
    if xs.is_empty() {
        return "none".to_string();
    }
    xs.iter()
        .map(|x| format!("{} ({})", x.name, x.kind))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The namespace of every WASI interface a component imports.
const WASI_NAMESPACE: &str = "wasi:";

/// Lists the world's imports one by one and WASI as a count: twenty
/// `wasi:*` interface names say nothing a reader needs.
fn imports_text(xs: &[ExternInfo]) -> String {
    if xs.is_empty() {
        return "none".to_string();
    }
    let mut wasi = 0;
    let mut parts = Vec::new();
    for x in xs {
        if x.name.starts_with(WASI_NAMESPACE) {
            wasi += 1;
            continue;
        }
        parts.push(format!("{} ({})", x.name, x.kind));
    }
    if wasi > 0 {
        parts.insert(0, format!("{WASI_NAMESPACE}* ({wasi})"));
    }
    parts.join(", ")
}

fn annotations_text(a: &std::collections::BTreeMap<String, String>) -> String {
    a.iter().map(|(k, v)| format!(" {k}={v}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An index entry counts as a platform's image only when it names one:
    /// a builder's attestation sits under unknown/unknown and is listed,
    /// never read.
    #[test]
    fn an_entry_without_a_platform_is_not_read() {
        let entry = |platform: Option<(&str, &str)>| Descriptor {
            platform: platform.map(|(os, architecture)| oci::Platform {
                os: os.to_string(),
                architecture: architecture.to_string(),
            }),
            ..Descriptor::default()
        };
        assert_eq!(
            platform_text(&entry(Some(("linux", "arm64")))),
            "linux/arm64"
        );
        assert_eq!(platform_text(&entry(Some(("unknown", "unknown")))), "");
        assert_eq!(platform_text(&entry(None)), "");
    }
}
