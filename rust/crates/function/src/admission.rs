//! Admission of one request's Input against the runtime's ceilings: the Rust
//! port of `internal/admission.Admit` and `internal/module.Validate` for the
//! sources this runtime serves. Refusal strings for implemented checks match
//! the Go runtime verbatim; a feature the port does not carry yet is refused
//! with a message naming it - never silently ignored, so nothing runs wider
//! than the Go runtime would allow.

use std::time::Duration;

use function_wasm_engine::duration;

use crate::input::{Input, ModuleSource};
use crate::quantity;

/// What a request may consume: the Input's limits, admitted against the
/// engine's ceilings.
#[derive(Debug, Default, PartialEq)]
pub struct Admitted {
    pub timeout: Option<Duration>,
    pub memory_limit: Option<u64>,
}

pub fn admit(input: &Input, ceilings: &function_wasm_engine::Config) -> Result<Admitted, String> {
    if !input.composition_policy.is_empty() {
        return Err("compositionPolicy is not implemented yet in the Rust runtime".to_string());
    }
    let admitted = run_options(input, ceilings)?;
    validate_source(&input.module).map_err(|e| format!("cannot resolve module: {e}"))?;

    // The features the port does not carry yet, refused explicitly. The
    // module source shape above is already valid, so these name exactly one
    // missing feature each.
    let m = &input.module;
    if !m.from.is_empty() {
        return Err("module.from is not implemented yet in the Rust runtime".to_string());
    }
    match m.r#type.as_str() {
        "OCI" | "HTTP" => {
            return Err(format!(
                "module.type {} is not implemented yet in the Rust runtime; only Path sources are",
                m.r#type
            ));
        }
        _ => {}
    }
    if !m.manifest_path.is_empty() {
        return Err("module.manifestPath is not implemented yet in the Rust runtime".to_string());
    }
    Ok(admitted)
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
fn validate_source(src: &ModuleSource) -> Result<(), String> {
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

fn digest_is_valid(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
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
        assert_eq!(
            admit(&path_input("fn.wasm"), &ceilings()).unwrap(),
            Admitted::default()
        );
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
            (
                "CompositionPolicyNotImplemented",
                Input {
                    composition_policy: "permit(principal, action, resource);".to_string(),
                    ..path_input("fn.wasm")
                },
                "compositionPolicy is not implemented yet in the Rust runtime",
            ),
            (
                "OCINotImplemented",
                Input {
                    module: ModuleSource {
                        r#type: "OCI".to_string(),
                        oci: Some(crate::input::OciSource {
                            r#ref: format!("ghcr.io/example/fn@sha256:{}", "a".repeat(64)),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "module.type OCI is not implemented yet in the Rust runtime; only Path sources are",
            ),
        ];
        for (name, input, want) in cases {
            let err = admit(input, &ceilings()).expect_err(name);
            assert_eq!(&err, want, "{name}");
        }
    }
}
