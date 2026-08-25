//! The function's Input (wasm.fn.crossplane.io/v1beta1), deserialized from
//! the request's input struct. The shape mirrors input/v1beta1/input.go; the
//! runtime enforces every rule itself (Crossplane never installs a
//! function's Input CRD), in admission.rs.

use serde::{Deserialize, Deserializer};

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Input {
    pub module: ModuleSource,
    pub composition_policy: String,
    pub limits: Option<Limits>,
    /// Passed to the module verbatim as part of its request input; the
    /// runtime only ever holds it against a manifest's config.schema.
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModuleSource {
    pub r#type: String,
    pub oci: Option<OciSource>,
    pub http: Option<HttpSource>,
    pub path: String,
    pub manifest_path: String,
    pub from: String,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OciSource {
    pub r#ref: String,
    pub credentials: String,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HttpSource {
    pub url: String,
    pub digest: String,
    #[serde(rename = "manifestURL")]
    pub manifest_url: String,
    pub manifest_digest: String,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Limits {
    /// A Go duration string, e.g. "5s".
    pub timeout: Option<String>,
    /// A Kubernetes quantity; YAML authors may write it as a bare number.
    #[serde(deserialize_with = "string_or_number")]
    pub memory: Option<String>,
    pub concurrency: Option<i64>,
}

/// Accepts a JSON string or number and yields its string form, the way a
/// Kubernetes quantity is written either as "128Mi" or as bytes.
fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        String(String),
        Number(serde_json::Number),
    }
    Ok(Option::<Raw>::deserialize(d)?.map(|raw| match raw {
        Raw::String(s) => s,
        Raw::Number(n) => n.to_string(),
    }))
}
