// Operator signature policy fixture: require a cosign signature for every
// module under one HTTP location. An HTTP source cannot carry one, so the
// rule refuses the modules it names; a Path source has no location for a
// rule to name and is never required.
permit (
  principal,
  action == Action::"requireSignature",
  resource
)
when { resource in Repository::"https://modules.invalid/secure" };
