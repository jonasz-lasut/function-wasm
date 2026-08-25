// A malformed SSRF CIDR rule: isMulticast is not one of the operations the
// dial decision table compiles, so the runtime (and function validate) refuses
// the policy file at load rather than compiling a table that means less than
// the operator wrote.
forbid (principal, action == Action::"dialAddress", resource)
when { context.ip.isMulticast() };
