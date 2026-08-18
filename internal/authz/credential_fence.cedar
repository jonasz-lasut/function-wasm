// The credential fence (docs/one-pager-policy-engine.md): a step credential a
// composite-chosen module names may be spent only when the Composition's own
// policy lists it and the module's repository is within the Composition's
// repository allow list. The credential (the resource) must be in the allowed
// set - exact membership, no boundary subtlety - and the module's repository
// must lie within one of the allowed repositories, which form the same
// boundary-correct hierarchy the repository fence uses, so a credential never
// reaches a sibling namespace or an adjacent host. Both allow lists travel in
// the request context, never in this policy text, so a Composition-authored
// entry cannot inject policy.
permit (
  principal,
  action == Action::"spendCredential",
  resource
)
when {
  resource in context.allowedCredentials &&
  context.repository in context.allowedRepositories
};
