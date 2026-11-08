//! The function's Input (guestfn.crossplane.io/v1), deserialized from the
//! request's input struct. The runtime enforces every rule itself
//! (Crossplane never installs a function's Input CRD), in admission.rs; the
//! shape is the contract (README, "Compatibility"). The CRD under
//! package/wasm/input/ is generated from these types (crd.rs) for crossplane
//! beta validate and editors, so a field's doc comment is its description
//! there.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

/// The Input's identity in a Composition step: what `function validate`
/// looks for and `guestfn scaffold composition` writes.
pub const API_VERSION: &str = "guestfn.crossplane.io/v1";
pub const KIND: &str = "Input";

/// The group the Input had through 0.6, before the OCI runtime (#239) made
/// the name generic. Its two versions, v1 and v1beta1, had the same fields
/// as today's. Nothing serves them: `function validate` still finds such a
/// step and admission refuses it with `renamed_group`, so a Composition
/// written for 0.6 fails with the new name instead of being skipped.
pub const LEGACY_GROUP: &str = "wasm.fn.crossplane.io";

/// Whether an apiVersion is one of the legacy group's.
pub fn is_legacy_group(api_version: &str) -> bool {
    api_version
        .strip_prefix(LEGACY_GROUP)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// The refusal a legacy-group Input gets, once per request by the runtime,
/// under the step by `function validate`, on the build line by `guestfn
/// build`. One function, so every site states the same sentence (the
/// conformance goldens pin it).
pub fn renamed_group(api_version: &str) -> String {
    format!(
        "apiVersion {api_version} is no longer served; write apiVersion: {API_VERSION} (the same fields)"
    )
}

/// Whether a document's apiVersion and kind name the Input: the current
/// version, or the legacy group that admission refuses by name.
pub fn is_input(api_version: &str, kind: &str) -> bool {
    kind == KIND && (api_version == API_VERSION || is_legacy_group(api_version))
}

/// What a Composition step hands the runtime: which module runs, what the
/// composition author's own policy layer permits, what a run may consume,
/// and what the module receives. What a module may do beyond the default
/// sandbox is decided per capability by three AND-combined layers
/// (docs/one-pager-three-layer-authz.md): the module's manifest requests
/// it, the Input's compositionPolicy permits it, and the operator's
/// --sandbox-policy-file permits it.
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[schemars(extend("x-kubernetes-validations" = [{
    "message": "module.from with type OCI or HTTP requires a compositionPolicy permitting pullModule",
    "rule": "!has(self.module.from) || self.module.type == 'Path' || (has(self.compositionPolicy) && size(self.compositionPolicy) > 0)"
}]))]
pub struct Input {
    /// The versioned schema of this object: guestfn.crossplane.io/v1. The
    /// runtime refuses the group it had through 0.6 by name and serves no
    /// other.
    pub api_version: String,
    /// Locates the module to run.
    pub module: ModuleSource,
    /// The composition author's own Cedar policy layer, as policy text over
    /// the same schema as the operator's --sandbox-policy-file (actions
    /// pullModule, spendCredential, grantEgress, usePrivateTmp, setEnv,
    /// composeResource, requireResource; a Request principal carrying
    /// namespace and xrKind; Repository, HostPattern, Capability,
    /// Credential, Kind and Group entities). It is AND-combined with the
    /// module's manifest and the operator's policy, so it can only narrow.
    /// Two regimes: a sandbox action it scopes no rule for is not narrowed
    /// (the operator and the manifest decide alone), while a module the
    /// composite resource chooses (module.from) is refused unless a
    /// pullModule permit matches its repository, and may spend a step
    /// credential only where a spendCredential permit matches. Read from
    /// the Input only, never through module.from, so an XR author may pick
    /// the module, not widen the policy. Malformed Cedar is a fatal result
    /// at admission.
    pub composition_policy: String,
    /// Narrows what this step's run may consume below the runtime's
    /// ceilings (--module-timeout, --module-memory-limit). Asking for more
    /// than a ceiling is a fatal result naming both. Read from the Input
    /// only.
    pub limits: Option<Limits>,
    /// Passed to the module verbatim as part of its request input. The
    /// runtime does not interpret it; it only ever holds it against the
    /// module manifest's config.schema. Non-secret module configuration
    /// belongs here: the module's environment is its manifest's
    /// requires.env credential bindings, never Input-authored values.
    #[schemars(schema_with = "crate::crd::preserve_unknown_fields")]
    pub config: Option<serde_json::Value>,
}

/// Where the module comes from. The Composition always chooses the kind;
/// with from, the composite resource chooses the instance.
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[schemars(extend("x-kubernetes-validations" = [
    {"message": "type OCI needs exactly one of oci and from, and oci is only allowed with type OCI",
     "rule": "self.type == 'OCI' ? (has(self.oci) != has(self.from)) : !has(self.oci)"},
    {"message": "type HTTP needs exactly one of http and from, and http is only allowed with type HTTP",
     "rule": "self.type == 'HTTP' ? (has(self.http) != has(self.from)) : !has(self.http)"},
    {"message": "type Path needs exactly one of path and from, and path is only allowed with type Path",
     "rule": "self.type == 'Path' ? (has(self.path) != has(self.from)) : !has(self.path)"},
    {"message": "manifestPath is only allowed with type Path",
     "rule": "!has(self.manifestPath) || self.type == 'Path'"},
    {"message": "allowEmpty is only allowed with from",
     "rule": "!has(self.allowEmpty) || !self.allowEmpty || has(self.from)"}
]))]
pub struct ModuleSource {
    /// The kind of source: OCI (an artifact in a registry, the oci object),
    /// HTTP (a URL, the http object) or Path (a file under the runtime's
    /// --module-dir, path). function-oci serves OCI alone: an OCI module is
    /// an image pulled from a registry, never a file or a URL.
    #[schemars(extend("enum" = ["OCI", "HTTP", "Path"]))]
    pub r#type: String,
    /// Pulls the module from an OCI registry. Set with type OCI, unless
    /// from names the composite resource field holding the object.
    pub oci: Option<OciSource>,
    /// Downloads the module from a URL. Set with type HTTP, unless from
    /// names the composite resource field holding the object.
    pub http: Option<HttpSource>,
    /// A module file relative to the directory the function was started
    /// with (--module-dir); refused when that flag is unset. Meant for
    /// local rendering and volume-mounted modules; it carries no digest.
    /// Set with type Path, unless from names the composite resource field
    /// holding the string.
    pub path: String,
    /// Optionally names the module's guestfn.yaml manifest file, relative to
    /// --module-dir: the request layer for a Path source
    /// (docs/one-pager-three-layer-authz.md), letting a Path module declare
    /// the capabilities it needs like an OCI artifact does. Valid only with
    /// type Path; without it a Path source carries no manifest and gets
    /// only the default sandbox. Read from the Input only, never through
    /// module.from: an XR author chooses the module file, not what its
    /// manifest requests.
    pub manifest_path: String,
    /// A field of the observed composite resource, under spec or status,
    /// that holds the source type names: an {ref, credentials} object for
    /// OCI, an {url, digest} object for HTTP, a string for Path; for
    /// example "status.module". The value is read on every request, so each
    /// composite resource can pick its own module; the Input's
    /// compositionPolicy fences what it may pick (pullModule) and spend
    /// (spendCredential). Nothing but the source is read from the composite
    /// resource: compositionPolicy and limits are the Composition's.
    #[schemars(regex(pattern = r"^(spec|status)\..+"))]
    pub from: String,
    /// With from, lets the composite resource leave the field unset (absent
    /// or null): the step then runs no module and returns the request's
    /// desired state and context unchanged, a no-op the pipeline continues
    /// past, instead of a fatal result naming the field. A field that is
    /// set is read, fenced and run exactly as without it. Read from the
    /// Input only, so a composite resource may leave a step empty only
    /// where the Composition allows it. Refused without from: a static
    /// source always resolves.
    pub allow_empty: bool,
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct OciSource {
    /// The artifact reference pinned to its manifest digest,
    /// registry/repository@sha256:<hex>, as guestfn push prints it. The
    /// manifest digest pins the module: the manifest names its layer's
    /// digest and every fetch is verified along that chain, so it also
    /// addresses the module in the caches. A tag alone is not accepted:
    /// tags can be moved, and the runtime resolves nothing at request time.
    /// registry/repository:tag@sha256:<hex> is fine: the digest is what is
    /// fetched, the tag is human-readable context.
    #[schemars(regex(pattern = r"^[^@\s]+@sha256:[a-f0-9]{64}$"))]
    pub r#ref: String,
    /// A credential of the pipeline step (a Secret) used to pull the
    /// artifact. The Secret carries either a .dockerconfigjson key or
    /// username and password keys. Without it the function's own Docker
    /// config (DOCKER_CONFIG) and anonymous access are tried. An object read
    /// through module.from may name credentials only where the Input's
    /// compositionPolicy permits spendCredential for them on the ref's
    /// repository: the composite resource's author would otherwise choose
    /// the registry host a step credential is sent to.
    pub credentials: String,
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[schemars(extend("x-kubernetes-validations" = [{
    "message": "manifestURL and manifestDigest must be set together",
    "rule": "has(self.manifestURL) == has(self.manifestDigest)"
}]))]
pub struct HttpSource {
    /// URL of the module.
    #[schemars(regex(pattern = r"^https?://"))]
    pub url: String,
    /// The sha256 of the module, sha256:<hex>; the download is verified
    /// against it and it addresses the module in the caches.
    #[schemars(regex(pattern = r"^sha256:[a-f0-9]{64}$"))]
    pub digest: String,
    /// Optionally locates the module's guestfn.yaml manifest, served over
    /// HTTP(S) beside the module: the module's request layer, authored by
    /// the module author (docs/one-pager-three-layer-authz.md). Without it
    /// an HTTP source carries no manifest and gets only the default
    /// sandbox. manifestDigest pins it, so it must be set with it. When the
    /// source is chosen by the composite resource (module.from), the
    /// manifest URL is fenced by the compositionPolicy's pullModule rules
    /// like the module URL.
    #[serde(rename = "manifestURL")]
    #[schemars(regex(pattern = r"^https?://"))]
    pub manifest_url: String,
    /// The sha256 of the manifest, sha256:<hex>; the download is verified
    /// against it. Required with manifestURL, refused without it.
    #[schemars(regex(pattern = r"^sha256:[a-f0-9]{64}$"))]
    pub manifest_digest: String,
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct Limits {
    /// The wall-clock budget of one run, a Go duration such as "5s" or
    /// "1m30s"; at most --module-timeout. The request context's deadline
    /// still applies if shorter.
    #[schemars(regex(pattern = r"^([0-9]+(\.[0-9]+)?(ns|us|µs|ms|s|m|h))+$"))]
    pub timeout: Option<String>,
    /// Caps the guest's linear memory, a Kubernetes quantity such as
    /// "128Mi" (YAML authors may write it as a bare number of bytes); at
    /// most --module-memory-limit. A guest that grows past it sees
    /// memory.grow fail (a Go guest panics), which ends the run as a fatal
    /// result.
    #[serde(deserialize_with = "string_or_number")]
    #[schemars(schema_with = "crate::crd::int_or_string_quantity")]
    pub memory: Option<String>,
    /// Caps how many runs of this step execute at once, across all
    /// requests. A further request waits for a slot under its own context;
    /// when the deadline passes first, it is a fatal result that consumed
    /// nothing and is not counted as a run. Keyed by the module's content
    /// digest, so two Compositions using the same module share the limit.
    /// A value above --max-concurrent-runs is silently capped. No ceiling
    /// flag: this only narrows.
    #[schemars(range(min = 1))]
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
    fn the_input_is_named_by_v1_or_the_legacy_group() {
        let cases = [
            ((API_VERSION, KIND), true),
            (("wasm.fn.crossplane.io/v1", KIND), true),
            (("wasm.fn.crossplane.io/v1beta1", KIND), true),
            (("wasm.fn.crossplane.io", KIND), false),
            (("guestfn.crossplane.io/v2", KIND), false),
            (("guestfn.crossplane.io/v1", "Resources"), false),
            (("", KIND), false),
        ];
        for ((api_version, kind), want) in cases {
            assert_eq!(is_input(api_version, kind), want, "{api_version} {kind}");
        }
    }

    #[test]
    fn the_legacy_group_is_refused_by_name() {
        assert!(is_legacy_group("wasm.fn.crossplane.io/v1beta1"));
        assert!(!is_legacy_group(API_VERSION));
        assert_eq!(
            renamed_group("wasm.fn.crossplane.io/v1"),
            "apiVersion wasm.fn.crossplane.io/v1 is no longer served; write apiVersion: guestfn.crossplane.io/v1 (the same fields)"
        );
    }

    #[test]
    fn the_api_version_is_decoded_beside_the_fields() {
        let input: Input = serde_json::from_value(serde_json::json!({
            "apiVersion": "wasm.fn.crossplane.io/v1",
            "kind": KIND,
            "module": {"type": "Path", "path": "fn.wasm"},
        }))
        .expect("decode");
        assert_eq!(input.api_version, "wasm.fn.crossplane.io/v1");
        assert_eq!(input.module.path, "fn.wasm");
    }
}
