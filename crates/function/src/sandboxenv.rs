//! Environment bindings - the Rust port of `internal/sandbox`'s binding.go
//! and materialize.go: the shape a manifest's requires.env carries, its
//! validation, and the resolution of admitted bindings against the request's
//! step credentials. Refusal strings match the Go runtime.

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

fn resolve_credential(
    field: &str,
    cred_name: &str,
    key: &str,
    src: &Sources<'_>,
) -> Result<String, String> {
    if cred_name == src.withheld {
        return Err(format!(
            "{field}: credential {cred_name:?} is the pull credential and cannot be used as a source"
        ));
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
