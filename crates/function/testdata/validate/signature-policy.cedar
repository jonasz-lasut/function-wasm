// Operator signature policy fixture: require a cosign signature for every
// module pulled from within ghcr.io/secure. A repository no rule names is not
// required (default-deny for requireSignature means no requirement), so the
// per-repository decision replaces --cosign-key's all-or-nothing.
permit (
  principal,
  action == Action::"requireSignature",
  resource
)
when { resource in Repository::"ghcr.io/secure" };
