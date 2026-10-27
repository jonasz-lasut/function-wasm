// The strict profile's operator policy: what a narrow operator layer
// refuses. Only the plain variant must be signed - the runtime has no
// verifier in this profile, so that is the no-verifier refusal - while the
// other variants load and are then refused by what they require: egress is
// granted to the registry host but no dialAddress rule opens the service
// CIDR, so the dial itself is refused by the block list; env, credentials
// and the private /tmp are never granted.
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"registry.e2e.svc:5000/e2e/plain" };

permit (principal, action == Action::"grantEgress", resource == HostPattern::"registry.e2e.svc")
when { context.method == "GET" };
