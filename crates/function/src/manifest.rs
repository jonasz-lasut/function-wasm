//! The module manifest - the Rust port of `internal/manifest`: what a module
//! declares about itself (the capabilities it cannot run without, the JSON
//! Schema of its config, its ABI and the oldest runtime it works on). A
//! manifest is a request, never a grant: it can make a run fail earlier and
//! say why, it cannot make a run possible. Refusal strings match the Go
//! runtime except where a message embeds a library's own wording (the JSON
//! decoder, the schema validator), noted in rust/README.md.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::egress_rules::{self, HttpRule};
use crate::sandboxenv::{self, EnvBinding};

/// Bounds the layer and the file: a manifest is a few lines of requirements
/// and a schema, never a document.
pub const MAX_SIZE: usize = 64 << 10;
/// The ABIs this runtime implements: v1 (docs/abi.md, core modules) and v2
/// (docs/abi-v2.md, components). The binary format decides which one a
/// module actually is; the manifest's abi is the declaration checked
/// against it.
pub const SUPPORTED_ABIS: [i64; 2] = [1, 2];

/// What a module declares about itself.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Manifest {
    /// The ABI version the module implements; required, one of
    /// SUPPORTED_ABIS, and it must match the module's binary format.
    pub abi: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
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
    /// inline: no $ref to a URL is followed.
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
}

/// The runtime's own release version, stamped at build time
/// (FUNCTION_WASM_VERSION, set by the release pipeline); empty for a
/// development build, which passes every minRuntime rule - the Go runtime's
/// debug.ReadBuildInfo behaviour.
/// The manifest file a guest project carries.
pub const FILE_NAME: &str = "wasmfn.yaml";

pub fn runtime_version() -> &'static str {
    option_env!("FUNCTION_WASM_VERSION").unwrap_or("")
}

impl Manifest {
    /// Decodes a manifest layer's JSON payload, at most MAX_SIZE. Unknown
    /// top-level fields are ignored, so a module built by a newer guestfn
    /// still loads; an unknown field anywhere under requires is refused, so
    /// a requirement this runtime cannot honour fails closed. The result is
    /// validated.
    /// Loads and validates a wasmfn.yaml file - what guestfn build and
    /// push read. Unknown top-level fields are refused: a typo in the file
    /// the author is editing should not vanish silently.
    pub fn load(path: &std::path::Path) -> Result<Manifest, String> {
        let raw =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let value: serde_json::Value = serde_yaml::from_slice(&raw)
            .map_err(|e| format!("{} is not valid YAML: {e}", path.display()))?;
        if let Some(map) = value.as_object() {
            const KNOWN: [&str; 8] = [
                "abi",
                "name",
                "version",
                "source",
                "description",
                "requires",
                "config",
                "minRuntime",
            ];
            for key in map.keys() {
                if !KNOWN.contains(&key.as_str()) {
                    return Err(format!("{}: unknown field {key:?}", path.display()));
                }
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

    /// Checks the manifest's shape and compiles its schema: abi is one of
    /// SUPPORTED_ABIS, every required egress rule passes the checks a
    /// Composition's rule passes, minRuntime is a semantic version, and
    /// config.schema is a JSON Schema.
    pub fn validate(&self) -> Result<(), String> {
        if !SUPPORTED_ABIS.contains(&self.abi) {
            return Err(format!(
                "abi must be 1 or 2 (this runtime implements ABI v1 and v2), got {}",
                self.abi
            ));
        }
        if let Some(r) = &self.requires {
            if let Some(egress) = &r.egress {
                egress_rules::validate_rules("requires.egress.http", &egress.http)?;
            }
            sandboxenv::validate_bindings("requires.env", &r.env)?;
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
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .should_validate_formats(true)
            .build(schema)
            .map_err(|e| format!("config.schema: {e}"))
    }

    /// One line for a CLI: name and version, then what the module requires
    /// and whether it ships a config schema; empty parts are left out.
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
    /// every requirement must be covered by g, the declared abi must be the
    /// module's actual binary format (module_abi), the runtime must be at
    /// least minRuntime, and config must satisfy the schema. The first miss
    /// is the error, worded for a fatal result the caller prefixes with the
    /// module's name.
    pub fn check(
        &self,
        g: &Grants,
        config: Option<&Value>,
        runtime_version: &str,
        module_abi: u8,
    ) -> Result<(), String> {
        if !SUPPORTED_ABIS.contains(&self.abi) {
            return Err(format!(
                "requires ABI v{}, this runtime implements ABI v1 and v2",
                self.abi
            ));
        }
        if self.abi != i64::from(module_abi) {
            let actual = match module_abi {
                1 => "a core module (ABI v1)",
                _ => "a component (ABI v2)",
            };
            return Err(format!(
                "manifest says abi: {}, but the module is {actual}",
                self.abi
            ));
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

    #[test]
    fn parses_and_summarises() {
        let m = manifest(
            r#"{"abi":1,"name":"greeter","version":"0.1.0","requires":{"egress":{"http":[{"host":"api.example.com","methods":["GET"]}]}}}"#,
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
                r#"{"abi":3}"#,
                "abi must be 1 or 2 (this runtime implements ABI v1 and v2), got 3",
            ),
            (
                r#"{"abi":1,"requires":{"egress":{"http":[{"methods":["GET"]}]}}}"#,
                "requires.egress.http[0] must set exactly one of host and hostPattern",
            ),
            (
                r#"{"abi":1,"minRuntime":"latest"}"#,
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
            Manifest::parse(br#"{"abi":1,"requires":{"sockets":true}}"#)
                .expect_err("strict requires")
                .starts_with("cannot parse manifest requires:")
        );
        assert!(Manifest::parse(br#"{"abi":1,"future":true}"#).is_ok());
    }

    #[test]
    fn check_holds_requirements_against_grants() {
        let m = manifest(
            r#"{"abi":1,"requires":{"filesystem":{"privateTmp":true},"egress":{"http":[{"host":"api.example.com","methods":["GET"]}]}}}"#,
        );
        let full = Grants {
            private_tmp: true,
            http: vec![HttpRule {
                host: "api.example.com".to_string(),
                methods: vec!["GET".to_string()],
                ..Default::default()
            }],
            env: Vec::new(),
        };
        assert!(m.check(&full, None, "", 1).is_ok());
        let none = Grants::default();
        assert_eq!(
            m.check(&none, None, "", 1).expect_err("refuse"),
            "requires egress host api.example.com methods [GET], which was not granted"
        );
    }

    #[test]
    fn check_holds_abi_against_the_binary_format() {
        let v2 = manifest(r#"{"abi":2}"#);
        assert_eq!(
            v2.check(&Grants::default(), None, "", 1)
                .expect_err("refuse"),
            "manifest says abi: 2, but the module is a core module (ABI v1)"
        );
        assert!(v2.check(&Grants::default(), None, "", 2).is_ok());
        let v1 = manifest(r#"{"abi":1}"#);
        assert_eq!(
            v1.check(&Grants::default(), None, "", 2)
                .expect_err("refuse"),
            "manifest says abi: 1, but the module is a component (ABI v2)"
        );
    }

    #[test]
    fn config_schema_validates() {
        let m = manifest(
            r#"{"abi":1,"config":{"schema":{"type":"object","properties":{"greeting":{"type":"string"}}}}}"#,
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
    fn min_runtime_compares() {
        let m = manifest(r#"{"abi":1,"minRuntime":"0.2.0"}"#);
        assert!(m.check(&Grants::default(), None, "(devel)", 1).is_ok());
        assert!(m.check(&Grants::default(), None, "", 1).is_ok());
        assert!(m.check(&Grants::default(), None, "v0.3.0", 1).is_ok());
        assert_eq!(
            m.check(&Grants::default(), None, "v0.1.0", 1)
                .expect_err("refuse"),
            "requires runtime v0.2.0 or newer, this is v0.1.0"
        );
    }
}
