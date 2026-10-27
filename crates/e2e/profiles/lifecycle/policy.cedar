// The lifecycle profile's operator policy: the published examples must be
// signed, and their release workflow's keyless signature counts - verified
// against the public-good trusted root the runtime fetches over TUF
// (--cosign-trust-instance), with no root file mounted. The suite's own
// registry is named by no rule, so its modules run unsigned here.
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples" };

permit (principal, action == Action::"trustSigner", resource)
when {
  resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples" &&
  context.issuer == "https://token.actions.githubusercontent.com" &&
  context.identity like "https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-pkg.yml@refs/tags/v*"
};
