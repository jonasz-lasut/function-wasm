// The repository fence (docs/one-pager-policy-engine.md): a module may be
// pulled when its location lies within one of the repositories the request
// lists. Repositories form an entity hierarchy - a location's ancestors are
// its path-boundary prefixes - so `in` admits a repository equal to the
// location or one that fences it at a "/", and never a sibling namespace or an
// adjacent host. The allow list travels in the request context, never in this
// policy text, so a Composition-authored entry cannot inject policy.
permit (
  principal,
  action == Action::"pullModule",
  resource
)
when { resource in context.allowedRepositories };
