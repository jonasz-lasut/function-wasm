// Operator signer policy fixture: every module must be signed, and a keyless
// signature counts only when this organisation's release workflow made it on
// a version tag, authenticated by GitHub Actions. The resource is left
// unconstrained because the conformance registry's address is not known in
// advance; a real policy fences both rules to a Repository.
permit (principal, action == Action::"requireSignature", resource);

permit (principal, action == Action::"trustSigner", resource)
when {
  context.issuer == "https://token.actions.githubusercontent.com" &&
  context.identity like "https://github.com/example/greeter/.github/workflows/release.yml@refs/tags/v*"
};
