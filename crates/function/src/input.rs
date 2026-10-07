//! The function's Input (wasm.fn.crossplane.io/v1), deserialized from the
//! request's input struct. The runtime enforces every rule itself
//! (Crossplane never installs a function's Input CRD), in admission.rs; the
//! shape is the 1.x contract (README, "Compatibility").

use serde::{Deserialize, Deserializer};

/// The Input's identity in a Composition step: what `function validate`
/// looks for and `guestfn scaffold composition` writes.
pub const API_VERSION: &str = "wasm.fn.crossplane.io/v1";
pub const KIND: &str = "Input";

/// The Input's apiVersion before 1.0.0: the same fields and rules as v1,
/// accepted throughout 1.x under the README's deprecation policy and
/// removed in 2.0.0.
pub const API_VERSION_V1BETA1: &str = "wasm.fn.crossplane.io/v1beta1";

/// What a v1beta1 document is told: once per request by the runtime, under
/// the step by `function validate`, on the build line by `guestfn build`.
/// One const, so every site states the same sentence (the conformance
/// goldens pin it).
pub const V1BETA1_DEPRECATION: &str = "apiVersion wasm.fn.crossplane.io/v1beta1 is deprecated and is removed in function-wasm 2.0.0; write apiVersion: wasm.fn.crossplane.io/v1 (the same fields)";

/// Whether a document's apiVersion and kind name the Input: the current
/// version, or the deprecated one 1.x still accepts.
pub fn is_input(api_version: &str, kind: &str) -> bool {
    kind == KIND && (api_version == API_VERSION || api_version == API_VERSION_V1BETA1)
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Input {
    /// Read for the deprecation warning only: the runtime never refused an
    /// apiVersion it did not know, and 1.x keeps that.
    pub api_version: String,
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
    /// With from: a composite resource that leaves the field unset chooses
    /// no module, and the step returns the request's desired state unchanged
    /// instead of a fatal result. Read from the Input only.
    pub allow_empty: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_is_named_by_v1_or_the_deprecated_v1beta1() {
        let cases = [
            ((API_VERSION, KIND), true),
            ((API_VERSION_V1BETA1, KIND), true),
            (("wasm.fn.crossplane.io/v2", KIND), false),
            (("wasm.fn.crossplane.io/v1", "Resources"), false),
            (("", KIND), false),
        ];
        for ((api_version, kind), want) in cases {
            assert_eq!(is_input(api_version, kind), want, "{api_version} {kind}");
        }
    }

    #[test]
    fn the_api_version_is_decoded_beside_the_fields() {
        let input: Input = serde_json::from_value(serde_json::json!({
            "apiVersion": API_VERSION_V1BETA1,
            "kind": KIND,
            "module": {"type": "Path", "path": "fn.wasm"},
        }))
        .expect("decode");
        assert_eq!(input.api_version, API_VERSION_V1BETA1);
        assert_eq!(input.module.path, "fn.wasm");
    }
}
