// Operator grant policy fixture: enables every sandbox capability for any
// caller, including spending step credentials for env bindings. The Cedar
// equivalent of a fully open operator layer, for validate cases that exercise
// a fully granted module.
permit (principal, action == Action::"usePrivateTmp", resource);
permit (principal, action == Action::"setEnv", resource);
permit (principal, action == Action::"grantEgress", resource);
permit (principal, action == Action::"spendCredential", resource);
