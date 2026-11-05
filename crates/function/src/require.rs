//! The resource selectors a module's response asks Crossplane for - what the
//! requireResource fence judges (#208). One pure function over the decoded
//! response the runtime already holds: no Cedar, no I/O, so it is tested
//! without an engine. The runtime judges the ask because the ask is what it
//! sees: Crossplane reads the resources after the response and sends them to
//! the module in a later request's `required_resources`.

use std::collections::BTreeMap;

use function_sdk_rust::proto::v1::{ResourceSelector, RunFunctionResponse, resource_selector};

use crate::authz::{Requirement, RequirementMatch};
use crate::compose::group_of;

/// The selectors of `rsp.requirements`, in judging order: `resources` then
/// the deprecated `extra_resources`, which Crossplane still serves and which
/// a module could otherwise use to step around the fence, each map in key
/// order, so the first refusal is the same on every reconcile.
///
/// Err names a selector with no `apiVersion` or no `kind`, which no rule
/// could judge.
pub fn requirements(rsp: &RunFunctionResponse) -> Result<Vec<Requirement>, String> {
    let Some(req) = rsp.requirements.as_ref() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (field, map) in [
        ("resources", &req.resources),
        ("extra_resources", &req.extra_resources),
    ] {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        for key in keys {
            out.push(requirement(field, key, &map[key])?);
        }
    }
    Ok(out)
}

fn requirement(field: &str, key: &str, s: &ResourceSelector) -> Result<Requirement, String> {
    if s.api_version.is_empty() || s.kind.is_empty() {
        return Err(format!(
            "required resource {key:?} states no apiVersion or kind, so the policy layers cannot judge it (requireResource)"
        ));
    }
    let matches = match &s.r#match {
        Some(resource_selector::Match::MatchName(name)) => RequirementMatch::Name(name.clone()),
        Some(resource_selector::Match::MatchLabels(l)) => RequirementMatch::Labels(
            l.labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        ),
        None => RequirementMatch::None,
    };
    Ok(Requirement {
        group: group_of(&s.api_version),
        kind: s.kind.clone(),
        matches,
        // An empty namespace is no namespace: the selector matches cluster
        // scoped resources, or namespaced ones across namespaces.
        namespace: s.namespace.clone().filter(|ns| !ns.is_empty()),
        field: field.to_string(),
        key: key.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use function_sdk_rust::proto::v1::{MatchLabels, Requirements};

    fn selector(
        api_version: &str,
        kind: &str,
        m: Option<resource_selector::Match>,
    ) -> ResourceSelector {
        ResourceSelector {
            api_version: api_version.to_string(),
            kind: kind.to_string(),
            namespace: None,
            r#match: m,
        }
    }

    fn by_name(name: &str) -> Option<resource_selector::Match> {
        Some(resource_selector::Match::MatchName(name.to_string()))
    }

    fn response(
        resources: &[(&str, ResourceSelector)],
        extra: &[(&str, ResourceSelector)],
    ) -> RunFunctionResponse {
        RunFunctionResponse {
            requirements: Some(Requirements {
                resources: resources
                    .iter()
                    .map(|(k, s)| (k.to_string(), s.clone()))
                    .collect(),
                extra_resources: extra
                    .iter()
                    .map(|(k, s)| (k.to_string(), s.clone()))
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn no_requirements_is_nothing_to_judge() {
        assert_eq!(
            requirements(&RunFunctionResponse::default()).expect("ok"),
            Vec::new()
        );
        assert_eq!(requirements(&response(&[], &[])).expect("ok"), Vec::new());
    }

    #[test]
    fn selectors_come_in_key_order_the_current_map_first() {
        let mut ns = selector("v1", "Secret", by_name("db"));
        ns.namespace = Some("team-b".to_string());
        let mut empty_ns = selector("v1", "ConfigMap", None);
        empty_ns.namespace = Some(String::new());
        let labels = selector(
            "apps/v1",
            "Deployment",
            Some(resource_selector::Match::MatchLabels(MatchLabels {
                labels: [("tier", "gold"), ("team", "a")]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            })),
        );
        let rsp = response(
            &[("secret", ns), ("deployments", labels)],
            &[("legacy", empty_ns)],
        );
        let got = requirements(&rsp).expect("ok");
        assert_eq!(
            got.iter()
                .map(|r| format!("{}.{} {}", r.field, r.key, r.kind_id()))
                .collect::<Vec<_>>(),
            [
                "resources.deployments apps/Deployment",
                "resources.secret core/Secret",
                "extra_resources.legacy core/ConfigMap",
            ]
        );
        assert_eq!(
            got[0].matches,
            RequirementMatch::Labels(
                [("team", "a"), ("tier", "gold")]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            ),
            "labels in key order"
        );
        assert_eq!(got[1].matches, RequirementMatch::Name("db".to_string()));
        assert_eq!(got[1].namespace.as_deref(), Some("team-b"));
        assert_eq!(got[2].matches, RequirementMatch::None);
        assert_eq!(got[2].namespace, None, "an empty namespace is no namespace");
    }

    #[test]
    fn a_selector_with_no_kind_cannot_be_judged() {
        let rsp = response(&[("db", selector("v1", "", by_name("db")))], &[]);
        assert_eq!(
            requirements(&rsp).unwrap_err(),
            r#"required resource "db" states no apiVersion or kind, so the policy layers cannot judge it (requireResource)"#
        );
        let rsp = response(&[("db", selector("", "Secret", by_name("db")))], &[]);
        assert!(requirements(&rsp).is_err());
    }
}
