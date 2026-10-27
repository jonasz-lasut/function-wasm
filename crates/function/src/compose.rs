//! The changes a module's response makes to the request's desired state -
//! what the composeResource fence judges (#174). One pure function over the
//! two decoded `State`s the runtime already holds: no Cedar, no I/O, so it
//! is tested without an engine. A `Resource` compares by value (pbjson's
//! `Struct` is a map with `PartialEq`), so a guest that re-encodes the
//! request's desired state with its maps in another order changed nothing.

use std::collections::BTreeSet;

use function_sdk_rust::proto::v1::{Resource, State};
use pbjson_types::value::Kind as ValueKind;

use crate::authz::{Change, ChangeKind};

/// The group of the core API, whose apiVersion carries no group.
pub const CORE_GROUP: &str = "core";

/// The changes `after` makes to `before`, by composition resource name over
/// `desired.resources`, then the composite resource as one more entry
/// (`composite` names its kind, from the observed composite; absent, the
/// desired bodies are read). The composite comes first, then the names
/// sorted, so the first refusal a policy produces is the same on every
/// reconcile.
///
/// A name the response has and the request did not is an add; one both have
/// whose resource differs (body, connection details or ready flag) is a
/// modify; one the request had and the response dropped is a remove - and a
/// response with no desired state at all drops every resource, since that
/// is what Crossplane would apply. A kind replaced under one name is a
/// remove of the old kind and an add of the new one, never a modify of
/// either: a permit on one kind must not hide a change to the other.
///
/// Err names a composed resource in the response whose body states no
/// apiVersion or kind, which no rule could judge. The request's resources
/// were Crossplane's, so a missing kind there is read as empty, never an
/// error.
pub fn changes(
    before: Option<&State>,
    after: Option<&State>,
    composite: Option<(&str, &str)>,
) -> Result<Vec<Change>, String> {
    let mut out = Vec::new();
    let before_composite = before.and_then(|s| s.composite.as_ref());
    let after_composite = after.and_then(|s| s.composite.as_ref());
    if let Some(change) = diff(before_composite, after_composite) {
        let (group, kind) = composite
            .map(|(g, k)| (g.to_string(), k.to_string()))
            .or_else(|| after_composite.and_then(kind_of))
            .or_else(|| before_composite.and_then(kind_of))
            .unwrap_or_default();
        out.push(Change {
            group,
            kind,
            change,
            name: String::new(),
            composite: true,
        });
    }
    let mut names = BTreeSet::new();
    for state in [before, after].into_iter().flatten() {
        names.extend(state.resources.keys().cloned());
    }
    for name in names {
        let b = before.and_then(|s| s.resources.get(&name));
        let a = after.and_then(|s| s.resources.get(&name));
        let kind_b = b.map(|r| kind_of(r).unwrap_or_default());
        let kind_a = match a {
            Some(r) => Some(kind_of(r).ok_or_else(|| {
                format!(
                    "composed resource {name:?} states no apiVersion or kind, so the policy layers cannot judge it (composeResource)"
                )
            })?),
            None => None,
        };
        match (b, a) {
            (Some(_), Some(_)) if kind_b != kind_a => {
                out.push(composed(kind_b.expect("before"), ChangeKind::Remove, &name));
                out.push(composed(kind_a.expect("after"), ChangeKind::Add, &name));
            }
            _ => {
                if let Some(change) = diff(b, a) {
                    out.push(composed(
                        kind_a.or(kind_b).expect("one side"),
                        change,
                        &name,
                    ));
                }
            }
        }
    }
    Ok(out)
}

fn composed((group, kind): (String, String), change: ChangeKind, name: &str) -> Change {
    Change {
        group,
        kind,
        change,
        name: name.to_string(),
        composite: false,
    }
}

/// What happened between two sides of one entry; None when nothing did.
fn diff(before: Option<&Resource>, after: Option<&Resource>) -> Option<ChangeKind> {
    match (before, after) {
        (None, None) => None,
        (None, Some(_)) => Some(ChangeKind::Add),
        (Some(_), None) => Some(ChangeKind::Remove),
        (Some(b), Some(a)) => (b != a).then_some(ChangeKind::Modify),
    }
}

/// The resource body's group and kind: apiVersion before its last `/`, or
/// the core group when it has none; None when either field is missing or
/// not a string.
pub fn kind_of(r: &Resource) -> Option<(String, String)> {
    let body = r.resource.as_ref()?;
    let string = |key: &str| match body.fields.get(key).and_then(|v| v.kind.as_ref()) {
        Some(ValueKind::StringValue(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    let api_version = string("apiVersion")?;
    let kind = string("kind")?;
    Some((group_of(&api_version), kind))
}

/// The group an apiVersion names: `apps/v1` is `apps`, `v1` is `core`.
pub fn group_of(api_version: &str) -> String {
    match api_version.rsplit_once('/') {
        Some((group, _)) if !group.is_empty() => group.to_string(),
        _ => CORE_GROUP.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use function_sdk_rust::resource::json_to_struct;
    use serde_json::json;

    use super::*;

    fn resource(v: serde_json::Value) -> Resource {
        Resource {
            resource: Some(json_to_struct(v.as_object().expect("object"))),
            ..Default::default()
        }
    }

    fn state(resources: &[(&str, Resource)], composite: Option<Resource>) -> State {
        State {
            composite,
            resources: resources
                .iter()
                .map(|(n, r)| (n.to_string(), r.clone()))
                .collect::<HashMap<_, _>>(),
        }
    }

    fn deployment(replicas: u64) -> Resource {
        resource(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "spec": {"replicas": replicas, "selector": {"matchLabels": {"app": "web", "tier": "front"}}}
        }))
    }

    fn secret() -> Resource {
        resource(json!({"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "db"}}))
    }

    fn pdb() -> Resource {
        resource(json!({"apiVersion": "policy/v1", "kind": "PodDisruptionBudget"}))
    }

    fn change(group: &str, kind: &str, change: ChangeKind, name: &str) -> Change {
        Change {
            group: group.to_string(),
            kind: kind.to_string(),
            change,
            name: name.to_string(),
            composite: false,
        }
    }

    #[test]
    fn groups_are_read_from_the_api_version() {
        assert_eq!(group_of("apps/v1"), "apps");
        assert_eq!(group_of("v1"), "core");
        assert_eq!(group_of("policy/v1beta1"), "policy");
        assert_eq!(group_of("nop.crossplane.io/v1alpha1"), "nop.crossplane.io");
        assert_eq!(group_of("/v1"), "core");
    }

    #[test]
    fn adds_modifies_and_removes_are_told_apart_by_name() {
        let before = state(&[("deployment", deployment(2)), ("old", secret())], None);
        let after = state(
            &[("deployment", deployment(3)), ("deployment-pdb", pdb())],
            None,
        );
        let got = changes(Some(&before), Some(&after), None).expect("changes");
        assert_eq!(
            got,
            vec![
                change("apps", "Deployment", ChangeKind::Modify, "deployment"),
                change(
                    "policy",
                    "PodDisruptionBudget",
                    ChangeKind::Add,
                    "deployment-pdb"
                ),
                change("core", "Secret", ChangeKind::Remove, "old"),
            ]
        );
    }

    #[test]
    fn a_pass_through_in_another_map_order_is_no_change() {
        let before = state(&[("deployment", deployment(2))], None);
        // The same body with its selector's labels in the other order.
        let reordered = resource(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "spec": {"selector": {"matchLabels": {"tier": "front", "app": "web"}}, "replicas": 2}
        }));
        let after = state(&[("deployment", reordered)], None);
        assert_eq!(
            changes(Some(&before), Some(&after), None).expect("changes"),
            vec![]
        );
        assert_eq!(changes(None, None, None).expect("changes"), vec![]);
    }

    #[test]
    fn a_ready_flag_or_connection_detail_alone_is_a_modify() {
        let before = state(&[("deployment", deployment(2))], None);
        let mut ready = deployment(2);
        ready.ready = function_sdk_rust::proto::v1::Ready::True as i32;
        let after = state(&[("deployment", ready)], None);
        assert_eq!(
            changes(Some(&before), Some(&after), None).expect("changes"),
            vec![change(
                "apps",
                "Deployment",
                ChangeKind::Modify,
                "deployment"
            )]
        );
        let mut detail = deployment(2);
        detail
            .connection_details
            .insert("host".to_string(), b"db".to_vec());
        let after = state(&[("deployment", detail)], None);
        assert_eq!(
            changes(Some(&before), Some(&after), None).expect("changes"),
            vec![change(
                "apps",
                "Deployment",
                ChangeKind::Modify,
                "deployment"
            )]
        );
    }

    #[test]
    fn no_desired_state_at_all_removes_everything() {
        let before = state(&[("deployment", deployment(2))], Some(deployment(1)));
        let got = changes(
            Some(&before),
            None,
            Some(("platform.example.org", "WebApp")),
        )
        .expect("changes");
        assert_eq!(
            got,
            vec![
                Change {
                    group: "platform.example.org".to_string(),
                    kind: "WebApp".to_string(),
                    change: ChangeKind::Remove,
                    name: String::new(),
                    composite: true,
                },
                change("apps", "Deployment", ChangeKind::Remove, "deployment"),
            ]
        );
    }

    #[test]
    fn a_kind_replaced_under_one_name_is_a_remove_and_an_add() {
        let before = state(&[("web", deployment(2))], None);
        let after = state(&[("web", secret())], None);
        assert_eq!(
            changes(Some(&before), Some(&after), None).expect("changes"),
            vec![
                change("apps", "Deployment", ChangeKind::Remove, "web"),
                change("core", "Secret", ChangeKind::Add, "web"),
            ]
        );
    }

    #[test]
    fn the_composite_is_judged_first_under_the_observed_kind() {
        let status = resource(json!({"status": {"policy": {"count": 1}}}));
        let after = state(&[("deployment-pdb", pdb())], Some(status));
        let got =
            changes(None, Some(&after), Some(("platform.example.org", "WebApp"))).expect("changes");
        assert_eq!(got[0].kind_id(), "platform.example.org/WebApp");
        assert_eq!(got[0].change, ChangeKind::Add);
        assert!(got[0].composite);
        assert_eq!(got[0].target(), "composite");
        assert_eq!(got[1].kind_id(), "policy/PodDisruptionBudget");
        // The same composite again is no change; a status write is a modify.
        let before = after.clone();
        let mut written = before.clone();
        written.composite = Some(resource(json!({"status": {"policy": {"count": 2}}})));
        let got = changes(
            Some(&before),
            Some(&written),
            Some(("platform.example.org", "WebApp")),
        )
        .expect("changes");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].change, ChangeKind::Modify);
        assert!(got[0].composite);
    }

    #[test]
    fn a_composed_resource_without_a_kind_is_an_error_on_the_response_side_only() {
        let bare = resource(json!({"metadata": {"name": "thing"}}));
        let after = state(&[("thing", bare.clone())], None);
        assert_eq!(
            changes(None, Some(&after), None).unwrap_err(),
            r#"composed resource "thing" states no apiVersion or kind, so the policy layers cannot judge it (composeResource)"#
        );
        // Dropped from the request side, it is a remove of an empty kind.
        let before = state(&[("thing", bare)], None);
        assert_eq!(
            changes(Some(&before), None, None).expect("changes"),
            vec![change("", "", ChangeKind::Remove, "thing")]
        );
    }
}
