//! The shape of HTTP egress rules - the subset of `internal/egress` the
//! manifest and the policy layers need: HTTPRule, its validation, and the
//! host/pattern coverage helpers manifest.Check compares grants with. The
//! egress client (SSRF judgment, budgets, audit) is not ported yet; a module
//! whose admitted rules would need it is refused by admission.

use serde::{Deserialize, Serialize};

/// Admits requests to one host or host pattern - the shape a module
/// manifest's requires.egress.http carries.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HttpRule {
    /// An exact host name, e.g. api.example.com. Exactly one of host and
    /// hostPattern is set.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub host: String,
    /// A host name with a leading wildcard label, e.g. "*.internal.example.com".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub host_pattern: String,
    /// Methods the rule admits; at least one - nothing is admitted implicitly.
    pub methods: Vec<String>,
    /// The path prefix the request path must start with; empty admits any.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub path_prefix: String,
}

const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Checks the shape of HTTP egress rules - a module manifest's
/// requires.egress.http - naming a wrong rule as field[i]. Strings match the
/// Go runtime's `egress.ValidateRules`.
pub fn validate_rules(field: &str, rules: &[HttpRule]) -> Result<(), String> {
    for (i, r) in rules.iter().enumerate() {
        if r.host.is_empty() == r.host_pattern.is_empty() {
            return Err(format!(
                "{field}[{i}] must set exactly one of host and hostPattern"
            ));
        }
        if !r.host.is_empty() && !valid_host(&r.host) {
            return Err(format!(
                "{field}[{i}].host {:?} must be a bare host name (no scheme, port, path or zone)",
                r.host
            ));
        }
        if !r.host_pattern.is_empty() && !valid_host_pattern(&r.host_pattern) {
            return Err(format!(
                "{field}[{i}].hostPattern {:?} must be a host name with one leading wildcard label, e.g. *.example.com",
                r.host_pattern
            ));
        }
        if r.methods.is_empty() {
            return Err(format!(
                "{field}[{i}].methods must list at least one method"
            ));
        }
        for m in &r.methods {
            if !METHODS.contains(&m.as_str()) {
                return Err(format!(
                    "{field}[{i}].methods: {m:?} is not one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS"
                ));
            }
        }
        if !r.path_prefix.is_empty() && !r.path_prefix.starts_with('/') {
            return Err(format!(
                "{field}[{i}].pathPrefix {:?} must start with /",
                r.path_prefix
            ));
        }
        if !crate::location::normalized_path(&r.path_prefix) {
            return Err(format!(
                "{field}[{i}].pathPrefix {:?} must be normalized (no . or .. segments, no empty segments)",
                r.path_prefix
            ));
        }
    }
    Ok(())
}

/// Lowercases a host name and drops a trailing dot, so rules compare the way
/// DNS does.
pub fn normalize_host(h: &str) -> String {
    h.trim().to_lowercase().trim_end_matches('.').to_string()
}

/// Whether h names one host the way a rule must: a bare host name that is
/// not empty once normalized.
pub fn valid_host(h: &str) -> bool {
    let n = normalize_host(h);
    !n.is_empty() && !n.contains(['*', '/', ':', '%', '@', ' ', '\t', '\n', '[', ']'])
}

/// Whether p is a host pattern a rule may carry: one leading wildcard label
/// over a valid host.
pub fn valid_host_pattern(p: &str) -> bool {
    pattern_suffix(p).is_some_and(|s| valid_host(&s[1..]))
}

/// Whether host is under pattern the way a rule's hostPattern admits it:
/// "*.example.com" covers "api.example.com", not "example.com" itself.
pub fn pattern_covers(pattern: &str, host: &str) -> bool {
    pattern_suffix(pattern).is_some_and(|s| matches_suffix(&normalize_host(host), &s))
}

/// Whether pattern sits at or under granted: "*.a.example.com" is under
/// "*.example.com", and a pattern is under itself.
pub fn pattern_under(pattern: &str, granted: &str) -> bool {
    match (pattern_suffix(pattern), pattern_suffix(granted)) {
        (Some(suffix), Some(over)) => suffix == over || suffix.ends_with(&over),
        _ => false,
    }
}

/// Turns "*.example.com" into ".example.com".
fn pattern_suffix(pattern: &str) -> Option<String> {
    let pattern = normalize_host(pattern);
    let rest = pattern.strip_prefix("*.")?;
    if rest.is_empty()
        || pattern[1..].contains(['*', '/', ':', '%', '@', ' ', '\t', '\n', '[', ']'])
    {
        return None;
    }
    Some(pattern[1..].to_string())
}

/// Whether host is under the given suffix: "a.example.com" matches
/// ".example.com", "example.com" itself does not.
fn matches_suffix(host: &str, suffix: &str) -> bool {
    host.len() > suffix.len() && host.ends_with(suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(host: &str, pattern: &str, methods: &[&str]) -> HttpRule {
        HttpRule {
            host: host.to_string(),
            host_pattern: pattern.to_string(),
            methods: methods.iter().map(|m| m.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn validates_rules_with_go_strings() {
        assert!(
            validate_rules(
                "requires.egress.http",
                &[rule("api.example.com", "", &["GET"])]
            )
            .is_ok()
        );
        let cases: &[(HttpRule, &str)] = &[
            (
                rule("", "", &["GET"]),
                "requires.egress.http[0] must set exactly one of host and hostPattern",
            ),
            (
                rule("api.example.com", "", &[]),
                "requires.egress.http[0].methods must list at least one method",
            ),
            (
                rule("api.example.com", "", &["FETCH"]),
                r#"requires.egress.http[0].methods: "FETCH" is not one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS"#,
            ),
            (
                rule("http://x", "", &["GET"]),
                r#"requires.egress.http[0].host "http://x" must be a bare host name (no scheme, port, path or zone)"#,
            ),
        ];
        for (r, want) in cases {
            assert_eq!(
                &validate_rules("requires.egress.http", std::slice::from_ref(r))
                    .expect_err("refuse"),
                want
            );
        }
    }

    #[test]
    fn patterns_cover_hosts_at_label_boundaries() {
        assert!(pattern_covers("*.example.com", "api.example.com"));
        assert!(!pattern_covers("*.example.com", "example.com"));
        assert!(pattern_under("*.a.example.com", "*.example.com"));
        assert!(pattern_under("*.example.com", "*.example.com"));
        assert!(!pattern_under("*.example.com", "*.a.example.com"));
    }
}
