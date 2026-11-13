//! module.from: the composite resource chooses the module instance, the
//! Input's compositionPolicy fences what it may pick. Refusal strings are
//! pinned by the conformance goldens.

use serde::Deserialize;

use crate::admission;
use crate::authz::{CompositionPolicy, Principal};
use crate::input::{Config, HttpSource, ModuleSource, OciSource};
use crate::location::{http_location, oci_location};

/// Materialises src.from: the named field of the composite resource (its
/// observed object form) is read and must decode into the source src.type
/// names. The returned source is concrete and can be resolved; a src
/// without from is returned unchanged (shape-checked). comp is the Input's
/// compositionPolicy, compiled - an XR-chosen ref (or url) must be permitted
/// by a pullModule rule, and an XR-chosen OCI object may name a
/// pipeline-step credential only where a spendCredential rule permits it for
/// that repository. A static source is not subject to the fence.
///
/// None is the one non-error way out: the field is unset (absent, or null)
/// and src.allowEmpty says the composite resource may choose no module, so
/// the step has nothing to run. Without allowEmpty an unset field is the
/// error it always was.
pub fn from_composite(
    src: &ModuleSource,
    comp: Option<&CompositionPolicy>,
    composite: Option<&serde_json::Value>,
) -> Result<Option<ModuleSource>, String> {
    admission::validate_source(src)?;
    let mut src = src.clone();
    if src.from.is_empty() {
        return Ok(Some(src));
    }
    let from = std::mem::take(&mut src.from);
    // The concrete source is static from here on: allowEmpty was the
    // choice about the field, and the field has been read.
    let allow_empty = std::mem::take(&mut src.allow_empty);
    let Some(composite) = composite else {
        if allow_empty {
            return Ok(None);
        }
        return Err(format!(
            "module.from {from}: no observed composite resource to read it from"
        ));
    };
    let value = match field_value(composite, &from) {
        Ok(value) => value,
        Err(e) => {
            if allow_empty {
                return Ok(None);
            }
            return Err(format!(
                "module.from: cannot read {from} from the composite resource: {e}"
            ));
        }
    };
    if allow_empty && value.is_null() {
        return Ok(None);
    }
    decode_from_value(&mut src, value).map_err(|e| {
        format!(
            "module.from: {from} of the composite resource is not a {}: {e}",
            kind_of(&src.r#type)
        )
    })?;
    admission::validate_source(&src)
        .map_err(|e| format!("module.from: {from} of the composite resource: {e}"))?;
    admit(&from, &src, comp, &principal_from_composite(composite))?;
    Ok(Some(src))
}

/// Reads config.from: the named field of the composite resource holds the
/// module's configuration, an object, which the runtime checks against the
/// module manifest's config.schema once the manifest is read and forwards
/// to the module as input.config (docs/one-pager-module-config.md). The
/// shape of the config object was validated by admission already; this is
/// the read, under module.from's rules and with its wording.
///
/// None is the one non-error way out: the field is unset (absent, or
/// null) and config.allowEmpty says the composite resource may leave the
/// module unconfigured. Without allowEmpty an unset field is an error that
/// names it, as a static configuration's absence never was: the
/// Composition author said where the configuration comes from.
pub fn config_from_composite(
    config: &Config,
    composite: Option<&serde_json::Value>,
) -> Result<Option<serde_json::Value>, String> {
    let from = config.from.as_str();
    let Some(composite) = composite else {
        if config.allow_empty {
            return Ok(None);
        }
        return Err(format!(
            "config.from {from}: no observed composite resource to read it from"
        ));
    };
    let value = match field_value(composite, from) {
        Ok(value) => value,
        Err(e) => {
            if config.allow_empty {
                return Ok(None);
            }
            return Err(format!(
                "config.from: cannot read {from} from the composite resource: {e}"
            ));
        }
    };
    if config.allow_empty && value.is_null() {
        return Ok(None);
    }
    // Objects only: every scaffold's schema declares one, and a scalar or
    // a list would reach the module as a shape no GetConfig expects
    // (the record's open question, settled for the first build).
    if !value.is_object() {
        return Err(format!(
            "config.from: {from} of the composite resource is not an object"
        ));
    }
    Ok(Some(value.clone()))
}

/// Checks what can be known of a from source without the composite
/// resource: that the Input carries a compositionPolicy at all - the fence
/// from_composite evaluates once the value is read. The shape was validated
/// by admission already.
pub fn validate_from(src: &ModuleSource, comp: Option<&CompositionPolicy>) -> Result<(), String> {
    if src.from.is_empty() {
        return Ok(());
    }
    require_composition_policy(&src.from, &src.r#type, comp)
}

/// A source the composite resource chooses must be fenced: without a
/// composition policy to permit its repository, its author would point the
/// runtime at any host. Path sources have no host.
fn require_composition_policy(
    from: &str,
    t: &str,
    comp: Option<&CompositionPolicy>,
) -> Result<(), String> {
    if t == "Path" {
        return Ok(());
    }
    if comp.is_none() {
        return Err(format!(
            "module.from: {from} of the composite resource names a {t} source, but the Input has no compositionPolicy: a module the composite resource chooses must be permitted by the compositionPolicy's pullModule rules, or its author could point the runtime at any host"
        ));
    }
    Ok(())
}

/// The composition-policy fence over a concrete composite-chosen source
/// (default-deny): its normalized location must be pullModule-permitted, and
/// credentials may be named only where spendCredential permits them for that
/// location.
fn admit(
    from: &str,
    src: &ModuleSource,
    comp: Option<&CompositionPolicy>,
    principal: &Principal,
) -> Result<(), String> {
    let (field, location) = match src.r#type.as_str() {
        "OCI" => {
            let oci = src
                .oci
                .as_ref()
                .expect("validated: an OCI source has its object");
            ("ref", oci_location(&oci.r#ref))
        }
        "HTTP" => {
            let http = src
                .http
                .as_ref()
                .expect("validated: an HTTP source has its object");
            ("url", http_location("module.http.url", &http.url))
        }
        _ => return Ok(()),
    };
    let location =
        location.map_err(|e| format!("module.from: {from} of the composite resource: {e}"))?;
    require_composition_policy(from, &src.r#type, comp)?;
    let comp = comp.expect("checked above");
    if !comp.permits_pull_module(principal, &location) {
        return Err(format!(
            "module.from: {from} of the composite resource names {field} {location:?}, which the compositionPolicy does not permit (pullModule)"
        ));
    }
    // A manifest the composite resource chose to fetch by URL is fenced like
    // the module: its own location must be pullModule-permitted too.
    if src.r#type == "HTTP"
        && let Some(http) = &src.http
        && !http.manifest_url.is_empty()
    {
        let manifest_loc = http_location("module.http.manifestURL", &http.manifest_url)
            .map_err(|e| format!("module.from: {from} of the composite resource: {e}"))?;
        if !comp.permits_pull_module(principal, &manifest_loc) {
            return Err(format!(
                "module.from: {from} of the composite resource names manifestURL {manifest_loc:?}, which the compositionPolicy does not permit (pullModule)"
            ));
        }
    }
    let Some(oci) = &src.oci else { return Ok(()) };
    if src.r#type != "OCI" || oci.credentials.is_empty() {
        return Ok(());
    }
    if !comp.permits_spend_credential(principal, &oci.credentials, &location) {
        return Err(format!(
            "module.from: {from} of the composite resource names credentials {:?}, which the compositionPolicy does not permit (spendCredential) for {location:?}: a module chosen by the composite resource cannot spend a step credential (the registry host would be its author's) unless the compositionPolicy permits it for that repository; otherwise pull it with the runtime's Docker config or anonymously",
            oci.credentials
        ));
    }
    Ok(())
}

/// The caller identity a composition policy may key on, read from the
/// observed composite resource: its kind and namespace.
pub fn principal_from_composite(composite: &serde_json::Value) -> Principal {
    Principal {
        xr_kind: composite
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        namespace: composite
            .pointer("/metadata/namespace")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        composition: String::new(),
    }
}

/// Reads a dotted field path ("status.module") from the composite resource.
fn field_value<'a>(
    composite: &'a serde_json::Value,
    path: &str,
) -> Result<&'a serde_json::Value, String> {
    let mut current = composite;
    for segment in path.split('.') {
        current = current
            .get(segment)
            .ok_or_else(|| format!("{segment}: no such field"))?;
    }
    Ok(current)
}

/// Casts the composite resource's value into the source src.type names,
/// refusing unknown fields so a typo in the composite resource is an error
/// rather than an ignored field.
fn decode_from_value(src: &mut ModuleSource, value: &serde_json::Value) -> Result<(), String> {
    #[derive(Default, Deserialize)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    struct StrictOci {
        r#ref: String,
        credentials: String,
    }
    #[derive(Default, Deserialize)]
    #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
    struct StrictHttp {
        url: String,
        digest: String,
        #[serde(rename = "manifestURL")]
        manifest_url: String,
        manifest_digest: String,
    }
    match src.r#type.as_str() {
        "OCI" => {
            let v: StrictOci = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            src.oci = Some(OciSource {
                r#ref: v.r#ref,
                credentials: v.credentials,
            });
        }
        "HTTP" => {
            let v: StrictHttp = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            src.http = Some(HttpSource {
                url: v.url,
                digest: v.digest,
                manifest_url: v.manifest_url,
                manifest_digest: v.manifest_digest,
            });
        }
        _ => {
            let v: String = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            src.path = v;
        }
    }
    Ok(())
}

fn kind_of(t: &str) -> &'static str {
    match t {
        "OCI" => "{ref, credentials} object",
        "HTTP" => "{url, digest} object",
        _ => "string",
    }
}

/// Whether a module at this source must carry a cosign signature - settled
/// before it is resolved. With an operator policy
/// the requirement is per repository (a path source, operator-controlled
/// and locationless, is never required); without one it is the legacy
/// all-or-nothing --cosign-key: a configured verifier requires every
/// module, no key requires none.
pub fn signature_required(
    policy: Option<&crate::authz::OperatorPolicy>,
    verifier_configured: bool,
    src: &ModuleSource,
) -> bool {
    let Some(policy) = policy else {
        return verifier_configured;
    };
    let location = match src.r#type.as_str() {
        "OCI" => src.oci.as_ref().and_then(|o| oci_location(&o.r#ref).ok()),
        "HTTP" => src
            .http
            .as_ref()
            .and_then(|h| http_location("module.http.url", &h.url).ok()),
        _ => None,
    };
    location.is_some_and(|l| policy.requires_signature(&l))
}

/// Why a required non-OCI source cannot run: only OCI artifacts carry a
/// cosign signature.
pub fn non_oci_signature_refusal(
    policy: Option<&crate::authz::OperatorPolicy>,
    src: &ModuleSource,
) -> String {
    if policy.is_none() {
        return "only cosign-signed oci modules are accepted (--cosign-key, --cosign-trusted-root or --cosign-trust-instance is set); http and path sources are refused".to_string();
    }
    let (field, location) = match src.r#type.as_str() {
        "HTTP" => (
            "http",
            src.http
                .as_ref()
                .and_then(|h| http_location("module.http.url", &h.url).ok())
                .unwrap_or_default(),
        ),
        _ => ("path", src.path.clone()),
    };
    format!(
        "module.{field} {location:?} requires a cosign signature (operator policy), but only OCI modules can be signature-verified"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::compile_composition_policy;

    fn xr() -> serde_json::Value {
        serde_json::json!({
            "apiVersion": "example.crossplane.io/v1",
            "kind": "XR",
            "metadata": { "name": "my-xr" },
            "status": {
                "module": { "ref": format!("ghcr.io/example/greeter@sha256:{}", "3f2a".repeat(16)) },
                "other": { "ref": format!("docker.io/someone/else@sha256:{}", "3f2a".repeat(16)) },
            },
        })
    }

    fn from_src(from: &str) -> ModuleSource {
        ModuleSource {
            r#type: "OCI".to_string(),
            from: from.to_string(),
            ..Default::default()
        }
    }

    const POLICY: &str = r#"permit (principal, action == Action::"pullModule", resource in Repository::"ghcr.io/example");"#;

    #[test]
    fn materialises_a_permitted_source() {
        let comp = compile_composition_policy(POLICY)
            .expect("compile")
            .expect("some");
        let src = from_composite(&from_src("status.module"), Some(&comp), Some(&xr()))
            .expect("admit")
            .expect("a module was chosen");
        assert!(src.from.is_empty());
        assert!(
            src.oci
                .expect("oci")
                .r#ref
                .starts_with("ghcr.io/example/greeter@")
        );
    }

    #[test]
    fn refuses_an_unpermitted_repository() {
        let comp = compile_composition_policy(POLICY)
            .expect("compile")
            .expect("some");
        let err =
            from_composite(&from_src("status.other"), Some(&comp), Some(&xr())).expect_err("fence");
        assert_eq!(
            err,
            r#"module.from: status.other of the composite resource names ref "index.docker.io/someone/else", which the compositionPolicy does not permit (pullModule)"#
        );
    }

    fn oci_src(name: &str) -> ModuleSource {
        ModuleSource {
            r#type: "OCI".to_string(),
            oci: Some(crate::input::OciSource {
                r#ref: format!("{name}@sha256:{}", "3f2a".repeat(16)),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_signature_requirement_covers_every_spelling_of_the_registry() {
        let policy = crate::authz::OperatorPolicy::new(
            "test",
            r#"permit (principal, action == Action::"requireSignature", resource)
               when { resource in Repository::"ghcr.io/acme/prod" };"#,
        )
        .expect("compile");
        let required = |name: &str| signature_required(Some(&policy), false, &oci_src(name));
        let cases = [
            ("ghcr.io/acme/prod/m", true),
            ("GHCR.IO/acme/prod/m", true),
            ("ghcr.io:443/acme/prod/m", true),
            ("ghcr.io./acme/prod/m", true),
            ("Ghcr.Io.:443/acme/prod/m:v1", true),
            // Another port is another registry, another path another
            // repository.
            ("ghcr.io:8443/acme/prod/m", false),
            ("ghcr.io/acme/production/m", false),
        ];
        for (name, want) in cases {
            assert_eq!(required(name), want, "{name}");
        }
        // Docker Hub under each of its names.
        let hub = crate::authz::OperatorPolicy::new(
            "test",
            r#"permit (principal, action == Action::"requireSignature", resource)
               when { resource in Repository::"index.docker.io/acme" };"#,
        )
        .expect("compile");
        for name in [
            "acme/m",
            "docker.io/acme/m",
            "index.docker.io/acme/m",
            "DOCKER.IO:443/acme/m",
        ] {
            assert!(
                signature_required(Some(&hub), false, &oci_src(name)),
                "{name}"
            );
        }
        // registry-1.docker.io is a host of its own to the policy.
        assert!(!signature_required(
            Some(&hub),
            false,
            &oci_src("registry-1.docker.io/acme/m")
        ));
    }

    #[test]
    fn a_pull_fence_covers_every_spelling_of_the_registry() {
        let comp = compile_composition_policy(
            r#"permit (principal, action == Action::"pullModule", resource);
               forbid (principal, action == Action::"pullModule", resource in Repository::"ghcr.io/example");"#,
        )
        .expect("compile")
        .expect("some");
        for registry in ["ghcr.io", "GHCR.IO", "ghcr.io:443", "ghcr.io."] {
            let xr = serde_json::json!({
                "status": { "module": {
                    "ref": format!("{registry}/example/greeter@sha256:{}", "3f2a".repeat(16)),
                } },
            });
            assert_eq!(
                from_composite(&from_src("status.module"), Some(&comp), Some(&xr)).map(|_| ()),
                Err(
                    r#"module.from: status.module of the composite resource names ref "ghcr.io/example/greeter", which the compositionPolicy does not permit (pullModule)"#
                        .to_string()
                ),
                "{registry}"
            );
        }
    }

    fn http_src(url: &str) -> ModuleSource {
        ModuleSource {
            r#type: "HTTP".to_string(),
            http: Some(crate::input::HttpSource {
                url: url.to_string(),
                digest: format!("sha256:{}", "3f2a".repeat(16)),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn path_src(path: &str) -> ModuleSource {
        ModuleSource {
            r#type: "Path".to_string(),
            path: path.to_string(),
            ..Default::default()
        }
    }

    /// Only an OCI artifact carries a cosign signature, so a source of
    /// another type is refused wherever one is required: every Path and
    /// HTTP source under a verifier with no operator policy, and under a
    /// policy the HTTP sources its requireSignature rules name. A Path
    /// source has no location for a rule to name.
    #[test]
    fn a_required_source_that_cannot_be_signed_is_refused_by_name() {
        const KEY_ALONE: &str = "only cosign-signed oci modules are accepted (--cosign-key, --cosign-trusted-root or --cosign-trust-instance is set); http and path sources are refused";
        let policy = |doc: &str| crate::authz::OperatorPolicy::new("test", doc).expect("compile");
        let secure = policy(
            r#"permit (principal, action == Action::"requireSignature", resource)
               when { resource in Repository::"https://modules.example.com/secure" };"#,
        );
        let everything =
            policy(r#"permit (principal, action == Action::"requireSignature", resource);"#);
        let no_rule = policy(r#"permit (principal, action == Action::"usePrivateTmp", resource);"#);

        // (name, policy, a verifier is configured, source, the refusal or
        // None where no signature is required)
        type Case<'a> = (
            &'a str,
            Option<&'a crate::authz::OperatorPolicy>,
            bool,
            ModuleSource,
            Option<&'a str>,
        );
        let cases: &[Case] = &[
            ("Neither/Path", None, false, path_src("fn.wasm"), None),
            (
                "Neither/HTTP",
                None,
                false,
                http_src("https://modules.example.com/secure/fn.wasm"),
                None,
            ),
            (
                "KeyAlone/Path",
                None,
                true,
                path_src("fn.wasm"),
                Some(KEY_ALONE),
            ),
            (
                "KeyAlone/HTTP",
                None,
                true,
                http_src("https://modules.example.com/open/fn.wasm"),
                Some(KEY_ALONE),
            ),
            (
                "Policy/HTTPUnderTheRule",
                Some(&secure),
                false,
                http_src("https://modules.example.com/secure/fn.wasm"),
                Some(
                    r#"module.http "https://modules.example.com/secure/fn.wasm" requires a cosign signature (operator policy), but only OCI modules can be signature-verified"#,
                ),
            ),
            (
                // The location, not the URL as written: the host lowercased,
                // the query left out.
                "Policy/HTTPUnderTheRuleInAnotherSpelling",
                Some(&secure),
                true,
                http_src("https://Modules.Example.COM/secure/team/fn.wasm?version=2"),
                Some(
                    r#"module.http "https://modules.example.com/secure/team/fn.wasm" requires a cosign signature (operator policy), but only OCI modules can be signature-verified"#,
                ),
            ),
            (
                "Policy/HTTPBesideTheRule",
                Some(&secure),
                true,
                http_src("https://modules.example.com/secure-not/fn.wasm"),
                None,
            ),
            (
                "Policy/HTTPOverAnotherScheme",
                Some(&secure),
                true,
                http_src("http://modules.example.com/secure/fn.wasm"),
                None,
            ),
            (
                "Policy/EveryRepository/HTTP",
                Some(&everything),
                false,
                http_src("https://modules.example.com/open/fn.wasm"),
                Some(
                    r#"module.http "https://modules.example.com/open/fn.wasm" requires a cosign signature (operator policy), but only OCI modules can be signature-verified"#,
                ),
            ),
            (
                "Policy/EveryRepository/Path",
                Some(&everything),
                true,
                path_src("fn.wasm"),
                None,
            ),
            (
                // Under a policy the key only supplies keys.
                "Policy/NoRule/HTTP",
                Some(&no_rule),
                true,
                http_src("https://modules.example.com/secure/fn.wasm"),
                None,
            ),
            (
                "Policy/NoRule/Path",
                Some(&no_rule),
                true,
                path_src("fn.wasm"),
                None,
            ),
        ];
        for (name, policy, verifier, source, want) in cases {
            let refusal = signature_required(*policy, *verifier, source)
                .then(|| non_oci_signature_refusal(*policy, source));
            assert_eq!(refusal.as_deref(), *want, "{name}");
        }
    }

    /// An HTTP source the composite resource chooses is fenced by its
    /// location, so a URL that has none is refused before the fence is
    /// asked: one carrying user information, or a path a server would
    /// read as another.
    #[test]
    fn a_from_source_must_name_an_http_location() {
        let comp = compile_composition_policy(
            r#"permit (principal, action == Action::"pullModule", resource in Repository::"https://modules.example.com/team");"#,
        )
        .expect("compile")
        .expect("some");
        let choose = |field: &str, value: &str| {
            let xr = serde_json::json!({
                "status": { "module": {
                    "url": "https://modules.example.com/team/fn.wasm",
                    "digest": format!("sha256:{}", "3f2a".repeat(16)),
                    field: value,
                } },
            });
            let src = ModuleSource {
                r#type: "HTTP".to_string(),
                from: "status.module".to_string(),
                ..Default::default()
            };
            from_composite(&src, Some(&comp), Some(&xr)).map(|_| ())
        };
        let refused = |sentence: &str| {
            Err(format!(
                "module.from: status.module of the composite resource: {sentence}"
            ))
        };
        let cases = [
            ("url", "https://modules.example.com/team/fn.wasm", Ok(())),
            (
                "url",
                "https://MODULES.example.com/team/fn.wasm?version=2",
                Ok(()),
            ),
            (
                "url",
                "https://team:secret@modules.example.com/team/fn.wasm",
                refused(
                    r#"module.http.url "https://team:secret@modules.example.com/team/fn.wasm" must name a host and carry no user information"#,
                ),
            ),
            (
                "url",
                "https:///team/fn.wasm",
                refused(
                    r#"module.http.url "https:///team/fn.wasm" must name a host and carry no user information"#,
                ),
            ),
            (
                "url",
                "https://modules.example.com/team/../other/fn.wasm",
                refused(
                    r#"module.http.url "https://modules.example.com/team/../other/fn.wasm" must have a normalized path (no . or .. segments, no empty segments)"#,
                ),
            ),
            (
                "url",
                "https://modules.example.com/team//fn.wasm",
                refused(
                    r#"module.http.url "https://modules.example.com/team//fn.wasm" must have a normalized path (no . or .. segments, no empty segments)"#,
                ),
            ),
            (
                "url",
                "https://modules.example.com/other/fn.wasm",
                Err(
                    r#"module.from: status.module of the composite resource names url "https://modules.example.com/other/fn.wasm", which the compositionPolicy does not permit (pullModule)"#
                        .to_string(),
                ),
            ),
            (
                "manifestURL",
                "https://modules.example.com/team/./guestfn.yaml",
                refused(
                    r#"module.http.manifestURL "https://modules.example.com/team/./guestfn.yaml" must have a normalized path (no . or .. segments, no empty segments)"#,
                ),
            ),
            (
                "manifestURL",
                "https://modules.example.com/other/guestfn.yaml",
                Err(
                    r#"module.from: status.module of the composite resource names manifestURL "https://modules.example.com/other/guestfn.yaml", which the compositionPolicy does not permit (pullModule)"#
                        .to_string(),
                ),
            ),
        ];
        for (field, value, want) in cases {
            assert_eq!(choose(field, value), want, "{field} {value}");
        }
    }

    #[test]
    fn refuses_a_from_source_with_no_policy() {
        let err = from_composite(&from_src("status.module"), None, Some(&xr())).expect_err("fence");
        assert!(
            err.contains("but the Input has no compositionPolicy"),
            "{err}"
        );
    }

    /// config.from reads an object, leaves an unset field to allowEmpty
    /// and words its refusals as module.from does.
    #[test]
    fn reads_the_configuration_from_the_composite_resource() {
        let xr = serde_json::json!({
            "apiVersion": "example.crossplane.io/v1",
            "kind": "XR",
            "metadata": {"name": "my-xr"},
            "spec": {"config": {"greeting": "hi"}, "scalar": 7, "empty": null},
        });
        let config = |from: &str, allow_empty: bool| Config {
            from: from.to_string(),
            allow_empty,
            ..Default::default()
        };
        assert_eq!(
            config_from_composite(&config("spec.config", false), Some(&xr)),
            Ok(Some(serde_json::json!({"greeting": "hi"})))
        );
        assert_eq!(
            config_from_composite(&config("spec.absent", false), Some(&xr)),
            Err("config.from: cannot read spec.absent from the composite resource: absent: no such field".to_string())
        );
        assert_eq!(
            config_from_composite(&config("spec.absent", true), Some(&xr)),
            Ok(None)
        );
        assert_eq!(
            config_from_composite(&config("spec.empty", true), Some(&xr)),
            Ok(None)
        );
        assert_eq!(
            config_from_composite(&config("spec.scalar", false), Some(&xr)),
            Err("config.from: spec.scalar of the composite resource is not an object".to_string())
        );
        assert_eq!(
            config_from_composite(&config("spec.scalar", true), Some(&xr)),
            Err("config.from: spec.scalar of the composite resource is not an object".to_string()),
            "allowEmpty forgives an unset field, never a wrong shape"
        );
        assert_eq!(
            config_from_composite(&config("spec.config", false), None),
            Err(
                "config.from spec.config: no observed composite resource to read it from"
                    .to_string()
            )
        );
        assert_eq!(
            config_from_composite(&config("spec.config", true), None),
            Ok(None)
        );
    }

    #[test]
    fn an_unset_field_is_an_error_unless_allow_empty() {
        let comp = compile_composition_policy(POLICY)
            .expect("compile")
            .expect("some");
        let mut xr = xr();
        xr["status"]["empty"] = serde_json::Value::Null;

        let err = from_composite(&from_src("status.hook"), Some(&comp), Some(&xr))
            .expect_err("an unset field");
        assert_eq!(
            err,
            "module.from: cannot read status.hook from the composite resource: hook: no such field"
        );
        let err = from_composite(&from_src("status.hook"), Some(&comp), None)
            .expect_err("no composite resource");
        assert_eq!(
            err,
            "module.from status.hook: no observed composite resource to read it from"
        );

        let allow = |from: &str| ModuleSource {
            allow_empty: true,
            ..from_src(from)
        };
        // Absent, null, and no composite resource at all: nothing chosen.
        for (name, composite) in [
            ("absent", Some(&xr)),
            ("null", Some(&xr)),
            ("no composite", None),
        ] {
            let from = if name == "null" {
                "status.empty"
            } else {
                "status.hook"
            };
            let got = from_composite(&allow(from), Some(&comp), composite).expect(name);
            assert!(got.is_none(), "{name}: {got:?}");
        }
        // A chosen module is still fenced and still concrete.
        let got = from_composite(&allow("status.module"), Some(&comp), Some(&xr))
            .expect("admit")
            .expect("chosen");
        assert!(!got.allow_empty && got.from.is_empty());
        let err =
            from_composite(&allow("status.other"), Some(&comp), Some(&xr)).expect_err("fenced");
        assert!(err.contains("(pullModule)"), "{err}");
    }
}
