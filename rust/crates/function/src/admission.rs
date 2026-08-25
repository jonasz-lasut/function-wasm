//! Admission of one request's Input against the runtime's ceilings: the Rust
//! port of `internal/admission.Admit` and `internal/module.Validate` for the
//! sources this runtime serves. Refusal strings for implemented checks match
//! the Go runtime verbatim; a feature the port does not carry yet is refused
//! with a message naming it - never silently ignored, so nothing runs wider
//! than the Go runtime would allow.

use std::sync::Arc;
use std::time::Duration;

use function_wasm_engine::duration;

use crate::authz::{
    ACTION_GRANT_EGRESS, ACTION_SET_ENV, ACTION_SPEND_CREDENTIAL, ACTION_USE_PRIVATE_TMP,
    CompositionPolicy, EgressGrant, OperatorPolicy, Principal, compile_composition_policy,
};
use crate::egress_rules::HttpRule;
use crate::input::{Input, ModuleSource};
use crate::location::digest_is_valid;
use crate::manifest::{Grants, Requires};
use crate::quantity;
use crate::sandboxenv::EnvBinding;

/// What a request gets when its Input is admitted: its limits within the
/// ceilings, and the Input's compositionPolicy compiled (content-hash
/// cached) - the composition layer of the capability decision and the fence
/// over a module.from source.
#[derive(Debug, Default)]
pub struct Admitted {
    pub timeout: Option<Duration>,
    pub memory_limit: Option<u64>,
    pub composition: Option<Arc<CompositionPolicy>>,
}

pub fn admit(input: &Input, ceilings: &function_wasm_engine::Config) -> Result<Admitted, String> {
    let composition = compile_composition_policy(&input.composition_policy)
        .map_err(|e| format!("compositionPolicy is invalid: {e}"))?;
    let mut admitted = run_options(input, ceilings)?;
    admitted.composition = composition;
    validate_source(&input.module).map_err(|e| format!("cannot resolve module: {e}"))?;
    Ok(admitted)
}

/// What one run gets of the sandbox: the module's requests, each permitted
/// by the composition and operator layers. The default is the default
/// sandbox: nothing.
#[derive(Debug, Default)]
pub struct Capabilities {
    pub private_tmp: bool,
    /// The module's egress rules the layers admitted, as required.
    pub rules: Vec<HttpRule>,
    /// The module's env bindings the layers admitted, for materialize.
    pub env: Vec<EnvBinding>,
}

impl Capabilities {
    /// The admitted set in the shape manifest::check holds Requires against.
    pub fn grants(&self) -> Grants {
        Grants {
            private_tmp: self.private_tmp,
            http: self.rules.clone(),
            env: self.env.clone(),
        }
    }
}

/// Decides a module's requested capabilities - its manifest's requires (None:
/// nothing, the default sandbox) - by the three-layer rule: each request must
/// be permitted by the composition layer (scoped default-permit: absent, or
/// scoping no rule for the action, it does not narrow) and by the operator
/// layer (default-deny: no --sandbox-policy-file, no capability). The first
/// refusal is returned in the runtime's words, for the caller to prefix with
/// the module's name.
pub fn admit_requires(
    r: Option<&Requires>,
    policy: Option<&OperatorPolicy>,
    comp: Option<&CompositionPolicy>,
    principal: &Principal,
) -> Result<Capabilities, String> {
    let mut out = Capabilities::default();
    let Some(r) = r else { return Ok(out) };
    if r.filesystem.as_ref().is_some_and(|f| f.private_tmp) {
        if scopes(comp, ACTION_USE_PRIVATE_TMP)
            && !comp.expect("scoped").permits_private_tmp(principal)
        {
            return Err("requires a private /tmp (requires.filesystem.privateTmp), which the compositionPolicy does not permit for this request".to_string());
        }
        let Some(policy) = policy else {
            return Err("requires a private /tmp (requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities".to_string());
        };
        if !policy.permits_private_tmp(principal) {
            return Err("requires a private /tmp (requires.filesystem.privateTmp), which the operator policy (--sandbox-policy-file) does not permit for this request".to_string());
        }
        out.private_tmp = true;
    }
    if let Some(egress) = &r.egress
        && !egress.http.is_empty()
    {
        admit_egress(&egress.http, policy, comp, principal)?;
        // The layers permit the rules, but this runtime does not carry the
        // egress client (internal/egress) yet: the mechanism itself is
        // missing, refused with the Go runtime's words for that state.
        return Err(
            "requires egress (requires.egress.http), but the runtime has no egress mechanism"
                .to_string(),
        );
    }
    if !r.env.is_empty() {
        admit_env(&r.env, policy, comp, principal)?;
        out.env = r.env.clone();
    }
    Ok(out)
}

fn scopes(comp: Option<&CompositionPolicy>, action: &str) -> bool {
    comp.is_some_and(|c| c.scopes_action(action))
}

/// Runs both policy layers over the module's egress rules, once per rule and
/// method, so a policy can key on context.method. The composition layer
/// first, whole: the author closest to the fix reads their own layer's
/// refusal even where the operator would also deny.
fn admit_egress(
    rules: &[HttpRule],
    policy: Option<&OperatorPolicy>,
    comp: Option<&CompositionPolicy>,
    principal: &Principal,
) -> Result<(), String> {
    if scopes(comp, ACTION_GRANT_EGRESS) {
        let comp = comp.expect("scoped");
        each_egress(rules, |i, host, g| {
            if !comp.permits_egress(principal, &g) {
                return Err(format!(
                    "requires egress {} to host {host:?} (requires.egress.http[{i}]), which the compositionPolicy does not permit",
                    g.method
                ));
            }
            Ok(())
        })?;
    }
    let Some(policy) = policy else {
        return Err("requires egress (requires.egress.http), but the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)".to_string());
    };
    each_egress(rules, |i, host, g| {
        if !policy.permits_egress(principal, &g) {
            return Err(format!(
                "requires egress {} to host {host:?} (requires.egress.http[{i}]), which the operator policy (--sandbox-policy-file) does not permit",
                g.method
            ));
        }
        Ok(())
    })
}

/// Visits every (rule, method) pair of the module's egress rules.
fn each_egress(
    rules: &[HttpRule],
    mut visit: impl FnMut(usize, &str, EgressGrant) -> Result<(), String>,
) -> Result<(), String> {
    for (i, r) in rules.iter().enumerate() {
        let host = if r.host.is_empty() {
            &r.host_pattern
        } else {
            &r.host
        };
        for m in &r.methods {
            let g = EgressGrant {
                host: r.host.clone(),
                host_pattern: r.host_pattern.clone(),
                method: m.clone(),
                path: r.path_prefix.clone(),
            };
            visit(i, host, g)?;
        }
    }
    Ok(())
}

/// Runs both policy layers over the module's env bindings: setEnv once with
/// every bound name as context.keys, then spendCredential per binding.
fn admit_env(
    bindings: &[EnvBinding],
    policy: Option<&OperatorPolicy>,
    comp: Option<&CompositionPolicy>,
    principal: &Principal,
) -> Result<(), String> {
    let keys: Vec<String> = bindings.iter().map(|b| b.name.clone()).collect();
    let names = format!("[{}]", keys.join(" "));
    if scopes(comp, ACTION_SET_ENV) && !comp.expect("scoped").permits_env(principal, &keys) {
        return Err(format!(
            "requires env {names} (requires.env), which the compositionPolicy does not permit (setEnv)"
        ));
    }
    let Some(policy) = policy else {
        return Err(format!(
            "requires env {names} (requires.env), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities"
        ));
    };
    if !policy.permits_env(principal, &keys) {
        return Err(format!(
            "requires env {names} (requires.env), which the operator policy (--sandbox-policy-file) does not permit (setEnv)"
        ));
    }
    let narrows = scopes(comp, ACTION_SPEND_CREDENTIAL);
    for b in bindings {
        // An env binding spends a credential with no repository in play, so
        // the composition layer sees no context.repository.
        if narrows
            && !comp.expect("scoped").permits_spend_credential(
                principal,
                &b.from_credential.name,
                "",
            )
        {
            return Err(format!(
                "requires env {} from credential {:?}, which the compositionPolicy does not permit (spendCredential)",
                b.name, b.from_credential.name
            ));
        }
        if !policy.permits_spend_credential(principal, &b.from_credential.name) {
            return Err(format!(
                "requires env {} from credential {:?}, which the operator policy (--sandbox-policy-file) does not permit (spendCredential)",
                b.name, b.from_credential.name
            ));
        }
    }
    Ok(())
}

/// Refuses, by name, every source feature the port does not carry yet - the
/// serve path's guard, so nothing runs wider than the Go runtime would
/// allow. `function validate` deliberately does not apply it: an OCI source
/// is describable offline even though this runtime cannot serve it yet.
pub fn require_ported(m: &ModuleSource) -> Result<(), String> {
    match m.r#type.as_str() {
        "OCI" | "HTTP" => {
            return Err(format!(
                "module.type {} is not implemented yet in the Rust runtime; only Path sources are",
                m.r#type
            ));
        }
        _ => {}
    }
    Ok(())
}

fn run_options(input: &Input, ceilings: &function_wasm_engine::Config) -> Result<Admitted, String> {
    let mut out = Admitted::default();
    let Some(limits) = &input.limits else {
        return Ok(out);
    };
    if let Some(n) = limits.concurrency {
        if n <= 0 {
            return Err(format!("limits.concurrency {n} must be positive"));
        }
        return Err("limits.concurrency is not implemented yet in the Rust runtime".to_string());
    }
    if let Some(t) = &limits.timeout {
        let timeout = duration::parse(t)
            .map_err(|_| format!("limits.timeout {t:?} is not a valid duration"))?;
        if timeout.is_zero() {
            return Err(format!(
                "limits.timeout {} must be positive",
                duration::format(timeout)
            ));
        }
        if timeout > ceilings.timeout {
            return Err(format!(
                "limits.timeout {} exceeds the runtime's --module-timeout of {}",
                duration::format(timeout),
                duration::format(ceilings.timeout)
            ));
        }
        out.timeout = Some(timeout);
    }
    if let Some(mem) = &limits.memory {
        let bytes = quantity::parse(mem)
            .map_err(|_| format!("limits.memory {mem:?} is not a valid quantity"))?;
        if bytes <= 0 {
            return Err(format!("limits.memory {mem} must be positive"));
        }
        let bytes = u64::try_from(bytes)
            .map_err(|_| format!("limits.memory {mem:?} is not a valid quantity"))?;
        if bytes > ceilings.memory_limit {
            return Err(format!(
                "limits.memory {mem} exceeds the runtime's --module-memory-limit of {}",
                quantity::format_binary_si(ceilings.memory_limit)
            ));
        }
        out.memory_limit = Some(bytes);
    }
    Ok(out)
}

/// The module source shape check, ported from `internal/module.Validate`:
/// Type is set and exactly one of the object it names (oci, http, path) or
/// From is set, with no object of another type present.
pub(crate) fn validate_source(src: &ModuleSource) -> Result<(), String> {
    if src.r#type.is_empty() {
        return Err("module.type is required: OCI, HTTP or Path".to_string());
    }
    let types: &[(&str, &str, bool)] = &[
        ("OCI", "oci", src.oci.is_some()),
        ("HTTP", "http", src.http.is_some()),
        ("Path", "path", !src.path.is_empty()),
    ];
    let Some(&(_, field, has_object)) = types.iter().find(|(t, _, _)| *t == src.r#type) else {
        return Err(format!(
            "module.type {:?} must be OCI, HTTP or Path",
            src.r#type
        ));
    };
    for (t, f, set) in types {
        if *t != src.r#type && *set {
            return Err(format!(
                "module.{f} is set but module.type is {}",
                src.r#type
            ));
        }
    }
    let has_from = !src.from.is_empty();
    if has_object == has_from {
        return Err(format!(
            "module.type {} needs exactly one of module.{field} and module.from",
            src.r#type
        ));
    }
    if has_from && !from_field_is_valid(&src.from) {
        return Err(format!(
            "module.from {:?} must be a field under spec or status of the composite resource, e.g. status.module",
            src.from
        ));
    }
    if !src.manifest_path.is_empty() && src.r#type != "Path" {
        return Err(format!(
            "module.manifestPath is set but module.type is {}: it names a manifest file under --module-dir and is only allowed with type Path",
            src.r#type
        ));
    }
    if let Some(oci) = &src.oci {
        if oci.r#ref.is_empty() {
            return Err("module.oci.ref is required".to_string());
        }
        if !oci_ref_is_pinned(&oci.r#ref) {
            return Err(format!(
                "module.oci.ref {:?} must be a reference pinned to its manifest digest (repository@sha256:...); tags are not supported",
                oci.r#ref
            ));
        }
    }
    if let Some(http) = &src.http {
        if http.url.is_empty() {
            return Err("module.http.url is required".to_string());
        }
        if !(http.url.starts_with("http://") || http.url.starts_with("https://")) {
            return Err(format!(
                "module.http.url {:?} must be an http or https URL",
                http.url
            ));
        }
        if http.digest.is_empty() {
            return Err(
                "module.http.digest is required: the sha256 of the module file (sha256sum fn.wasm)"
                    .to_string(),
            );
        }
        if !digest_is_valid(&http.digest) {
            return Err(format!(
                "module.http.digest {:?} is not sha256:<64 hex characters>",
                http.digest
            ));
        }
    }
    Ok(())
}

fn from_field_is_valid(from: &str) -> bool {
    let rest = from
        .strip_prefix("spec.")
        .or_else(|| from.strip_prefix("status."));
    rest.is_some_and(|r| !r.is_empty())
}

fn oci_ref_is_pinned(r: &str) -> bool {
    let Some((repo, digest)) = r.rsplit_once('@') else {
        return false;
    };
    !repo.is_empty() && !repo.contains(char::is_whitespace) && digest_is_valid(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Limits;

    fn path_input(path: &str) -> Input {
        Input {
            module: ModuleSource {
                r#type: "Path".to_string(),
                path: path.to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn ceilings() -> function_wasm_engine::Config {
        function_wasm_engine::Config::default()
    }

    #[test]
    fn admits_a_path_source() {
        let admitted = admit(&path_input("fn.wasm"), &ceilings()).unwrap();
        assert_eq!(admitted.timeout, None);
        assert_eq!(admitted.memory_limit, None);
        assert!(admitted.composition.is_none());
    }

    #[test]
    fn refusal_strings_match_the_go_runtime() {
        let cases: &[(&str, Input, &str)] = &[
            (
                "NoType",
                Input::default(),
                "cannot resolve module: module.type is required: OCI, HTTP or Path",
            ),
            (
                "UnknownType",
                Input {
                    module: ModuleSource {
                        r#type: "Zip".to_string(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                r#"cannot resolve module: module.type "Zip" must be OCI, HTTP or Path"#,
            ),
            (
                "PathAndOCI",
                Input {
                    module: ModuleSource {
                        r#type: "Path".to_string(),
                        path: "fn.wasm".to_string(),
                        oci: Some(crate::input::OciSource {
                            r#ref: "r@sha256:0".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "cannot resolve module: module.oci is set but module.type is Path",
            ),
            (
                "NeitherObjectNorFrom",
                Input {
                    module: ModuleSource {
                        r#type: "Path".to_string(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "cannot resolve module: module.type Path needs exactly one of module.path and module.from",
            ),
            (
                "TimeoutOverCeiling",
                Input {
                    module: ModuleSource {
                        r#type: "Path".to_string(),
                        path: "fn.wasm".to_string(),
                        ..Default::default()
                    },
                    limits: Some(Limits {
                        timeout: Some("1m".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "limits.timeout 1m0s exceeds the runtime's --module-timeout of 30s",
            ),
            (
                "MemoryOverCeiling",
                Input {
                    module: ModuleSource {
                        r#type: "Path".to_string(),
                        path: "fn.wasm".to_string(),
                        ..Default::default()
                    },
                    limits: Some(Limits {
                        memory: Some("1Gi".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                "limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi",
            ),
        ];
        for (name, input, want) in cases {
            let err = admit(input, &ceilings()).expect_err(name);
            assert_eq!(&err, want, "{name}");
        }
    }
    #[test]
    fn require_ported_refuses_unported_sources() {
        let src = ModuleSource {
            r#type: "OCI".to_string(),
            oci: Some(crate::input::OciSource {
                r#ref: format!("ghcr.io/example/fn@sha256:{}", "a".repeat(64)),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            require_ported(&src).expect_err("OCI is not ported"),
            "module.type OCI is not implemented yet in the Rust runtime; only Path sources are"
        );
        assert_eq!(require_ported(&path_input("fn.wasm").module), Ok(()));
    }
}
