// An operator's ceiling on what any module may read through its
// requirements: no Secrets, whatever a Composition says, and every other
// read inside the tenant's own namespace or of a cluster-scoped Crossplane
// kind. The permits beside the forbid are required: a forbid alone scopes
// the action, and a scoped action with no matching permit refuses every
// selector.
forbid (principal, action == Action::"requireResource", resource)
when { resource == Kind::"core/Secret" };

permit (principal, action == Action::"requireResource", resource)
when { context has namespace && context.namespace == principal.namespace };

permit (principal, action == Action::"requireResource", resource in Group::"apiextensions.crossplane.io");
