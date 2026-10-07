//! Environment bindings and step credentials - the Rust port of
//! `internal/sandbox`'s binding.go and materialize.go: the shape a manifest's
//! requires.env carries, its validation, and the resolution of admitted
//! bindings against the request's step credentials; and the step credentials
//! a manifest requires whole (requires.credentials), held against the same
//! request. Refusal strings match the Go runtime where it had the check.

use std::collections::{BTreeMap, HashMap};

use function_sdk_rust::proto::v1::{Credentials, credentials};
use serde::{Deserialize, Serialize};

/// Binds one environment variable to one key of a pipeline-step credential.
/// A binding is a requirement the module declares, never a grant: both Cedar
/// layers must permit setEnv and spendCredential before it is resolved.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EnvBinding {
    /// The variable the guest sees: an identifier, [A-Za-z_][A-Za-z0-9_]*.
    pub name: String,
    /// The step credential key that supplies the value.
    pub from_credential: CredentialKey,
}

/// Selects one key of a step credential.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CredentialKey {
    pub name: String,
    pub key: String,
}

/// Checks the shape of env bindings - a manifest's requires.env - naming a
/// wrong one as field[i]: an identifier name set at most once, and a
/// credential name and key.
pub fn validate_bindings(field: &str, bindings: &[EnvBinding]) -> Result<(), String> {
    let mut seen: HashMap<&str, String> = HashMap::new();
    for (i, b) in bindings.iter().enumerate() {
        let entry = format!("{field}[{i}]");
        if !valid_env_key(&b.name) {
            return Err(format!(
                "{entry}.name {:?} is not an identifier ([A-Za-z_][A-Za-z0-9_]*)",
                b.name
            ));
        }
        if b.from_credential.name.is_empty() {
            return Err(format!("{entry}.fromCredential.name must not be empty"));
        }
        if b.from_credential.key.is_empty() {
            return Err(format!("{entry}.fromCredential.key must not be empty"));
        }
        if let Some(prev) = seen.get(b.name.as_str()) {
            return Err(format!("{entry}: {} is already bound by {prev}", b.name));
        }
        seen.insert(&b.name, entry);
    }
    Ok(())
}

/// Checks the shape of the step credentials a module reads whole from its
/// request - a manifest's requires.credentials - naming a wrong one as
/// field[i]: a credential name, required at most once.
pub fn validate_credentials(field: &str, names: &[String]) -> Result<(), String> {
    let mut seen: HashMap<&str, String> = HashMap::new();
    for (i, name) in names.iter().enumerate() {
        let entry = format!("{field}[{i}]");
        if name.is_empty() {
            return Err(format!("{entry} must not be empty"));
        }
        if let Some(prev) = seen.get(name.as_str()) {
            return Err(format!(
                "{entry}: credential {name:?} is already required by {prev}"
            ));
        }
        seen.insert(name, entry);
    }
    Ok(())
}

/// Whether s is an environment variable identifier.
pub fn valid_env_key(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Where env bindings resolve values from.
pub struct Sources<'a> {
    /// The request's step credentials.
    pub credentials: &'a HashMap<String, Credentials>,
    /// The name of the pull credential, refused as a source: what the guest
    /// may not see in its request, it may not see in its environ either.
    pub withheld: &'a str,
}

/// Resolves a module's admitted env bindings against the request's
/// credentials and returns the resolved environment. Invariants: the pull
/// credential is refused as a source, a missing credential or key is a
/// fatal-worthy error, and a NUL byte in a value is refused (WASI cannot
/// pass it).
pub fn materialize(
    bindings: &[EnvBinding],
    src: &Sources<'_>,
) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::new();
    for (i, b) in bindings.iter().enumerate() {
        let field = format!("requires.env[{i}] ({})", b.name);
        let v = resolve_credential(&field, &b.from_credential.name, &b.from_credential.key, src)?;
        if v.contains('\0') {
            return Err(format!(
                "{field}: the value of {} contains a NUL byte, which WASI cannot pass",
                b.name
            ));
        }
        env.insert(b.name.clone(), v);
    }
    Ok(env)
}

/// Holds the step credentials a module was admitted to read whole
/// (requires.credentials) against the request: none may be the pull
/// credential, and the request must carry every one - the invariants
/// materialize keeps for an env binding's credential, so a module that
/// cannot run as declared fails before it runs, naming the credential.
pub fn check_credentials(required: &[String], src: &Sources<'_>) -> Result<(), String> {
    refuse_pull_credential(&[], required, src.withheld)?;
    for (i, name) in required.iter().enumerate() {
        if !src.credentials.contains_key(name) {
            return Err(format!(
                "requires.credentials[{i}]: the request carries no credential {name:?}; declare it on the pipeline step"
            ));
        }
    }
    Ok(())
}

/// Refuses an admitted env binding or required credential that names the
/// pull credential (withheld; empty when the module is pulled without one):
/// the secret that fetched a module never reaches it, in its environ or in
/// its request. The pull credential is named by the Input, so function
/// validate refuses this offline, in the runtime's words.
pub fn refuse_pull_credential(
    bindings: &[EnvBinding],
    required: &[String],
    withheld: &str,
) -> Result<(), String> {
    if withheld.is_empty() {
        return Ok(());
    }
    for (i, b) in bindings.iter().enumerate() {
        if b.from_credential.name == withheld {
            return Err(pull_source_refusal(
                &format!("requires.env[{i}] ({})", b.name),
                withheld,
            ));
        }
    }
    for (i, name) in required.iter().enumerate() {
        if name == withheld {
            return Err(format!(
                "requires.credentials[{i}]: credential {name:?} is the pull credential and is never forwarded to the module"
            ));
        }
    }
    Ok(())
}

fn pull_source_refusal(field: &str, cred_name: &str) -> String {
    format!(
        "{field}: credential {cred_name:?} is the pull credential and cannot be used as a source"
    )
}

fn resolve_credential(
    field: &str,
    cred_name: &str,
    key: &str,
    src: &Sources<'_>,
) -> Result<String, String> {
    if cred_name == src.withheld {
        return Err(pull_source_refusal(field, cred_name));
    }
    let Some(cred) = src.credentials.get(cred_name) else {
        return Err(format!(
            "{field}: the request carries no credential {cred_name:?}; declare it on the pipeline step"
        ));
    };
    let data = match &cred.source {
        Some(credentials::Source::CredentialData(data)) => &data.data,
        None => return Err(format!("{field}: credential {cred_name:?} has no data")),
    };
    let Some(v) = data.get(key) else {
        return Err(format!(
            "{field}: credential {cred_name:?} has no key {key:?}"
        ));
    };
    Ok(String::from_utf8_lossy(v).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use function_sdk_rust::proto::v1::CredentialData;

    fn binding(name: &str, cred: &str, key: &str) -> EnvBinding {
        EnvBinding {
            name: name.to_string(),
            from_credential: CredentialKey {
                name: cred.to_string(),
                key: key.to_string(),
            },
        }
    }

    fn credentials(name: &str, key: &str, value: &[u8]) -> HashMap<String, Credentials> {
        HashMap::from([(
            name.to_string(),
            Credentials {
                source: Some(credentials::Source::CredentialData(CredentialData {
                    data: HashMap::from([(key.to_string(), value.to_vec())]),
                })),
            },
        )])
    }

    #[test]
    fn materializes_bindings() {
        let creds = credentials("apikeys", "token", b"secret");
        let src = Sources {
            credentials: &creds,
            withheld: "",
        };
        let env = materialize(&[binding("API_TOKEN", "apikeys", "token")], &src).expect("resolve");
        assert_eq!(env.get("API_TOKEN").map(String::as_str), Some("secret"));
    }

    #[test]
    fn refusals_match_the_go_runtime() {
        let creds = credentials("apikeys", "token", b"secret");
        let cases: &[(EnvBinding, &str, &str)] = &[
            (
                binding("A", "missing", "k"),
                "",
                r#"requires.env[0] (A): the request carries no credential "missing"; declare it on the pipeline step"#,
            ),
            (
                binding("A", "apikeys", "nope"),
                "",
                r#"requires.env[0] (A): credential "apikeys" has no key "nope""#,
            ),
            (
                binding("A", "apikeys", "token"),
                "apikeys",
                r#"requires.env[0] (A): credential "apikeys" is the pull credential and cannot be used as a source"#,
            ),
        ];
        for (b, withheld, want) in cases {
            let src = Sources {
                credentials: &creds,
                withheld,
            };
            assert_eq!(
                &materialize(std::slice::from_ref(b), &src).expect_err("refuse"),
                want
            );
        }
    }

    #[test]
    fn checks_required_credentials_against_the_request() {
        let creds = credentials("cmdb", "payments", b"token");
        let src = |withheld| Sources {
            credentials: &creds,
            withheld,
        };
        assert!(check_credentials(&["cmdb".to_string()], &src("")).is_ok());
        assert!(check_credentials(&[], &src("cmdb")).is_ok());
        assert_eq!(
            check_credentials(&["cmdb".to_string(), "other".to_string()], &src(""))
                .expect_err("refuse"),
            r#"requires.credentials[1]: the request carries no credential "other"; declare it on the pipeline step"#
        );
        assert_eq!(
            check_credentials(&["cmdb".to_string()], &src("cmdb")).expect_err("refuse"),
            r#"requires.credentials[0]: credential "cmdb" is the pull credential and is never forwarded to the module"#
        );
    }

    #[test]
    fn refuses_the_pull_credential_offline() {
        let bindings = [binding("TOKEN", "registry", "password")];
        assert!(refuse_pull_credential(&bindings, &["registry".to_string()], "").is_ok());
        assert!(refuse_pull_credential(&bindings, &["cmdb".to_string()], "other").is_ok());
        // The env binding's refusal is materialize's, word for word.
        assert_eq!(
            refuse_pull_credential(&bindings, &[], "registry").expect_err("refuse"),
            r#"requires.env[0] (TOKEN): credential "registry" is the pull credential and cannot be used as a source"#
        );
        assert_eq!(
            refuse_pull_credential(
                &[],
                &["cmdb".to_string(), "registry".to_string()],
                "registry"
            )
            .expect_err("refuse"),
            r#"requires.credentials[1]: credential "registry" is the pull credential and is never forwarded to the module"#
        );
    }

    #[test]
    fn validates_credential_shapes() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(validate_credentials("requires.credentials", &names(&["cmdb", "db"])).is_ok());
        assert_eq!(
            validate_credentials("requires.credentials", &names(&["cmdb", ""]))
                .expect_err("refuse"),
            "requires.credentials[1] must not be empty"
        );
        assert_eq!(
            validate_credentials("requires.credentials", &names(&["cmdb", "db", "cmdb"]))
                .expect_err("refuse"),
            r#"requires.credentials[2]: credential "cmdb" is already required by requires.credentials[0]"#
        );
    }

    #[test]
    fn validates_binding_shapes() {
        assert!(validate_bindings("requires.env", &[binding("API_TOKEN", "c", "k")]).is_ok());
        assert_eq!(
            validate_bindings("requires.env", &[binding("1BAD", "c", "k")]).expect_err("refuse"),
            r#"requires.env[0].name "1BAD" is not an identifier ([A-Za-z_][A-Za-z0-9_]*)"#
        );
        assert_eq!(
            validate_bindings(
                "requires.env",
                &[binding("A", "c", "k"), binding("A", "c", "k2")]
            )
            .expect_err("refuse"),
            "requires.env[1]: A is already bound by requires.env[0]"
        );
    }
}
