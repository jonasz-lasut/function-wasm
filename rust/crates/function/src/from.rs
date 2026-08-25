//! module.from: the composite resource chooses the module instance, the
//! Input's compositionPolicy fences what it may pick - the Rust port of
//! `internal/module`'s from.go and policy.go. Refusal strings match the Go
//! runtime verbatim.

use serde::Deserialize;

use crate::admission;
use crate::authz::{CompositionPolicy, Principal};
use crate::input::{HttpSource, ModuleSource, OciSource};
use crate::location::{http_location, oci_location};

/// Materialises src.from: the named field of the composite resource (its
/// observed object form) is read and must decode into the source src.type
/// names. The returned source is concrete and can be resolved; a src
/// without from is returned unchanged (shape-checked). comp is the Input's
/// compositionPolicy, compiled - an XR-chosen ref (or url) must be permitted
/// by a pullModule rule, and an XR-chosen OCI object may name a
/// pipeline-step credential only where a spendCredential rule permits it for
/// that repository. A static source is not subject to the fence.
pub fn from_composite(
    src: &ModuleSource,
    comp: Option<&CompositionPolicy>,
    composite: Option<&serde_json::Value>,
) -> Result<ModuleSource, String> {
    admission::validate_source(src)?;
    let mut src = src.clone();
    if src.from.is_empty() {
        return Ok(src);
    }
    let from = std::mem::take(&mut src.from);
    let Some(composite) = composite else {
        return Err(format!(
            "module.from {from}: no observed composite resource to read it from"
        ));
    };
    let value = field_value(composite, &from)
        .map_err(|e| format!("module.from: cannot read {from} from the composite resource: {e}"))?;
    decode_from_value(&mut src, value).map_err(|e| {
        format!(
            "module.from: {from} of the composite resource is not a {}: {e}",
            kind_of(&src.r#type)
        )
    })?;
    admission::validate_source(&src)
        .map_err(|e| format!("module.from: {from} of the composite resource: {e}"))?;
    admit(&from, &src, comp, &principal_from_composite(composite))?;
    Ok(src)
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
        let src =
            from_composite(&from_src("status.module"), Some(&comp), Some(&xr())).expect("admit");
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

    #[test]
    fn refuses_a_from_source_with_no_policy() {
        let err = from_composite(&from_src("status.module"), None, Some(&xr())).expect_err("fence");
        assert!(
            err.contains("but the Input has no compositionPolicy"),
            "{err}"
        );
    }
}
