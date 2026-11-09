//! The module manifest: what a module
//! declares about itself (the capabilities it cannot run without, the JSON
//! Schema of its config, its contract and the oldest runtime it works on). A
//! manifest is a request, never a grant: it can make a run fail earlier and
//! say why, it cannot make a run possible. Refusal strings are pinned by the
//! conformance goldens, except where a message embeds a library's own wording
//! (the JSON decoder, the schema validator).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::egress_rules::{self, HttpRule};
use crate::sandboxenv::{self, EnvBinding};

/// Bounds the layer and the file: a manifest is a few lines of requirements
/// and a schema, never a document.
pub const MAX_SIZE: usize = 64 << 10;
/// The kinds of module a manifest describes, the engines that serve them
/// (docs/one-pager-oci-runtime.md): a WebAssembly component, which
/// function-wasm serves, and an OCI image, which function-oci serves. The
/// contract's prefix names the kind.
pub const TYPE_WASM: &str = "wasm";
pub const TYPE_OCI: &str = "oci";

/// The contracts this workspace's engines implement, by kind: the
/// component world of docs/abi-v2.md as `wasm@1.0` (the one WebAssembly
/// contract since function-wasm 1.0.0 removed the core-module ABI), the
/// gRPC contract of an image as `oci@0.1`. From a contract's 1.0 a minor adds what
/// a module may use and a major changes what the engine does with a
/// module; at 0.x a minor may change anything.
pub const WASM_CONTRACTS: [Version; 1] = [Version { major: 1, minor: 0 }];
pub const OCI_CONTRACTS: [Version; 1] = [Version { major: 0, minor: 1 }];

/// A contract's version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// What only an `oci@0.1` manifest says.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OciSettings {
    /// The image the module was built from, pinned by digest
    /// (`ghcr.io/acme/tagger@sha256:…`): the one `guestfn push --image`
    /// appended the manifest layer to, written into the published manifest
    /// as provenance.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub image: String,
    /// The arguments for the function's binary. They replace the image's
    /// command and keep its entrypoint, as a Kubernetes container's `args`
    /// do. A function on a Crossplane SDK usually needs `--insecure`:
    /// its server demands mTLS otherwise, and the host's call stays inside
    /// the VM.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// The contract a module implements, `<engine>@<major>.<minor>`: the
/// engine kind that serves it and the version of that engine's contract.
/// `wasm@1.0` is the component world, `oci@0.1` the gRPC contract of an image.
/// Required. Named `abi` (an integer) through 0.6, replaced with no alias
/// (docs/one-pager-oci-runtime.md, decision 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contract {
    Wasm(Version),
    Oci(Version),
}

/// What a wrong contract says: the shape, and the two this workspace has.
const CONTRACT_SHAPE: &str = "contract must be <engine>@<major>.<minor>: wasm@1.0 (the component world) or oci@0.1 (the gRPC contract)";

impl Contract {
    /// Parses `wasm@1.0`; a version this workspace's engines do not
    /// implement is a parse error too, so a manifest never loads with a
    /// contract nothing serves.
    pub fn parse(s: &str) -> Result<Contract, String> {
        let Some((engine, version)) = s.split_once('@') else {
            return Err(format!("{CONTRACT_SHAPE}, got {s:?}"));
        };
        let Some((major, minor)) = version.split_once('.') else {
            return Err(format!("{CONTRACT_SHAPE}, got {s:?}"));
        };
        let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
            return Err(format!("{CONTRACT_SHAPE}, got {s:?}"));
        };
        let v = Version { major, minor };
        let (contract, known): (Contract, &[Version]) = match engine {
            TYPE_WASM => (Contract::Wasm(v), &WASM_CONTRACTS),
            TYPE_OCI => (Contract::Oci(v), &OCI_CONTRACTS),
            other => {
                return Err(format!(
                    "contract names the engine {other:?}, and the engines are wasm and oci (got {s:?})"
                ));
            }
        };
        if !known.contains(&v) {
            let versions: Vec<String> = known.iter().map(|k| format!("{engine}@{k}")).collect();
            return Err(format!(
                "contract {s} is not one the {engine} engine implements: {}",
                versions.join(", ")
            ));
        }
        Ok(contract)
    }

    /// The engine kind: `wasm` or `oci`.
    pub fn kind(&self) -> &'static str {
        match self {
            Contract::Wasm(_) => TYPE_WASM,
            Contract::Oci(_) => TYPE_OCI,
        }
    }

    pub fn version(&self) -> Version {
        match self {
            Contract::Wasm(v) | Contract::Oci(v) => *v,
        }
    }
}

impl std::fmt::Display for Contract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.kind(), self.version())
    }
}

impl Serialize for Contract {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Contract {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Contract, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Contract;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a contract such as wasm@1.0")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Contract, E> {
                Contract::parse(s).map_err(E::custom)
            }
            fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<Contract, E> {
                Err(E::custom(format!(
                    "{CONTRACT_SHAPE}, got the number {n} (the field was abi, an integer, through function-wasm 0.6)"
                )))
            }
            fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<Contract, E> {
                self.visit_i64(n as i64)
            }
        }
        d.deserialize_any(V)
    }
}

/// What a module declares about itself.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Manifest {
    /// The contract the module implements (`Contract`); required, and
    /// absent in a manifest that has not declared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract: Option<Contract>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// What only an OCI module says: keys specific to a contract are
    /// nested under its engine's name. A `wasm@1.0` manifest must not
    /// carry this section.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oci: Option<OciSettings>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The capabilities the module cannot run without.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<Requires>,
    /// Describes the Input's config block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,
    /// The oldest function-wasm runtime that serves this module.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub min_runtime: String,
    /// What the module composes - advisory (#174): the runtime judges the
    /// module's actual output against the policy layers' composeResource
    /// rules and never reads this; `function validate --resolve` warns where
    /// a declared change would be refused, `guestfn inspect` shows it and
    /// `guestfn scaffold composition` writes the matching permits.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub composes: Vec<Composes>,
}

/// One kind a module declares it requires from Crossplane through its
/// response's requirements, and how it selects it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RequiredResource {
    /// `<group>/<Kind>`, the core group spelled `core`, as the policy's
    /// `Kind::` entity names it: `apiextensions.crossplane.io/EnvironmentConfig`.
    pub kind: String,
    /// `name`, `labels`; empty means both.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by: Vec<String>,
}

/// The ways a selector matches, as a `requires.resources` entry spells them.
pub const RESOURCE_MATCHES: &[&str] = &["name", "labels"];

impl RequiredResource {
    /// The group and kind halves of `kind`.
    pub fn group_and_kind(&self) -> Option<(&str, &str)> {
        self.kind
            .split_once('/')
            .filter(|(g, k)| !g.is_empty() && !k.is_empty())
    }

    /// The matches declared, both when none is.
    pub fn match_kinds(&self) -> Vec<&str> {
        if self.by.is_empty() {
            return RESOURCE_MATCHES.to_vec();
        }
        self.by
            .iter()
            .map(String::as_str)
            .filter(|b| RESOURCE_MATCHES.contains(b))
            .collect()
    }
}

/// One kind a module declares it composes, and how.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Composes {
    /// `<group>/<Kind>`, the core group spelled `core`, as the policy's
    /// `Kind::` entity names it: `policy/PodDisruptionBudget`,
    /// `core/ConfigMap`.
    pub kind: String,
    /// `add`, `modify`, `remove`; empty means all three.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<String>,
}

impl Composes {
    /// The group and kind halves of `kind`.
    pub fn group_and_kind(&self) -> Option<(&str, &str)> {
        self.kind
            .split_once('/')
            .filter(|(g, k)| !g.is_empty() && !k.is_empty())
    }

    /// The changes declared, every one when none is.
    pub fn change_kinds(&self) -> Vec<crate::authz::ChangeKind> {
        use crate::authz::ChangeKind;
        if self.changes.is_empty() {
            return vec![ChangeKind::Add, ChangeKind::Modify, ChangeKind::Remove];
        }
        self.changes
            .iter()
            .filter_map(|c| ChangeKind::parse(c))
            .collect()
    }
}

/// The sandbox capabilities a module needs - its request, the least-trusted
/// layer of the three-layer decision.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Requires {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress: Option<Egress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<Filesystem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvBinding>,
    /// The pipeline-step credentials the module reads whole from its
    /// request; every other step credential is withheld from it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub credentials: Vec<String>,
    /// The resources the module asks Crossplane for through its response's
    /// requirements - advisory (#208), as composes is: the runtime judges
    /// the actual selectors against the policy layers' requireResource
    /// rules after the run and never reads this, so unlike the entries
    /// above it is never granted or refused before the run; `function
    /// validate --resolve` warns where a declared kind would be refused,
    /// `guestfn inspect` shows it and `guestfn scaffold composition` writes
    /// the matching permits.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<RequiredResource>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Egress {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub http: Vec<HttpRule>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Filesystem {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub private_tmp: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// A JSON Schema (draft 2020-12) the Input's config must satisfy,
    /// self-contained: a $ref outside the schema (a URL, a file) is refused
    /// at load, and `pattern` is the regex crate's syntax, so a schema from
    /// a module author costs the runtime no network, file or backtracking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
}

/// What one run was granted - the requirements the policy layers admitted -
/// held against Requires by check.
#[derive(Debug, Default)]
pub struct Grants {
    pub private_tmp: bool,
    pub http: Vec<HttpRule>,
    pub env: Vec<EnvBinding>,
    pub credentials: Vec<String>,
}

/// The runtime's own release version, stamped at build time
/// (GUESTFN_VERSION, set by the release pipeline); empty for a
/// development build, which passes every minRuntime rule.
/// The manifest file a guest project carries.
pub const FILE_NAME: &str = "guestfn.yaml";

pub fn runtime_version() -> &'static str {
    option_env!("GUESTFN_VERSION").unwrap_or("")
}

impl Manifest {
    /// The image an OCI module was built from, empty where none is named.
    pub fn image(&self) -> &str {
        self.oci.as_ref().map_or("", |o| o.image.as_str())
    }

    /// The arguments for an OCI module's binary, empty where none are.
    pub fn args(&self) -> &[String] {
        self.oci.as_ref().map_or(&[], |o| o.args.as_slice())
    }

    /// Decodes a manifest layer's JSON payload, at most MAX_SIZE. Unknown
    /// top-level fields are ignored, so a module built by a newer guestfn
    /// still loads; an unknown field anywhere under requires is refused, so
    /// a requirement this runtime cannot honour fails closed. The result is
    /// validated.
    /// Loads and validates a guestfn.yaml file - what guestfn build and
    /// push read. Unknown top-level fields are refused: a typo in the file
    /// the author is editing should not vanish silently.
    pub fn load(path: &std::path::Path) -> Result<Manifest, String> {
        let raw =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let value: serde_json::Value = serde_saphyr::from_slice(&raw)
            .map_err(|e| format!("{} is not valid YAML: {e}", path.display()))?;
        if let Some(map) = value.as_object() {
            const KNOWN: [&str; 10] = [
                "contract",
                "name",
                "version",
                "oci",
                "source",
                "description",
                "requires",
                "config",
                "minRuntime",
                "composes",
            ];
            for key in map.keys() {
                if !KNOWN.contains(&key.as_str()) {
                    return Err(format!("{}: unknown field {key:?}", path.display()));
                }
            }
            if let Some(oci) = map.get("oci").and_then(|v| v.as_object()) {
                for key in oci.keys() {
                    if !["image", "args"].contains(&key.as_str()) {
                        return Err(format!("{}: unknown field \"oci.{key}\"", path.display()));
                    }
                }
            }
            // The author's file is read strictly here, where a typo can be
            // fixed; a published manifest's composes is advisory, so a field
            // this runtime does not know is forward compatibility there.
            if let Some(composes) = map.get("composes")
                && !composes.is_null()
            {
                strict_composes(composes)
                    .map_err(|e| format!("{}: cannot parse composes: {e}", path.display()))?;
            }
        }
        let json =
            serde_json::to_vec(&value).map_err(|e| format!("cannot encode manifest: {e}"))?;
        Self::parse(&json).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The manifest as the JSON bytes guestfn push publishes as the
    /// artifact's manifest layer.
    pub fn json(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("cannot encode manifest: {e}"))
    }

    pub fn parse(raw: &[u8]) -> Result<Manifest, String> {
        if raw.len() > MAX_SIZE {
            return Err(format!(
                "manifest is {} bytes, the limit is {}",
                raw.len(),
                MAX_SIZE
            ));
        }
        // The contract first, on its own, so its refusal reads without
        // serde's position suffix.
        let value: Value =
            serde_json::from_slice(raw).map_err(|e| format!("cannot parse manifest: {e}"))?;
        match value.get("contract") {
            None | Some(Value::Null) => {}
            Some(Value::String(c)) => {
                Contract::parse(c).map_err(|e| format!("cannot parse manifest: {e}"))?;
            }
            Some(Value::Number(n)) => {
                return Err(format!(
                    "cannot parse manifest: {CONTRACT_SHAPE}, got the number {n} (the field was abi, an integer, through function-wasm 0.6)"
                ));
            }
            Some(other) => {
                return Err(format!(
                    "cannot parse manifest: {CONTRACT_SHAPE}, got {other}"
                ));
            }
        }
        let m: Manifest =
            serde_json::from_slice(raw).map_err(|e| format!("cannot parse manifest: {e}"))?;
        // The strict pass over requires alone, with unknown fields refused.
        #[derive(Deserialize)]
        struct Top {
            #[serde(default)]
            requires: Option<Value>,
        }
        let top: Top =
            serde_json::from_slice(raw).map_err(|e| format!("cannot parse manifest: {e}"))?;
        if let Some(requires) = top.requires
            && !requires.is_null()
        {
            strict_requires(&requires)
                .map_err(|e| format!("cannot parse manifest requires: {e}"))?;
        }
        m.validate()?;
        Ok(m)
    }

    /// Checks the manifest's shape and compiles its schema: contract is
    /// declared (its value was checked as it was read), image is a
    /// digest-pinned reference of an oci module alone, every required
    /// egress rule passes the checks a Composition's rule passes,
    /// minRuntime is a semantic version, and config.schema is a JSON
    /// Schema.
    pub fn validate(&self) -> Result<(), String> {
        let Some(contract) = self.contract else {
            return Err(format!("{CONTRACT_SHAPE}; the manifest declares none"));
        };
        match contract {
            Contract::Wasm(_) => {
                if self.oci.is_some() {
                    return Err(
                        "oci holds an OCI module's settings, which only an oci contract's manifest carries"
                            .to_string(),
                    );
                }
            }
            Contract::Oci(_) => {
                if !self.image().is_empty()
                    && let Err(e) = crate::location::parse_oci_reference(self.image())
                {
                    return Err(format!(
                        "oci.image must be an OCI reference pinned by digest (registry/repository@sha256:…): {e}"
                    ));
                }
            }
        }
        if let Some(r) = &self.requires {
            if let Some(egress) = &r.egress {
                egress_rules::validate_rules("requires.egress.http", &egress.http)?;
            }
            sandboxenv::validate_bindings("requires.env", &r.env)?;
            sandboxenv::validate_credentials("requires.credentials", &r.credentials)?;
            for (i, res) in r.resources.iter().enumerate() {
                if res.group_and_kind().is_none() {
                    return Err(format!(
                        "requires.resources[{i}].kind must be <group>/<Kind> (the core group spelled core), got {:?}",
                        res.kind
                    ));
                }
                for by in &res.by {
                    if !RESOURCE_MATCHES.contains(&by.as_str()) {
                        return Err(format!(
                            "requires.resources[{i}].by must be among name and labels, got {by:?}"
                        ));
                    }
                }
            }
        }
        for (i, c) in self.composes.iter().enumerate() {
            if c.group_and_kind().is_none() {
                return Err(format!(
                    "composes[{i}].kind must be <group>/<Kind> (the core group spelled core), got {:?}",
                    c.kind
                ));
            }
            for change in &c.changes {
                if crate::authz::ChangeKind::parse(change).is_none() {
                    return Err(format!(
                        "composes[{i}].changes must be among add, modify and remove, got {change:?}"
                    ));
                }
            }
        }
        if !self.min_runtime.is_empty() && parse_semver(&canonical(&self.min_runtime)).is_none() {
            return Err(format!(
                "minRuntime {:?} is not a semantic version (e.g. v0.2.0)",
                self.min_runtime
            ));
        }
        if let Some(schema) = self.config_schema() {
            self.compile_schema(schema)?;
        }
        Ok(())
    }

    fn config_schema(&self) -> Option<&Value> {
        self.config
            .as_ref()?
            .schema
            .as_ref()
            .filter(|s| !s.is_null())
    }

    fn compile_schema(&self, schema: &Value) -> Result<jsonschema::Validator, String> {
        // Every $ref the schema itself does not hold goes through the
        // retriever, which refuses it with the runtime's own sentence; the
        // regex engine is the linear-time one, so a schema's pattern cannot
        // backtrack the runtime off its deadline (both the 1.0 audit's F-3).
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .should_validate_formats(true)
            .with_retriever(NoExternalRefs)
            .with_pattern_options(jsonschema::PatternOptions::regex())
            .build(schema)
            .map_err(|e| format!("config.schema: {e}"))
    }

    /// One line for a CLI: name and version, then what the module requires
    /// and whether it ships a config schema; empty parts are left out.
    /// The engine kind the contract names; `wasm` for a manifest that
    /// declares none, which validate refuses.
    pub fn kind(&self) -> &'static str {
        self.contract.map(|c| c.kind()).unwrap_or(TYPE_WASM)
    }

    pub fn summary(&self) -> String {
        let mut head = Vec::new();
        if !self.name.is_empty() {
            head.push(self.name.clone());
        }
        if !self.version.is_empty() {
            head.push(self.version.clone());
        }
        let mut parts = Vec::new();
        let h = head.join(" ");
        if !h.is_empty() {
            parts.push(h);
        }
        if let Some(c @ Contract::Oci(_)) = self.contract {
            parts.push(format!("contract {c}"));
            if !self.image().is_empty() {
                parts.push(format!("image {}", self.image()));
            }
            if !self.args().is_empty() {
                parts.push(format!("args {}", self.args().join(" ")));
            }
        }
        if let Some(r) = &self.requires {
            if let Some(egress) = &r.egress
                && !egress.http.is_empty()
            {
                let hosts: Vec<&str> = egress
                    .http
                    .iter()
                    .map(|r| {
                        if r.host.is_empty() {
                            r.host_pattern.as_str()
                        } else {
                            r.host.as_str()
                        }
                    })
                    .collect();
                parts.push(format!("requires egress {}", hosts.join(" ")));
            }
            if r.filesystem.as_ref().is_some_and(|f| f.private_tmp) {
                parts.push("private /tmp".to_string());
            }
            if !r.env.is_empty() {
                let names: Vec<&str> = r.env.iter().map(|b| b.name.as_str()).collect();
                parts.push(format!("env {}", names.join(" ")));
            }
            if !r.credentials.is_empty() {
                parts.push(format!("credentials {}", r.credentials.join(" ")));
            }
            if !r.resources.is_empty() {
                let kinds: Vec<String> = r
                    .resources
                    .iter()
                    .map(|res| {
                        if res.by.is_empty() {
                            res.kind.clone()
                        } else {
                            format!("{} ({})", res.kind, res.by.join(" "))
                        }
                    })
                    .collect();
                parts.push(format!("resources {}", kinds.join(" ")));
            }
        }
        if !self.composes.is_empty() {
            let kinds: Vec<String> = self
                .composes
                .iter()
                .map(|c| {
                    if c.changes.is_empty() {
                        c.kind.clone()
                    } else {
                        format!("{} ({})", c.kind, c.changes.join(" "))
                    }
                })
                .collect();
            parts.push(format!("composes {}", kinds.join(" ")));
        }
        let mut out = parts.join(", ");
        if self.config_schema().is_some() {
            if !out.is_empty() {
                out += "; ";
            }
            out += "config schema";
        }
        if !self.min_runtime.is_empty() {
            if !out.is_empty() {
                out += "; ";
            }
            out += &format!("runtime {} or newer", canonical(&self.min_runtime));
        }
        out
    }

    /// Holds the manifest against what one run was granted - narrowing only:
    /// every requirement must be covered by g, the runtime must be at least
    /// minRuntime, and config must satisfy the schema. The first miss is the
    /// error, worded for a fatal result the caller prefixes with the
    /// module's name.
    /// Holds the manifest's type to the package serving it (`served`: the
    /// engine's kind), so a step pointed at the wrong package fails with
    /// the other package's name rather than at the engine; and, on the OCI
    /// runtime, refuses what its sandbox does not offer yet (egress, until
    /// the record's proxy phase). Worded for a fatal result the caller
    /// prefixes with the module's name.
    pub fn check_type(&self, served: &str) -> Result<(), String> {
        match (self.kind(), served) {
            (TYPE_OCI, TYPE_WASM) => Err(
                "is an oci@0.1 module, which function-wasm does not serve; the OCI runtime is function-oci (docs/one-pager-oci-runtime.md)".to_string(),
            ),
            (TYPE_WASM, TYPE_OCI) => Err(
                "is a WebAssembly module (wasm@1.0), which function-oci does not serve; function-wasm serves it".to_string(),
            ),
            (TYPE_OCI, TYPE_OCI)
                if self
                    .requires
                    .as_ref()
                    .and_then(|r| r.egress.as_ref())
                    .is_some_and(|e| !e.http.is_empty()) =>
            {
                Err(
                    "requires egress (requires.egress.http), which function-oci does not serve yet: the VM has no network device (docs/one-pager-oci-runtime.md)".to_string(),
                )
            }
            _ => Ok(()),
        }
    }

    pub fn check(
        &self,
        g: &Grants,
        config: Option<&Value>,
        runtime_version: &str,
    ) -> Result<(), String> {
        if self.kind() == TYPE_OCI {
            return Ok(());
        }
        if let Some(r) = &self.requires {
            if let Some(egress) = &r.egress {
                for required in &egress.http {
                    if !covered(required, &g.http) {
                        return Err(format!(
                            "requires egress {}, which was not granted",
                            describe_rule(required)
                        ));
                    }
                }
            }
            if r.filesystem.as_ref().is_some_and(|f| f.private_tmp) && !g.private_tmp {
                return Err("requires a private /tmp (requires.filesystem.privateTmp), which was not granted".to_string());
            }
            for b in &r.env {
                if !g.env.contains(b) {
                    return Err(format!(
                        "requires env {} from credential {:?} key {:?}, which was not granted",
                        b.name, b.from_credential.name, b.from_credential.key
                    ));
                }
            }
            for (i, name) in r.credentials.iter().enumerate() {
                if !g.credentials.contains(name) {
                    return Err(format!(
                        "requires credential {name:?} (requires.credentials[{i}]), which was not granted"
                    ));
                }
            }
        }
        if !self.min_runtime.is_empty()
            && !runtime_version.is_empty()
            && runtime_version != "(devel)"
        {
            let have = canonical(runtime_version);
            if let (Some(have_v), Some(want_v)) = (
                parse_semver(&have),
                parse_semver(&canonical(&self.min_runtime)),
            ) && have_v < want_v
            {
                return Err(format!(
                    "requires runtime {} or newer, this is {have}",
                    canonical(&self.min_runtime)
                ));
            }
        }
        self.validate_config(config)
    }

    /// Holds config against config.schema alone; fine without a schema, and
    /// an absent config validates as an empty object.
    pub fn validate_config(&self, config: Option<&Value>) -> Result<(), String> {
        let Some(schema) = self.config_schema() else {
            return Ok(());
        };
        let validator = self.compile_schema(schema)?;
        let empty = Value::Object(serde_json::Map::new());
        let instance = config.unwrap_or(&empty);
        if let Some(err) = validator.iter_errors(instance).next() {
            let pointer = err.instance_path().to_string();
            let pointer = if pointer.is_empty() {
                "/".to_string()
            } else {
                pointer
            };
            return Err(format!(
                "config does not match the module's schema: {pointer}: {err}"
            ));
        }
        Ok(())
    }
}

/// The retriever a config.schema is compiled with: a reference the schema
/// does not resolve within itself is refused, whatever its scheme, so a
/// manifest never makes the runtime fetch a URL or read a file.
struct NoExternalRefs;

impl jsonschema::Retrieve for NoExternalRefs {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err(format!("$ref {uri} is external; config.schema must be self-contained").into())
    }
}

/// The strict decode of the requires block: mirror structs refusing unknown
/// fields, so a requirement this runtime cannot honour fails closed rather
/// than being silently dropped.
fn strict_requires(v: &Value) -> Result<(), String> {
    #[derive(Deserialize)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    #[derive(Default)]
    struct StrictRequires {
        egress: Option<StrictEgress>,
        filesystem: Option<StrictFilesystem>,
        env: Vec<StrictBinding>,
        credentials: Vec<String>,
        resources: Vec<StrictResource>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    struct StrictResource {
        kind: String,
        by: Vec<String>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    struct StrictEgress {
        http: Vec<StrictRule>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    struct StrictRule {
        host: String,
        host_pattern: String,
        methods: Vec<String>,
        path_prefix: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    struct StrictFilesystem {
        private_tmp: bool,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    struct StrictBinding {
        name: String,
        from_credential: StrictCredentialKey,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    struct StrictCredentialKey {
        name: String,
        key: String,
    }
    let _: StrictRequires = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    Ok(())
}

/// The strict decode of the composes declaration for the author's own file.
fn strict_composes(v: &Value) -> Result<(), String> {
    #[derive(Deserialize, Default)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    struct StrictComposes {
        kind: String,
        changes: Vec<String>,
    }
    let _: Vec<StrictComposes> = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    Ok(())
}

/// Whether one required rule is admitted by a granted rule: the same host,
/// or a granted pattern covering the required host or pattern; the required
/// methods among the granted; the granted path prefix a prefix of the
/// required one.
fn covered(required: &HttpRule, granted: &[HttpRule]) -> bool {
    granted.iter().any(|g| {
        host_covered(required, g)
            && methods_covered(&required.methods, &g.methods)
            && (g.path_prefix.is_empty() || required.path_prefix.starts_with(&g.path_prefix))
    })
}

fn host_covered(required: &HttpRule, granted: &HttpRule) -> bool {
    if !required.host.is_empty() {
        if !granted.host.is_empty() {
            return granted
                .host
                .trim_end_matches('.')
                .eq_ignore_ascii_case(required.host.trim_end_matches('.'));
        }
        return egress_rules::pattern_covers(&granted.host_pattern, &required.host);
    }
    // A granted exact host never covers a pattern.
    !granted.host_pattern.is_empty()
        && egress_rules::pattern_under(&required.host_pattern, &granted.host_pattern)
}

fn methods_covered(required: &[String], granted: &[String]) -> bool {
    required
        .iter()
        .all(|m| granted.iter().any(|g| g.eq_ignore_ascii_case(m)))
}

/// Renders a rule for a refusal: "host api.example.com methods [GET]
/// pathPrefix /v1/".
fn describe_rule(r: &HttpRule) -> String {
    let mut out = if r.host.is_empty() {
        format!("hostPattern {}", r.host_pattern)
    } else {
        format!("host {}", r.host)
    };
    out += &format!(" methods [{}]", r.methods.join(" "));
    if !r.path_prefix.is_empty() {
        out += &format!(" pathPrefix {}", r.path_prefix);
    }
    out
}

/// Gives a version the leading v its comparison wants.
fn canonical(v: &str) -> String {
    if v.is_empty() || v.starts_with('v') {
        return v.to_string();
    }
    format!("v{v}")
}

/// A minimal semantic version: (major, minor, patch, has_prerelease) - a
/// version with a prerelease sorts before the same version without one.
fn parse_semver(v: &str) -> Option<(u64, u64, u64, bool)> {
    let v = v.strip_prefix('v')?;
    let (core, pre) = match v.split_once(['-', '+']) {
        Some((core, _)) => (core, v.contains('-')),
        None => (v, false),
    };
    let mut parts = core.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    // Invert has_prerelease so tuple ordering puts a prerelease first.
    Some((major, minor, patch, !pre))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(raw: &str) -> Manifest {
        Manifest::parse(raw.as_bytes()).expect("parse")
    }

    /// The contract names the engine and its version, an oci one takes an
    /// image, and each package serves its own kind alone.
    #[test]
    fn the_contract_selects_the_engine_and_the_package() {
        let wasm = manifest(r#"{"contract":"wasm@1.0","name":"greeter"}"#);
        assert_eq!(wasm.kind(), TYPE_WASM);
        assert_eq!(
            wasm.contract,
            Some(Contract::Wasm(Version { major: 1, minor: 0 }))
        );
        assert!(wasm.validate().is_ok());
        assert_eq!(
            String::from_utf8(wasm.json().unwrap()).unwrap(),
            r#"{"contract":"wasm@1.0","name":"greeter"}"#
        );
        let oci = manifest(
            r#"{"contract":"oci@0.1","name":"tagger","version":"0.3.0","oci":{"image":"ghcr.io/acme/tagger@sha256:b66b70ad00384aa91b72f1848e96a6a29623c66b7cb8ead15c6eea807a24f744"}}"#,
        );
        assert_eq!(oci.kind(), TYPE_OCI);
        assert!(oci.validate().is_ok());
        assert_eq!(
            oci.summary(),
            "tagger 0.3.0, contract oci@0.1, image ghcr.io/acme/tagger@sha256:b66b70ad00384aa91b72f1848e96a6a29623c66b7cb8ead15c6eea807a24f744"
        );
        assert_eq!(
            oci.check_type(TYPE_WASM).unwrap_err(),
            "is an oci@0.1 module, which function-wasm does not serve; the OCI runtime is function-oci (docs/one-pager-oci-runtime.md)"
        );
        assert!(oci.check_type(TYPE_OCI).is_ok());
        assert_eq!(
            wasm.check_type(TYPE_OCI).unwrap_err(),
            "is a WebAssembly module (wasm@1.0), which function-oci does not serve; function-wasm serves it"
        );
        assert!(wasm.check_type(TYPE_WASM).is_ok());
        let egress = manifest(
            r#"{"contract":"oci@0.1","requires":{"egress":{"http":[{"host":"api.example.com","methods":["GET"]}]}}}"#,
        );
        assert!(egress.check_type(TYPE_OCI).unwrap_err().starts_with(
            "requires egress (requires.egress.http), which function-oci does not serve yet"
        ));
        let err = |raw: &str| Manifest::parse(raw.as_bytes()).unwrap_err();
        assert_eq!(
            err(r#"{"contract":"oci@2.0"}"#),
            "cannot parse manifest: contract oci@2.0 is not one the oci engine implements: oci@0.1"
        );
        assert_eq!(
            err(r#"{"contract":"wasm@0.9"}"#),
            "cannot parse manifest: contract wasm@0.9 is not one the wasm engine implements: wasm@1.0"
        );
        assert_eq!(
            err(r#"{"contract":"jar@1.0"}"#),
            "cannot parse manifest: contract names the engine \"jar\", and the engines are wasm and oci (got \"jar@1.0\")"
        );
        assert!(
            err(r#"{"contract":"wasm"}"#)
                .starts_with("cannot parse manifest: contract must be <engine>@<major>.<minor>")
        );
        assert!(err(r#"{"contract":2}"#).contains("got the number 2 (the field was abi"));
        assert!(err(r#"{"name":"greeter"}"#).ends_with("; the manifest declares none"));
        // The image is the OCI module's, pinned, and no WebAssembly
        // module's.
        assert!(
            err(r#"{"contract":"oci@0.1","oci":{"image":"ghcr.io/acme/tagger:v1"}}"#)
                .starts_with("oci.image must be an OCI reference pinned by digest")
        );
        assert_eq!(
            err(
                r#"{"contract":"wasm@1.0","oci":{"image":"ghcr.io/acme/tagger@sha256:b66b70ad00384aa91b72f1848e96a6a29623c66b7cb8ead15c6eea807a24f744"}}"#
            ),
            "oci holds an OCI module's settings, which only an oci contract's manifest carries"
        );
    }

    #[test]
    fn parses_and_summarises() {
        let m = manifest(
            r#"{"contract":"wasm@1.0","name":"greeter","version":"0.1.0","requires":{"egress":{"http":[{"host":"api.example.com","methods":["GET"]}]}}}"#,
        );
        assert_eq!(
            m.summary(),
            "greeter 0.1.0, requires egress api.example.com"
        );
    }

    #[test]
    fn refuses_with_go_strings() {
        let cases: &[(&str, &str)] = &[
            (
                r#"{"contract":"wasm@3.0"}"#,
                "cannot parse manifest: contract wasm@3.0 is not one the wasm engine implements: wasm@1.0",
            ),
            // A manifest of the removed ABI, as 0.6 wrote it: refused by its
            // declaration, so a v1 artifact's author reads the fix before
            // the module's own refusal.
            (
                r#"{"abi":1}"#,
                "contract must be <engine>@<major>.<minor>: wasm@1.0 (the component world) or oci@0.1 (the gRPC contract); the manifest declares none",
            ),
            (
                r#"{"contract":"wasm@1.0","requires":{"egress":{"http":[{"methods":["GET"]}]}}}"#,
                "requires.egress.http[0] must set exactly one of host and hostPattern",
            ),
            (
                r#"{"contract":"wasm@1.0","minRuntime":"latest"}"#,
                r#"minRuntime "latest" is not a semantic version (e.g. v0.2.0)"#,
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(
                &Manifest::parse(raw.as_bytes()).expect_err(raw),
                want,
                "{raw}"
            );
        }
        // An unknown field under requires fails closed; an unknown top-level
        // field is forward compatibility.
        assert!(
            Manifest::parse(br#"{"contract":"wasm@1.0","requires":{"sockets":true}}"#)
                .expect_err("strict requires")
                .starts_with("cannot parse manifest requires:")
        );
        assert!(Manifest::parse(br#"{"contract":"wasm@1.0","future":true}"#).is_ok());
        // composes is validated in shape, and read by summary.
        let composing = Manifest::parse(
            br#"{"contract":"wasm@1.0","name":"pdb-addon","composes":[{"kind":"policy/PodDisruptionBudget","changes":["add"]},{"kind":"apps/Deployment"}]}"#,
        )
        .expect("composes");
        assert_eq!(
            composing.summary(),
            "pdb-addon, composes policy/PodDisruptionBudget (add) apps/Deployment"
        );
        assert_eq!(
            composing.composes[1].change_kinds().len(),
            3,
            "no changes declared means every change"
        );
        assert_eq!(
            Manifest::parse(br#"{"contract":"wasm@1.0","composes":[{"kind":"Deployment"}]}"#)
                .unwrap_err(),
            r#"composes[0].kind must be <group>/<Kind> (the core group spelled core), got "Deployment""#
        );
        assert_eq!(
            Manifest::parse(
                br#"{"contract":"wasm@1.0","composes":[{"kind":"apps/Deployment","changes":["patch"]}]}"#
            )
            .unwrap_err(),
            r#"composes[0].changes must be among add, modify and remove, got "patch""#
        );
        // requires.resources is validated in shape, strictly decoded like
        // the rest of requires, and read by summary.
        let requiring = Manifest::parse(
            br#"{"contract":"wasm@1.0","name":"policy-gate","requires":{"resources":[{"kind":"apiextensions.crossplane.io/EnvironmentConfig","by":["name"]},{"kind":"core/ConfigMap"}]}}"#,
        )
        .expect("resources");
        assert_eq!(
            requiring.summary(),
            "policy-gate, resources apiextensions.crossplane.io/EnvironmentConfig (name) core/ConfigMap"
        );
        assert_eq!(
            requiring.requires.as_ref().expect("requires").resources[1].match_kinds(),
            ["name", "labels"],
            "no by declared means both"
        );
        assert_eq!(
            Manifest::parse(
                br#"{"contract":"wasm@1.0","requires":{"resources":[{"kind":"Secret"}]}}"#
            )
            .unwrap_err(),
            r#"requires.resources[0].kind must be <group>/<Kind> (the core group spelled core), got "Secret""#
        );
        assert_eq!(
            Manifest::parse(
                br#"{"contract":"wasm@1.0","requires":{"resources":[{"kind":"core/Secret","by":["uid"]}]}}"#
            )
            .unwrap_err(),
            r#"requires.resources[0].by must be among name and labels, got "uid""#
        );
        assert!(
            Manifest::parse(
                br#"{"contract":"wasm@1.0","requires":{"resources":[{"kind":"core/Secret","namespace":"x"}]}}"#
            )
            .expect_err("strict resources")
            .starts_with("cannot parse manifest requires:")
        );
    }

    #[test]
    fn check_holds_requirements_against_grants() {
        let m = manifest(
            r#"{"contract":"wasm@1.0","requires":{"filesystem":{"privateTmp":true},"egress":{"http":[{"host":"api.example.com","methods":["GET"]}]}}}"#,
        );
        let full = Grants {
            private_tmp: true,
            http: vec![HttpRule {
                host: "api.example.com".to_string(),
                methods: vec!["GET".to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(m.check(&full, None, "").is_ok());
        let none = Grants::default();
        assert_eq!(
            m.check(&none, None, "").expect_err("refuse"),
            "requires egress host api.example.com methods [GET], which was not granted"
        );
    }

    #[test]
    fn required_credentials_parse_summarise_and_check() {
        let m = manifest(
            r#"{"contract":"wasm@1.0","name":"team-tags","requires":{"env":[{"name":"API_TOKEN","fromCredential":{"name":"apikeys","key":"token"}}],"credentials":["cmdb","db"]}}"#,
        );
        assert_eq!(m.summary(), "team-tags, env API_TOKEN, credentials cmdb db");
        // Published and printed as declared.
        let json: Value = serde_json::from_slice(&m.json().expect("json")).expect("decode");
        assert_eq!(
            json["requires"]["credentials"],
            serde_json::json!(["cmdb", "db"])
        );
        let granted = Grants {
            env: m.requires.as_ref().expect("requires").env.clone(),
            credentials: vec!["cmdb".to_string(), "db".to_string()],
            ..Default::default()
        };
        assert!(m.check(&granted, None, "").is_ok());
        let partly = Grants {
            credentials: vec!["cmdb".to_string()],
            ..granted
        };
        assert_eq!(
            m.check(&partly, None, "").expect_err("refuse"),
            r#"requires credential "db" (requires.credentials[1]), which was not granted"#
        );
        // Names are checked like an env binding's credential, and each is
        // required once.
        let cases: &[(&str, &str)] = &[
            (
                r#"{"contract":"wasm@1.0","requires":{"credentials":[""]}}"#,
                "requires.credentials[0] must not be empty",
            ),
            (
                r#"{"contract":"wasm@1.0","requires":{"credentials":["cmdb","cmdb"]}}"#,
                r#"requires.credentials[1]: credential "cmdb" is already required by requires.credentials[0]"#,
            ),
        ];
        for (raw, want) in cases {
            assert_eq!(
                &Manifest::parse(raw.as_bytes()).expect_err(raw),
                want,
                "{raw}"
            );
        }
        // Not a list of names: the strict decode refuses it.
        assert!(
            Manifest::parse(
                br#"{"contract":"wasm@1.0","requires":{"credentials":[{"name":"cmdb"}]}}"#
            )
            .expect_err("strict requires")
            .starts_with("cannot parse manifest")
        );
    }

    #[test]
    fn config_schema_validates() {
        let m = manifest(
            r#"{"contract":"wasm@1.0","config":{"schema":{"type":"object","properties":{"greeting":{"type":"string"}}}}}"#,
        );
        assert!(
            m.validate_config(Some(&serde_json::json!({"greeting": "hi"})))
                .is_ok()
        );
        let err = m
            .validate_config(Some(&serde_json::json!({"greeting": 3})))
            .expect_err("refuse");
        assert!(
            err.starts_with("config does not match the module's schema: /greeting: "),
            "{err}"
        );
        assert!(m.validate_config(None).is_ok());
    }

    #[test]
    fn config_schema_refuses_external_refs() {
        for uri in [
            "file:///etc/hosts",
            "http://127.0.0.1:1/schema.json",
            "https://example.com/schema.json",
        ] {
            let err = Manifest::parse(
                format!(r#"{{"contract":"wasm@1.0","config":{{"schema":{{"$ref":"{uri}"}}}}}}"#)
                    .as_bytes(),
            )
            .expect_err("refuse");
            assert!(err.starts_with("config.schema: "), "{err}");
            assert!(
                err.contains(&format!(
                    "$ref {uri} is external; config.schema must be self-contained"
                )),
                "{err}"
            );
        }
        // A reference inside the schema never reaches the retriever.
        let m = manifest(
            r##"{"contract":"wasm@1.0","config":{"schema":{"$defs":{"s":{"type":"string"}},"type":"object","properties":{"greeting":{"$ref":"#/$defs/s"}}}}}"##,
        );
        assert!(
            m.validate_config(Some(&serde_json::json!({"greeting": "hi"})))
                .is_ok()
        );
        assert!(
            m.validate_config(Some(&serde_json::json!({"greeting": 3})))
                .is_err()
        );
    }

    #[test]
    fn config_schema_pattern_is_linear_time() {
        // The regex crate has no look-around or backreferences, so a
        // pattern that needs backtracking is refused at load rather than run.
        let err = Manifest::parse(
            r#"{"contract":"wasm@1.0","config":{"schema":{"type":"string","pattern":"a(?=b)"}}}"#
                .as_bytes(),
        )
        .expect_err("refuse");
        assert!(err.starts_with("config.schema: "), "{err}");
        let m = manifest(
            r#"{"contract":"wasm@1.0","config":{"schema":{"type":"string","pattern":"^[a-z]+$"}}}"#,
        );
        assert!(m.validate_config(Some(&serde_json::json!("hi"))).is_ok());
        assert!(m.validate_config(Some(&serde_json::json!("Hi"))).is_err());
    }

    #[test]
    fn min_runtime_compares() {
        let m = manifest(r#"{"contract":"wasm@1.0","minRuntime":"0.2.0"}"#);
        assert!(m.check(&Grants::default(), None, "(devel)").is_ok());
        assert!(m.check(&Grants::default(), None, "").is_ok());
        assert!(m.check(&Grants::default(), None, "v0.3.0").is_ok());
        assert_eq!(
            m.check(&Grants::default(), None, "v0.1.0")
                .expect_err("refuse"),
            "requires runtime v0.2.0 or newer, this is v0.1.0"
        );
    }
}
