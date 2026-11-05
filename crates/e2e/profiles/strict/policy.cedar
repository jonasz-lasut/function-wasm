// The strict profile's operator policy: what a narrow operator layer
// refuses. Only the plain variant must be signed - the runtime has no
// verifier in this profile, so that is the no-verifier refusal - while the
// other variants load and are then refused by what they require: egress is
// granted to the registry host but no dialAddress rule opens the service
// CIDR, so the dial itself is refused by the block list; env, credentials
// and the private /tmp are never granted; and the read fence's operator
// ceiling lets a module ask Crossplane only for resources in its own
// namespace, so the guest's cluster-scoped EnvironmentConfig ask is refused
// with the operator layer's sentence.
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"registry.e2e.svc:5000/e2e/plain" };

permit (principal, action == Action::"grantEgress", resource == HostPattern::"registry.e2e.svc")
when { context.method == "GET" };

// The one permit scopes requireResource: every selector must match it, and
// a selector with no namespace matches nothing.
permit (principal, action == Action::"requireResource", resource)
when { context has namespace && context.namespace == principal.namespace };
