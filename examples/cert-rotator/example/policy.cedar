// The operator grant policy of the local render (--sandbox-policy-file). The
// module requires no capability, so nothing is granted; the two rules are
// the operator's ceilings on the fences the module's output meets: a
// module may read only its own namespace's resources (the cluster-scoped
// Crossplane kinds aside) and may compose no RBAC.
permit (principal, action == Action::"requireResource", resource)
when { context has namespace && context.namespace == principal.namespace };

permit (principal, action == Action::"requireResource", resource in Group::"apiextensions.crossplane.io");

forbid (principal, action == Action::"composeResource", resource in Group::"rbac.authorization.k8s.io");

permit (principal, action == Action::"composeResource", resource);
