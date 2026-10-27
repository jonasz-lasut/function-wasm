// Operator signer policy fixture that trusts only main-branch runs of the
// release workflow: a signature made on a version tag is refused.
permit (principal, action == Action::"requireSignature", resource);

permit (principal, action == Action::"trustSigner", resource)
when {
  context.issuer == "https://token.actions.githubusercontent.com" &&
  context.identity like "https://github.com/example/greeter/.github/workflows/release.yml@refs/heads/main"
};
