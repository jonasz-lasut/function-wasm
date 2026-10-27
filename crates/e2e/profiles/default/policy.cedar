// The default profile's operator policy: everything the suite's admitted
// cases need, and nothing more.
//
// Signatures: every module from the suite's registry and from the published
// examples must be signed. A key-based signature verifies with the
// --cosign-key the profile mounts; a keyless one counts when its signer is
// trusted for the repository - the suite's release identity for the e2e
// repository, this repository's release workflow for the published examples.
permit (principal, action == Action::"requireSignature", resource)
when {
  resource in Repository::"registry.e2e.svc:5000" ||
  resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples"
};

permit (principal, action == Action::"trustSigner", resource)
when {
  resource in Repository::"registry.e2e.svc:5000/e2e" &&
  context.issuer == "https://e2e.function-wasm.io" &&
  context.identity == "https://e2e.function-wasm.io/release"
};

permit (principal, action == Action::"trustSigner", resource)
when {
  resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples" &&
  context.issuer == "https://token.actions.githubusercontent.com" &&
  context.identity like "https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-pkg.yml@refs/tags/v*"
};

// Egress: GET on the registry host, which resolves into kind's service
// CIDR - inside the default block list, so the dial needs this hole.
permit (principal, action == Action::"grantEgress", resource == HostPattern::"registry.e2e.svc")
when { context.method == "GET" };

permit (principal, action == Action::"dialAddress", resource)
when { context.ip.isInRange(ip("10.96.0.0/12")) };

// The step credential e2e, whole and through the env binding; the private
// /tmp.
permit (principal, action == Action::"spendCredential", resource == Credential::"e2e");

permit (principal, action == Action::"setEnv", resource)
when { context.keys.contains("E2E_TOKEN") };

permit (principal, action == Action::"usePrivateTmp", resource);
