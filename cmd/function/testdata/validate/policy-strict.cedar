// Operator grant policy fixture: permits only a private /tmp, so default-deny
// refuses the environment and egress the operator-policy-denied Composition
// asks for - the env and egress refusals validate prints, matching the request
// path.
permit (principal, action == Action::"usePrivateTmp", resource);
