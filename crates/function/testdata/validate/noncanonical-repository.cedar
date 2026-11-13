// Operator policy fixture: a rule naming a repository in a spelling no
// module location has. The runtime reads every reference's registry into
// its canonical form, so this rule would cover nothing: the policy is
// refused at load, naming the spelling to write.
permit (
  principal,
  action == Action::"requireSignature",
  resource
)
when { resource in Repository::"GHCR.IO/secure" };
