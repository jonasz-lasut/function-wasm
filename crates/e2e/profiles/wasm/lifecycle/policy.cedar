// The lifecycle profile's operator policy: the published examples must be
// signed, and their release workflow's keyless signature counts - verified
// against the public-good trusted root the runtime fetches over TUF
// (--cosign-trust-instance), with no root file mounted. The suite's own
// registry is named by no rule, so its modules run unsigned here.
// A certificate names the repository as it was at signing time, so the
// examples published while the repository was function-wasm (through 0.6)
// keep that identity beside today's.
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/jonasz-lasut/guestfn/examples" ||
       resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples" };

permit (principal, action == Action::"trustSigner", resource)
when {
  (resource in Repository::"ghcr.io/jonasz-lasut/guestfn/examples" ||
   resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples") &&
  context.issuer == "https://token.actions.githubusercontent.com" &&
  (context.identity like "https://github.com/jonasz-lasut/guestfn/.github/workflows/publish-examples.yml@refs/heads/main" ||
   context.identity like "https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-examples.yml@refs/heads/main" ||
   context.identity like "https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-pkg.yml@refs/tags/v*")
};
