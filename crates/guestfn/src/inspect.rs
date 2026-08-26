//! guestfn inspect: what a module (or an artifact in a registry) is made
//! of, as the runtime sees it - read by the runtime's own engine, so a
//! verdict printed on a laptop is the verdict a load reaches.

use function_wasm::location::parse_any_reference;
use function_wasm::manifest::Manifest;
use function_wasm::oci::{self, Descriptor, OciManifest, RegistryClient};
use function_wasm_engine::{Inspection, WASI_MODULE};
use serde::Serialize;

#[derive(clap::Args, Debug)]
pub struct InspectCmd {
    /// A module file, compiled with wasmtime for the runtime's own view
    /// (seconds for a large Go module); or an OCI reference (tag or digest)
    /// described from its manifest - media types, layer size, annotations -
    /// without pulling.
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
    config: DescriptorInfo,
    layers: Vec<DescriptorInfo>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    annotations: std::collections::BTreeMap<String, String>,
    /// The layer the runtime would take as the module, by the resolver's
    /// own rule.
    #[serde(rename = "moduleLayer", skip_serializing_if = "Option::is_none")]
    module_layer: Option<DescriptorInfo>,
    #[serde(rename = "moduleLayerError", skip_serializing_if = "String::is_empty")]
    module_layer_error: String,
    /// The module manifest the artifact carries as its manifest layer, read
    /// without pulling the module.
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<Manifest>,
    #[serde(rename = "manifestError", skip_serializing_if = "String::is_empty")]
    manifest_error: String,
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
    /// The ABI the binary format names: 1 for a core module, 2 for a
    /// component - stated even when the module fails its ABI's check.
    #[serde(rename = "abiVersion")]
    abi_version: u8,
    /// "v1" or "v2" when the module passes the runtime's check; otherwise
    /// empty, with abiError saying what the runtime says at load.
    #[serde(skip_serializing_if = "String::is_empty")]
    abi: String,
    #[serde(rename = "abiError", skip_serializing_if = "String::is_empty")]
    abi_error: String,
    exports: Vec<ExternInfo>,
    imports: Vec<ExternInfo>,
    memories: Vec<MemoryInfo>,
}

#[derive(Serialize)]
struct ExternInfo {
    #[serde(skip_serializing_if = "String::is_empty")]
    module: String,
    name: String,
    kind: String,
    #[serde(rename = "type", skip_serializing_if = "String::is_empty")]
    ty: String,
}

#[derive(Serialize)]
struct MemoryInfo {
    #[serde(rename = "minPages")]
    min_pages: u64,
    #[serde(rename = "maxPages", skip_serializing_if = "Option::is_none")]
    max_pages: Option<u64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    shared: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    memory64: bool,
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
        let mut info = ReferenceInfo {
            digest,
            media_type: m.media_type.clone(),
            size: raw.len() as i64,
            config: m
                .config
                .as_ref()
                .map(descriptor)
                .unwrap_or_else(|| descriptor(&Descriptor::default())),
            layers: m.layers.iter().map(descriptor).collect(),
            annotations: m.annotations.clone(),
            module_layer: None,
            module_layer_error: String::new(),
            manifest: None,
            manifest_error: String::new(),
        };
        let layer = match oci::wasm_layer(&m) {
            Ok(l) => {
                info.module_layer = Some(descriptor(&l));
                Some(l)
            }
            Err(e) => {
                info.module_layer_error = e;
                None
            }
        };
        if let Some(ml) = oci::manifest_layer(&m) {
            match fetch_manifest(&client, &ml) {
                Ok(parsed) => info.manifest = Some(parsed),
                Err(e) => info.manifest_error = e,
            }
        }
        out.reference = Some(info);
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
                "{}: manifest {} ({}, {})",
                out.target,
                r.digest,
                r.media_type,
                crate::human_bytes(r.size as u64)
            );
            println!(
                "  config: {} {} ({})",
                r.config.media_type,
                r.config.digest,
                crate::human_bytes(r.config.size as u64)
            );
            for l in &r.layers {
                println!(
                    "  layer: {} {} ({}){}",
                    l.media_type,
                    l.digest,
                    crate::human_bytes(l.size as u64),
                    annotations_text(&l.annotations)
                );
            }
            if !r.annotations.is_empty() {
                println!("  annotations:{}", annotations_text(&r.annotations));
            }
            match &r.module_layer {
                Some(l) => println!(
                    "  module layer: {} {} ({})",
                    l.media_type,
                    l.digest,
                    crate::human_bytes(l.size as u64)
                ),
                None => println!(
                    "  module layer: none - the runtime would refuse it: {}",
                    r.module_layer_error
                ),
            }
            match (&r.manifest, r.manifest_error.as_str()) {
                (Some(m), _) => println!("  manifest: {}", manifest_text(m)),
                (None, "") => println!("  manifest: none"),
                (None, e) => {
                    println!("  manifest: invalid - the runtime would refuse the module: {e}")
                }
            }
        }
        if let Some(m) = &out.module {
            let verdict = if m.abi.is_empty() {
                format!("not ABI v{}: {}", m.abi_version, m.abi_error)
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
            for mem in &m.memories {
                let mut line = format!(
                    "  memory: {} pages ({}) initial",
                    mem.min_pages,
                    pages_text(mem.min_pages)
                );
                match mem.max_pages {
                    Some(max) => {
                        line += &format!(", {max} pages ({}) maximum", pages_text(max));
                    }
                    None => line += ", no maximum",
                }
                if mem.shared {
                    line += ", shared";
                }
                if mem.memory64 {
                    line += ", 64-bit";
                }
                println!("{line}");
            }
        }
        Ok(())
    }
}

/// A registry client for any reference, authenticated with the local Docker
/// config.
pub(crate) fn client_for(reference: &function_wasm::location::AnyReference) -> RegistryClient {
    let pinned = function_wasm::location::OciReference {
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
    let raw = client
        .blob(
            &layer.digest,
            function_wasm::manifest::MAX_SIZE as u64,
            "manifest layer",
        )
        .map_err(|e| format!("cannot fetch the manifest layer: {e}"))?;
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
fn describe_module(wasm: &[u8]) -> Result<ModuleInfo, String> {
    let shape: Inspection = crate::inspect_module(wasm)?;
    let (abi, abi_error) = match &shape.abi_error {
        None => (format!("v{}", shape.abi_version), String::new()),
        Some(e) => (String::new(), e.clone()),
    };
    Ok(ModuleInfo {
        size: wasm.len(),
        abi_version: shape.abi_version,
        abi,
        abi_error,
        exports: shape
            .exports
            .iter()
            .map(|e| ExternInfo {
                module: String::new(),
                name: e.name.clone(),
                kind: e.kind.clone(),
                ty: e.ty.clone(),
            })
            .collect(),
        imports: shape
            .imports
            .iter()
            .map(|i| ExternInfo {
                module: i.module.clone(),
                name: i.name.clone(),
                kind: i.kind.clone(),
                ty: i.ty.clone(),
            })
            .collect(),
        memories: shape
            .memories
            .iter()
            .map(|m| MemoryInfo {
                min_pages: m.min,
                max_pages: m.max,
                shared: m.shared,
                memory64: m.memory64,
            })
            .collect(),
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
        .map(|x| {
            if x.ty.is_empty() {
                format!("{} ({})", x.name, x.kind)
            } else {
                format!("{} {}", x.name, x.ty)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Lists host imports one by one and WASI as a count: forty
/// wasi_snapshot_preview1 names say nothing a reader needs.
fn imports_text(xs: &[ExternInfo]) -> String {
    if xs.is_empty() {
        return "none".to_string();
    }
    let mut wasi = 0;
    let mut parts = Vec::new();
    for x in xs {
        if x.module == WASI_MODULE {
            wasi += 1;
            continue;
        }
        if x.ty.is_empty() {
            parts.push(format!("{}.{} ({})", x.module, x.name, x.kind));
        } else {
            parts.push(format!("{}.{} {}", x.module, x.name, x.ty));
        }
    }
    if wasi > 0 {
        parts.insert(0, format!("{WASI_MODULE} ({wasi})"));
    }
    parts.join(", ")
}

/// Renders a page count as bytes; a count past what fits in bytes (a
/// memory64 declaration) is shown as pages only.
fn pages_text(pages: u64) -> String {
    if pages > 1 << 40 {
        return format!("{pages} pages");
    }
    crate::human_bytes(pages << 16)
}

fn annotations_text(a: &std::collections::BTreeMap<String, String>) -> String {
    a.iter().map(|(k, v)| format!(" {k}={v}")).collect()
}
