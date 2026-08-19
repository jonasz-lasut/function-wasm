// Operator grant policy fixture: enables every sandbox capability for any
// caller. The Cedar equivalent of the old all-flags-on runtime, for validate
// cases that exercise a fully granted sandbox.
permit (principal, action == Action::"usePrivateTmp", resource);
permit (principal, action == Action::"setEnv", resource);
permit (principal, action == Action::"grantEgress", resource);
