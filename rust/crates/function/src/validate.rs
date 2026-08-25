//! `function validate`: the runtime's admission over Compositions, offline -
//! the Rust port of cmd/function/validate.go. For every pipeline step whose
//! input is a function-wasm Input it runs exactly what RunFunction runs
//! before it resolves anything and, with --resolve, goes on to resolve and
//! fetch each module and read its ABI. One line per step in the runtime's
//! own words; exit 0 when every step is admitted, 1 when at least one is
//! refused, 2 when the tool itself failed. Output parity with the Go binary
//! is enforced by tests/conformance.rs; a check the port does not carry yet
//! is refused by name, so the differential harness sees an honest gap.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde::Serialize;

use function_wasm_engine::{Config, Engine, duration};

use crate::admission;
use crate::authz::OperatorPolicy;
use crate::input::{Input, ModuleSource};
use crate::quantity;
use crate::resolver::{Resolver, go_io_error};

const INPUT_API_VERSION: &str = "wasm.fn.crossplane.io/v1beta1";
const INPUT_KIND: &str = "Input";
const COMPOSITION_API_VERSION: &str = "apiextensions.crossplane.io/v1";
const COMPOSITION_KIND: &str = "Composition";

#[derive(clap::Args, Debug)]
pub struct ValidateArgs {
    /// Composition or Input files, YAML or JSON, multi-document; - reads
    /// stdin. Every pipeline step whose input is a wasm.fn.crossplane.io/
    /// v1beta1 Input is checked, and every bare Input document.
    #[arg(value_name = "file", required = true)]
    files: Vec<String>,

    /// Only check pipeline steps whose functionRef.name is this; by default
    /// every step carrying a function-wasm Input, whatever the function's
    /// name.
    #[arg(long)]
    function_name: Option<String>,

    /// A composite resource (YAML or JSON) to materialise module.from
    /// sources against, as the observed XR would.
    #[arg(long)]
    xr: Option<PathBuf>,

    /// Also resolve and fetch every module and compile it with wasmtime for
    /// the runtime's own verdict: size, ABI, host imports.
    #[arg(long)]
    resolve: bool,

    /// text: one line per step, warnings indented below it; json: one JSON
    /// object per step, one per line.
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    output: String,

    /// Emit debug logs in addition to info logs.
    #[arg(short, long, default_value_t = false)]
    debug: bool,

    /// Directory served for 'path' module sources; unset refuses them.
    #[arg(long, env = "MODULE_DIR")]
    module_dir: Option<PathBuf>,

    /// Maximum size of a module in MB.
    #[arg(long, default_value_t = 128)]
    max_module_size: u64,

    /// Maximum wall-clock time one module run may take.
    #[arg(long, default_value = "30s", value_parser = duration::parse)]
    module_timeout: Duration,

    /// Maximum linear memory of a running module in MB.
    #[arg(long, default_value_t = 512)]
    module_memory_limit: u64,

    /// PEM file with one or more cosign public keys.
    #[arg(long, env = "COSIGN_KEY")]
    cosign_key: Option<PathBuf>,

    /// Cedar document with the operator's grant policy.
    #[arg(long, env = "SANDBOX_POLICY_FILE")]
    sandbox_policy_file: Option<PathBuf>,

    /// Sustained egress requests per minute per module digest.
    #[arg(
        long,
        env = "EGRESS_RATE_LIMIT_PER_MINUTE",
        default_value_t = 0.0,
        allow_negative_numbers = true
    )]
    egress_rate_limit_per_minute: f64,

    /// Burst tokens for --egress-rate-limit-per-minute.
    #[arg(
        long,
        env = "EGRESS_RATE_LIMIT_BURST",
        default_value_t = 0,
        allow_negative_numbers = true
    )]
    egress_rate_limit_burst: i64,
}

/// The verdict on one step - what one text line and one JSON object carry.
/// Field names and omission match the Go stepResult exactly.
#[derive(Debug, Default, Serialize)]
struct StepResult {
    file: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    composition: String,
    document: usize,
    index: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    step: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    function: String,
    status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    message: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    module: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    details: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved: Option<Resolved>,
}

/// What --resolve reads from a module.
#[derive(Debug, Serialize)]
struct Resolved {
    digest: String,
    size: usize,
    abi: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    imports: Vec<String>,
    /// The module's manifest, when it carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<crate::manifest::Manifest>,
}

/// One Input found in a file, with where it came from.
struct Step {
    file: String,
    composition: String,
    document: usize,
    index: i64,
    name: String,
    function: String,
    input: serde_json::Map<String, serde_json::Value>,
}

pub fn run(args: &ValidateArgs) -> ExitCode {
    match run_inner(args) {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::from(1),
        Err(e) => {
            eprintln!("function validate: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_inner(args: &ValidateArgs) -> Result<bool, String> {
    if args.egress_rate_limit_per_minute < 0.0 || args.egress_rate_limit_burst < 0 {
        return Err(
            "--egress-rate-limit-per-minute and --egress-rate-limit-burst must not be negative"
                .to_string(),
        );
    }
    let mut policy = None;
    if let Some(path) = &args.sandbox_policy_file {
        let p = OperatorPolicy::load(path)?;
        // The SSRF CIDR rules compile at load, as at the runtime's startup: a
        // malformed rule stops the tool rather than meaning less than written.
        p.compile_ip_rules()?;
        policy = Some(p);
    }
    if args.cosign_key.is_some() {
        return Err("--cosign-key is not implemented yet in the Rust runtime".to_string());
    }
    let ceilings = Config {
        timeout: args.module_timeout,
        memory_limit: args.module_memory_limit << 20,
    };
    let mut engine = None;
    let mut resolver = None;
    if args.resolve {
        // wasmtime reads a module only by compiling it: --resolve pays the
        // compile the runtime would, for the verdict the runtime would reach.
        engine = Some(Engine::new(ceilings).map_err(|e| e.to_string())?);
        resolver = Some(Resolver::new(
            args.module_dir.clone(),
            args.max_module_size << 20,
        ));
    }
    let mut xr = None;
    if let Some(path) = &args.xr {
        let file = path.display().to_string();
        let docs = read_documents(&file)?;
        if docs.len() != 1 {
            return Err(format!(
                "--xr {file} must hold exactly one document, found {}",
                docs.len()
            ));
        }
        xr = docs.into_iter().next();
    }

    let v = Validator {
        ceilings,
        engine,
        resolver,
        xr,
        policy,
        cosign_key: args.cosign_key.is_some(),
    };
    let mut refused = false;
    for file in &args.files {
        let docs = read_documents(file)?;
        let steps = find_steps(file, &docs, args.function_name.as_deref());
        if steps.is_empty() {
            eprintln!("{file}: no function-wasm Input found");
        }
        for s in steps {
            let result = v.validate(s);
            if result.status != "ok" {
                refused = true;
            }
            print_result(&args.output, &result)?;
        }
    }
    Ok(refused)
}

fn print_result(output: &str, r: &StepResult) -> Result<(), String> {
    if output == "json" {
        println!("{}", serde_json::to_string(r).map_err(|e| e.to_string())?);
        return Ok(());
    }
    let mut where_ = format!("{}: ", r.file);
    if r.index >= 0 {
        where_ += &format!(
            "Composition/{} pipeline[{}] {}",
            r.composition, r.index, r.step
        );
    } else {
        where_ += &format!("Input[{}]", r.document);
        if !r.step.is_empty() {
            where_ += &format!(" {}", r.step);
        }
    }
    if r.status != "ok" {
        println!("{where_}: refused: {}", r.message);
    } else {
        let mut parts = vec![r.module.clone()];
        parts.extend(r.details.iter().cloned());
        println!("{where_}: OK ({})", parts.join(", "));
    }
    if let Some(resolved) = &r.resolved {
        let mut line = format!(
            "  module: {}, {}, ABI {}",
            resolved.digest,
            human_bytes(resolved.size),
            resolved.abi
        );
        if !resolved.imports.is_empty() {
            line += &format!(", imports {}", resolved.imports.join(" "));
        }
        if let Some(m) = &resolved.manifest {
            line += &format!("; manifest: {}", m.summary());
        }
        println!("{line}");
    }
    for warning in &r.warnings {
        println!("  warning: {warning}");
    }
    Ok(())
}

/// Reads every YAML or JSON document of a file (- is stdin) as an
/// unstructured object; empty documents are skipped. The error strings
/// mirror the Go tool's (its YAML layer wraps go-yaml, whose parser messages
/// libyaml-descended parsers share).
fn read_documents(file: &str) -> Result<Vec<serde_json::Value>, String> {
    let raw = if file == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| format!("cannot read {file}: {e}"))?;
        buf
    } else {
        std::fs::read(file).map_err(|e| {
            format!(
                "cannot read {file}: {}",
                go_io_error("open", std::path::Path::new(file), &e)
            )
        })?
    };
    let mut docs = Vec::new();
    for de in serde_yaml::Deserializer::from_slice(&raw) {
        let doc = serde_json::Value::deserialize(de).map_err(|e| parse_error(file, &e))?;
        if doc.is_null() {
            continue;
        }
        docs.push(doc);
    }
    Ok(docs)
}

use serde::Deserialize as _;

/// Renders a YAML parse failure the way the Go tool does: its YAML layer
/// reports "error converting YAML to JSON: yaml: line N: <message>", and the
/// message wording is libyaml's, which serde_yaml shares.
fn parse_error(file: &str, e: &serde_yaml::Error) -> String {
    let mut msg = e.to_string();
    // go-yaml prints libyaml's zero-based line; serde_yaml's location is
    // one-based.
    let line = e.location().map(|l| l.line().saturating_sub(1));
    if let Some(i) = msg.find(" at line ") {
        msg.truncate(i);
    }
    match line {
        Some(line) => {
            format!("cannot parse {file}: error converting YAML to JSON: yaml: line {line}: {msg}")
        }
        None => format!("cannot parse {file}: error converting YAML to JSON: yaml: {msg}"),
    }
}

/// Returns the function-wasm Inputs of the documents: every pipeline step of
/// a Composition whose input is one (and, with a function name, whose
/// functionRef.name matches), and every bare Input document.
fn find_steps(file: &str, docs: &[serde_json::Value], function_name: Option<&str>) -> Vec<Step> {
    let mut steps = Vec::new();
    let str_of = |v: &serde_json::Value, key: &str| -> String {
        v.get(key)
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string()
    };
    for (i, doc) in docs.iter().enumerate() {
        let (api_version, kind) = (str_of(doc, "apiVersion"), str_of(doc, "kind"));
        if api_version == INPUT_API_VERSION && kind == INPUT_KIND {
            if function_name.is_none() {
                let name = doc
                    .get("metadata")
                    .map(|m| str_of(m, "name"))
                    .unwrap_or_default();
                let input = doc.as_object().cloned().unwrap_or_default();
                steps.push(Step {
                    file: file.to_string(),
                    composition: String::new(),
                    document: i,
                    index: -1,
                    name,
                    function: String::new(),
                    input,
                });
            }
            continue;
        }
        if api_version != COMPOSITION_API_VERSION || kind != COMPOSITION_KIND {
            continue;
        }
        let name = doc
            .get("metadata")
            .map(|m| str_of(m, "name"))
            .unwrap_or_default();
        let pipeline = doc
            .pointer("/spec/pipeline")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();
        for (j, entry) in pipeline.iter().enumerate() {
            let Some(input) = entry.get("input").and_then(|v| v.as_object()) else {
                continue;
            };
            if input.get("apiVersion").and_then(|v| v.as_str()) != Some(INPUT_API_VERSION) {
                continue;
            }
            if input.get("kind").and_then(|v| v.as_str()) != Some(INPUT_KIND) {
                continue;
            }
            let function = entry
                .get("functionRef")
                .map(|f| str_of(f, "name"))
                .unwrap_or_default();
            if let Some(want) = function_name
                && function != want
            {
                continue;
            }
            steps.push(Step {
                file: file.to_string(),
                composition: name.clone(),
                document: i,
                index: j as i64,
                name: str_of(entry, "step"),
                function,
                input: input.clone(),
            });
        }
    }
    steps
}

/// Judges steps against one set of ceilings.
struct Validator {
    ceilings: Config,
    engine: Option<Engine>,
    resolver: Option<Resolver>,
    xr: Option<serde_json::Value>,
    policy: Option<OperatorPolicy>,
    cosign_key: bool,
}

impl Validator {
    fn validate(&self, s: Step) -> StepResult {
        let mut r = StepResult {
            file: s.file,
            composition: s.composition,
            document: s.document,
            index: s.index,
            step: s.name,
            function: s.function,
            status: "ok".to_string(),
            ..Default::default()
        };
        macro_rules! refuse {
            ($msg:expr) => {{
                r.status = "refused".to_string();
                r.message = $msg;
                return r;
            }};
        }

        let (input, warnings) = match decode_input(&s.input) {
            Ok(decoded) => decoded,
            Err(e) => refuse!(e),
        };
        r.warnings = warnings;

        // The runtime's admission, verbatim: the compositionPolicy compiled,
        // limits within the ceilings, the module source's shape.
        let admitted = match admission::admit(&input, &self.ceilings) {
            Ok(admitted) => admitted,
            Err(e) => refuse!(e),
        };
        r.details = describe_admitted(&input, admitted.composition.is_some());

        // The module: materialised against the XR when one is given, as the
        // runtime does on every request; otherwise checked for the fence a
        // composite-chosen source requires and reported as such.
        let comp = admitted.composition.as_deref();
        let src = match (&input.module.from.is_empty(), &self.xr) {
            (false, Some(xr)) => {
                let concrete = match crate::from::from_composite(&input.module, comp, Some(xr)) {
                    Ok(concrete) => concrete,
                    Err(e) => refuse!(format!("cannot resolve module: {e}")),
                };
                r.module = format!(
                    "{} (from {})",
                    describe_source(&concrete),
                    input.module.from
                );
                concrete
            }
            (false, None) => {
                if let Err(e) = crate::from::validate_from(&input.module, comp) {
                    refuse!(format!("cannot resolve module: {e}"));
                }
                r.module = describe_source(&input.module);
                r.warnings.extend(self.warnings(&input));
                return r;
            }
            _ => {
                r.module = describe_source(&input.module);
                input.module.clone()
            }
        };
        r.warnings.extend(self.warnings(&input));

        let (Some(resolver), Some(engine)) = (&self.resolver, &self.engine) else {
            return r;
        };
        match src.r#type.as_str() {
            "OCI" => {
                let oci = src
                    .oci
                    .as_ref()
                    .expect("validated: an OCI source has its object");
                if !oci.credentials.is_empty() {
                    r.warnings.push(format!(
                        "module.oci.credentials {:?} is a step Secret this tool cannot read; the module is pulled with the local Docker config instead",
                        oci.credentials
                    ));
                }
                let location = match crate::location::oci_location(&oci.r#ref) {
                    Ok(location) => location,
                    Err(e) => refuse!(format!("cannot resolve module: {e}")),
                };
                let description = format!("oci {}", oci.r#ref);
                // Whether this module must carry a cosign signature is
                // settled before any registry is reached.
                if let Some(policy) = &self.policy
                    && policy.requires_signature(&location)
                    && !self.cosign_key
                {
                    refuse!(format!(
                        "cannot verify module {description}: the operator policy requires a cosign signature, but the runtime has no --cosign-key to verify it"
                    ));
                }
                refuse!(format!(
                    "cannot load module {description}: cannot fetch module: OCI sources are not implemented yet in the Rust runtime"
                ));
            }
            "HTTP" => {
                let http = src
                    .http
                    .as_ref()
                    .expect("validated: an HTTP source has its object");
                let description = format!("http {}", http.url);
                if let (Some(policy), Ok(location)) = (
                    &self.policy,
                    crate::location::http_location("module.http.url", &http.url),
                ) && policy.requires_signature(&location)
                {
                    refuse!(format!(
                        "cannot resolve module: module.http {location:?} requires a cosign signature (operator policy), but only OCI modules can be signature-verified"
                    ));
                }
                refuse!(format!(
                    "cannot load module {description}: cannot fetch module: HTTP sources are not implemented yet in the Rust runtime"
                ));
            }
            _ => {}
        }
        let resolved = match resolver.resolve(&src.path) {
            Ok(resolved) => resolved,
            Err(e) => refuse!(format!("cannot resolve module: {e}")),
        };
        let wasm = match resolver.fetch(&resolved) {
            Ok(wasm) => wasm,
            Err(e) => refuse!(format!(
                "cannot load module {}: cannot fetch module: {e}",
                resolved.description
            )),
        };
        let inspection = match engine.inspect(&wasm) {
            Ok(inspection) => inspection,
            Err(e) => refuse!(format!("cannot load module {}: {e}", resolved.description)),
        };
        if let Some(abi_error) = inspection.abi_error {
            refuse!(format!(
                "cannot load module {}: {abi_error}",
                resolved.description
            ));
        }
        r.resolved = Some(Resolved {
            digest: resolved.digest.clone(),
            size: wasm.len(),
            abi: "v1".to_string(),
            imports: inspection.host_imports,
            manifest: None,
        });

        // The module's manifest: its requests decided by the three layers -
        // with the principal from --xr when one is given - then held against
        // what the layers granted, the checks the runtime makes between load
        // and run.
        let raw = if src.manifest_path.is_empty() {
            Vec::new()
        } else {
            match resolver.read_manifest(&src.manifest_path) {
                Ok(raw) => raw,
                Err(e) => refuse!(format!(
                    "cannot read the manifest of module {}: {e}",
                    resolved.description
                )),
            }
        };
        if raw.is_empty() {
            return r;
        }
        let m = match crate::manifest::Manifest::parse(&raw) {
            Ok(m) => m,
            Err(e) => refuse!(format!(
                "module {} has an invalid manifest: {e}",
                resolved.description
            )),
        };
        if let Some(res) = &mut r.resolved {
            res.manifest = Some(m.clone());
        }
        if m.requires
            .as_ref()
            .and_then(|q| q.egress.as_ref())
            .is_some_and(|e| !e.http.is_empty())
            && !self.cosign_key
        {
            r.warnings.push(
                "the module requires egress but is not signature-verified: no --cosign-key was given"
                    .to_string(),
            );
        }
        let principal = self
            .xr
            .as_ref()
            .map(crate::from::principal_from_composite)
            .unwrap_or_default();
        let caps = match admission::admit_requires(
            m.requires.as_ref(),
            self.policy.as_ref(),
            comp,
            &principal,
        ) {
            Ok(caps) => caps,
            Err(e) => refuse!(format!("module {} {e}", resolved.description)),
        };
        if let Err(e) = m.check(&caps.grants(), input.config.as_ref(), "") {
            refuse!(format!("module {} {e}", resolved.description));
        }
        r
    }

    /// The short fixed list of accepted-but-unwise findings.
    fn warnings(&self, input: &Input) -> Vec<String> {
        let mut out = Vec::new();
        if input.module.r#type == "Path" {
            out.push("module.type Path names a file under the runtime's --module-dir and carries no digest; a cluster Composition should pin an OCI or HTTP source by digest".to_string());
        }
        if let Some(limits) = &input.limits {
            if let Some(t) = &limits.timeout
                && let Ok(timeout) = duration::parse(t)
                && timeout == self.ceilings.timeout
            {
                out.push(format!(
                    "limits.timeout {} equals --module-timeout: it narrows nothing",
                    duration::format(timeout)
                ));
            }
            if let Some(mem) = &limits.memory
                && let Ok(bytes) = quantity::parse(mem)
                && bytes == i128::from(self.ceilings.memory_limit)
            {
                out.push(format!(
                    "limits.memory {mem} equals --module-memory-limit ({}): it narrows nothing",
                    quantity::format_binary_si(self.ceilings.memory_limit)
                ));
            }
        }
        out
    }
}

/// Turns the unstructured Input into the typed one the runtime decodes -
/// strictly first, so a field the runtime would silently ignore becomes a
/// warning naming it, then as the runtime does.
fn decode_input(
    raw: &serde_json::Map<String, serde_json::Value>,
) -> Result<(Input, Vec<String>), String> {
    // The removed v1beta1 fields are refused naming their replacement, so an
    // unported Composition must not read as admitted here.
    if raw.contains_key("policy") {
        return Err("the Input's policy field was removed: fence a module.from source with compositionPolicy instead (Cedar pullModule and spendCredential rules)".to_string());
    }
    if raw.contains_key("sandbox") {
        return Err("the Input's sandbox field was removed: a module requests capabilities through its manifest's requires, granted by the operator's --sandbox-policy-file and narrowed by the Input's compositionPolicy".to_string());
    }
    let mut warnings = Vec::new();
    unknown_fields(raw, "", &mut warnings);
    let input: Input = serde_json::from_value(serde_json::Value::Object(raw.clone()))
        .map_err(|e| format!("cannot decode the Input: {e}"))?;
    Ok((input, warnings))
}

/// Walks the raw Input against the known field names, warning about every
/// field the runtime would silently ignore - keys sorted at each level and
/// depth-first, the order the Go tool's repeated strict decode reports them
/// in (its decoder streams a key-sorted re-marshal of the document).
fn unknown_fields(
    obj: &serde_json::Map<String, serde_json::Value>,
    path: &str,
    warnings: &mut Vec<String>,
) {
    // Object-valued fields with a schema recurse; metadata and config are
    // opaque to the runtime.
    let known: &[&str] = match path {
        "" => &[
            "apiVersion",
            "kind",
            "metadata",
            "module",
            "compositionPolicy",
            "limits",
            "config",
        ],
        "module" => &["type", "oci", "http", "path", "manifestPath", "from"],
        "module.oci" => &["ref", "credentials"],
        "module.http" => &["url", "digest", "manifestURL", "manifestDigest"],
        "limits" => &["timeout", "memory", "concurrency"],
        _ => return,
    };
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    for key in keys {
        if !known.contains(&key.as_str()) {
            warnings.push(format!("unknown field {key:?} is ignored by the runtime"));
            continue;
        }
        if matches!(key.as_str(), "metadata" | "config") {
            continue;
        }
        if let Some(child) = obj[key].as_object() {
            let child_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            unknown_fields(child, &child_path, warnings);
        }
    }
}

/// Names a source the way the runtime's messages do.
fn describe_source(src: &ModuleSource) -> String {
    if !src.from.is_empty() {
        return format!("chosen by the composite resource from {}", src.from);
    }
    if let Some(oci) = &src.oci {
        return format!("oci {}", oci.r#ref);
    }
    if let Some(http) = &src.http {
        return format!("http {}", http.url);
    }
    format!("path {}", src.path)
}

/// Lists what the step was admitted: its limits, and whether a
/// compositionPolicy layer is present.
fn describe_admitted(input: &Input, has_composition: bool) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(l) = &input.limits {
        let mut limits = Vec::new();
        if let Some(t) = &l.timeout
            && let Ok(timeout) = duration::parse(t)
        {
            limits.push(format!("timeout {}", duration::format(timeout)));
        }
        if let Some(mem) = &l.memory {
            limits.push(format!("memory {mem}"));
        }
        if !limits.is_empty() {
            out.push(format!("limits {}", limits.join(" ")));
        }
    }
    if has_composition {
        out.push("compositionPolicy".to_string());
    }
    out
}

/// Renders a size for the text output, as the Go tool's humanBytes does.
fn human_bytes(n: usize) -> String {
    if n >= 1 << 20 {
        return format!("{:.1} MB", n as f64 / f64::from(1u32 << 20));
    }
    if n >= 1 << 10 {
        return format!("{:.1} KB", n as f64 / f64::from(1u32 << 10));
    }
    format!("{n} B")
}
