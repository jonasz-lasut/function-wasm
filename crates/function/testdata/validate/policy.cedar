// Operator grant policy fixture: permits egress and environment unconditionally
// but never a private /tmp, so default-deny leaves usePrivateTmp refused.
permit (principal, action == Action::"grantEgress", resource);
permit (principal, action == Action::"setEnv", resource);
