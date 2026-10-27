// An operator's ceiling on what any module may compose: no Secrets, no RBAC,
// whatever a Composition says. The permit beside the forbid is required: a
// forbid alone scopes the action, and a scoped action with no matching
// permit refuses every change.
forbid (principal, action == Action::"composeResource", resource)
when { resource in Group::"rbac.authorization.k8s.io" || resource == Kind::"core/Secret" };

permit (principal, action == Action::"composeResource", resource);
