//! guestfn scaffold composition: a Composition step for a module, from its
//! manifest - the module source, a config skeleton from the manifest's
//! schema, and a commented compositionPolicy skeleton derived from the
//! manifest's requires.

use std::path::{Path, PathBuf};

use function_wasm::location::parse_any_reference;
use function_wasm::manifest::Manifest;
use function_wasm::oci;

#[derive(clap::Args, Debug)]
pub struct CompositionCmd {
    /// The module: a file (a Path source, served from its directory) or an
    /// OCI reference (pinned to its manifest digest in the output, its
    /// manifest read from the artifact's manifest layer).
    #[arg(long, default_value = "fn.wasm")]
    from: String,

    /// The manifest to scaffold from; by default the wasmfn.yaml next to a
    /// module file (when there is one), or the artifact's manifest layer
    /// for a reference.
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// The step's name (and the Composition's, with --full); defaults to
    /// the manifest's name, else the module file's base name.
    #[arg(long)]
    name: Option<String>,

    /// The functionRef.name of the step.
    #[arg(long, default_value = "function-wasm")]
    function_name: String,

    /// Print a whole Composition, like a scaffold's example/composition.yaml,
    /// instead of one pipeline step.
    #[arg(long)]
    full: bool,
}

/// The module source of a step, rendered by hand so the output matches the
/// Input's own YAML shape.
enum Source {
    Path(String),
    Oci(String),
}

impl CompositionCmd {
    pub fn run(&self) -> Result<(), String> {
        let (src, m) = self.source()?;
        let name = match &self.name {
            Some(n) => n.clone(),
            None => match &m {
                Some(m) if !m.name.is_empty() => m.name.clone(),
                _ => match &src {
                    Source::Path(p) => Path::new(p)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "module".to_string()),
                    Source::Oci(_) => "module".to_string(),
                },
            },
        };
        let step = composition_step(&name, &self.function_name, &src, m.as_ref())?;
        if !self.full {
            print!("{step}");
            return Ok(());
        }
        print!(
            "apiVersion: apiextensions.crossplane.io/v1\nkind: Composition\nmetadata:\n  name: {name}\nspec:\n  compositeTypeRef:\n    apiVersion: example.crossplane.io/v1\n    kind: XR\n  mode: Pipeline\n  pipeline:\n{}",
            indent(&step, "  ")
        );
        Ok(())
    }

    /// Resolves --from to a module source and its manifest: --manifest,
    /// else the wasmfn.yaml beside a module file, else an artifact's
    /// manifest layer; None when there is none.
    fn source(&self) -> Result<(Source, Option<Manifest>), String> {
        let mut m = match &self.manifest {
            Some(p) => Some(Manifest::load(p)?),
            None => None,
        };
        let from = Path::new(&self.from);
        if from.is_file() {
            if m.is_none() {
                let candidate = from
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(function_wasm::manifest::FILE_NAME);
                if candidate.is_file() {
                    m = Some(Manifest::load(&candidate)?);
                }
            }
            let base = from
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.from.clone());
            return Ok((Source::Path(base), m));
        }
        let reference = parse_any_reference(&self.from)
            .map_err(|e| format!("{} is neither a file nor an OCI reference: {e}", self.from))?;
        let client = crate::inspect::client_for(&reference);
        let (raw, digest) = client.raw_manifest(&reference.manifest_ref())?;
        let om: oci::OciManifest = serde_json::from_slice(&raw)
            .map_err(|e| format!("cannot parse manifest {digest}: {e}"))?;
        if m.is_none()
            && let Some(ml) = oci::manifest_layer(&om)
        {
            m = Some(
                crate::inspect::fetch_manifest(&client, &ml)
                    .map_err(|e| format!("{}: {e}", self.from))?,
            );
        }
        Ok((Source::Oci(reference.pinned(&digest)), m))
    }
}

/// Renders one pipeline step: the module, commented limits and a config
/// skeleton from the schema. The module's sandbox needs are its manifest's
/// requires - granted by the operator's policy (and narrowed by a
/// compositionPolicy), never copied into the Input.
fn composition_step(
    name: &str,
    function_name: &str,
    src: &Source,
    m: Option<&Manifest>,
) -> Result<String, String> {
    let mut b = format!(
        "- step: {name}\n  functionRef:\n    name: {function_name}\n  input:\n    apiVersion: {}\n    kind: {}\n",
        crate::INPUT_API_VERSION,
        crate::INPUT_KIND
    );
    let module_yaml = match src {
        Source::Path(p) => format!("module:\n  type: Path\n  path: {p}\n"),
        Source::Oci(r) => format!("module:\n  type: OCI\n  oci:\n    ref: {r}\n"),
    };
    b += &indent(&module_yaml, "    ");
    b += "    # limits: {timeout: 5s, memory: 128Mi}\n";
    if let Some(m) = m
        && let Some(config) = config_skeleton(m)?
    {
        #[derive(serde::Serialize)]
        struct Block {
            config: std::collections::BTreeMap<String, serde_json::Value>,
        }
        let yaml = serde_yaml::to_string(&Block { config }).map_err(|e| e.to_string())?;
        b += &indent(&yaml, "    ");
    }
    if let Some(skeleton) = composition_policy_skeleton(src, m) {
        b += &indent(&skeleton, "    ");
    }
    Ok(b)
}

/// Renders a commented compositionPolicy the author can start from: the
/// Cedar permits this module's manifest requirements would need and, for an
/// OCI source, a pullModule permit for its repository. The whole block is
/// commented: the manifest is the request and the two Cedar layers decide,
/// so a skeleton never copies in a grant. None when nothing is derivable.
/// The comments inside the block use Cedar's `//` so the block is valid
/// once uncommented.
fn composition_policy_skeleton(src: &Source, m: Option<&Manifest>) -> Option<String> {
    let mut body: Vec<String> = Vec::new();
    if let Some(r) = m.and_then(|m| m.requires.as_ref()) {
        if let Some(egress) = &r.egress {
            for h in egress_hosts(&egress.http) {
                body.push(format!(
                    "// egress {h} (grantEgress is also the host allowlist):"
                ));
                body.push(format!(
                    "permit (principal, action == Action::\"grantEgress\", resource in HostPattern::\"{h}\");"
                ));
            }
        }
        if r.filesystem.as_ref().is_some_and(|f| f.private_tmp) {
            body.push("// the private /tmp the module requires:".to_string());
            body.push(
                "permit (principal, action == Action::\"usePrivateTmp\", resource);".to_string(),
            );
        }
        if !r.env.is_empty() {
            body.push("// the env the module binds from step credentials:".to_string());
            body.push("permit (principal, action == Action::\"setEnv\", resource);".to_string());
            for name in credential_names(&r.env) {
                body.push(format!(
                    "permit (principal, action == Action::\"spendCredential\", resource == Credential::\"{name}\");"
                ));
            }
        }
    }
    if let Source::Oci(reference) = src
        && let Ok(r) = function_wasm::location::parse_oci_reference(reference)
    {
        body.push(
            "// for a module.from source, permit pulling this repository (a static".to_string(),
        );
        body.push("// source needs no pullModule):".to_string());
        body.push(format!(
            "permit (principal, action == Action::\"pullModule\", resource in Repository::\"{}\");",
            r.location()
        ));
    }
    if body.is_empty() {
        return None;
    }
    let mut b = String::new();
    b += "# compositionPolicy is the composition author's Cedar layer - optional and\n";
    b += "# narrowing-only. A static source needs none (sandbox actions are scoped\n";
    b += "# default-permit; writing a permit for one opts into narrowing it). These\n";
    b += "# permits, from this module's manifest, are a starting point, never a grant:\n";
    b += "# compositionPolicy: |\n";
    for line in body {
        b += &format!("#   {line}\n");
    }
    Some(b)
}

/// The distinct hosts (or host patterns) of a module's egress rules,
/// first-seen order, so one grantEgress permit is emitted per host.
fn egress_hosts(rules: &[function_wasm::egress_rules::HttpRule]) -> Vec<String> {
    let mut out = Vec::new();
    for r in rules {
        let h = if r.host.is_empty() {
            &r.host_pattern
        } else {
            &r.host
        };
        if h.is_empty() || out.contains(h) {
            continue;
        }
        out.push(h.clone());
    }
    out
}

/// The distinct credential names a module's env bindings spend, first-seen
/// order.
fn credential_names(bindings: &[function_wasm::sandboxenv::EnvBinding]) -> Vec<String> {
    let mut out = Vec::new();
    for b in bindings {
        let name = &b.from_credential.name;
        if name.is_empty() || out.contains(name) {
            continue;
        }
        out.push(name.clone());
    }
    out
}

/// Derives a config block from the manifest's schema: every top-level
/// property, its default where the schema has one, otherwise a placeholder
/// of its type. None without a schema or without properties.
fn config_skeleton(
    m: &Manifest,
) -> Result<Option<std::collections::BTreeMap<String, serde_json::Value>>, String> {
    let Some(schema) = m.config.as_ref().and_then(|c| c.schema.as_ref()) else {
        return Ok(None);
    };
    let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else {
        return Ok(None);
    };
    if properties.is_empty() {
        return Ok(None);
    }
    let mut out = std::collections::BTreeMap::new();
    for (k, p) in properties {
        let value = match p.get("default") {
            Some(d) if !d.is_null() => d.clone(),
            _ => placeholder(p.get("type")),
        };
        out.insert(k.clone(), value);
    }
    Ok(Some(out))
}

/// The empty value of a JSON Schema type ("string", or a list whose first
/// entry decides).
fn placeholder(t: Option<&serde_json::Value>) -> serde_json::Value {
    let t = match t {
        Some(serde_json::Value::Array(list)) if !list.is_empty() => &list[0],
        Some(v) => v,
        None => &serde_json::Value::Null,
    };
    match t.as_str() {
        Some("number") | Some("integer") => serde_json::json!(0),
        Some("boolean") => serde_json::json!(false),
        Some("object") => serde_json::json!({}),
        Some("array") => serde_json::json!([]),
        _ => serde_json::json!(""),
    }
}

/// Prefixes every non-empty line.
fn indent(s: &str, prefix: &str) -> String {
    let mut out: Vec<String> = s
        .trim_end_matches('\n')
        .split('\n')
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{prefix}{l}")
            }
        })
        .collect();
    out.push(String::new());
    out.join("\n")
}
