// The lifecycle profile's operator policy: the published examples must be
// signed, and the keyless signature of their Publish Examples workflow
// counts - verified against the public-good trusted root the runtime
// fetches over TUF (--cosign-trust-instance), with no root file mounted.
// The suite's own registry is named by no rule, so its modules run
// unsigned here.
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/jonasz-lasut/guestfn/examples" };

permit (principal, action == Action::"trustSigner", resource)
when {
  resource in Repository::"ghcr.io/jonasz-lasut/guestfn/examples" &&
  context.issuer == "https://token.actions.githubusercontent.com" &&
  context.identity == "https://github.com/jonasz-lasut/guestfn/.github/workflows/publish-examples.yml@refs/heads/main"
};
