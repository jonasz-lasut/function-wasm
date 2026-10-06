// The e2e runtime's operator policy (--sandbox-policy-file): every module
// under localhost:5005/team-a must carry a cosign signature by the key run.sh
// generates (--cosign-key). The add-on requires no sandbox capability, so the
// policy grants none; the pull credential a WebApp names is fenced by the
// Composition's own compositionPolicy.
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"localhost:5005/team-a" };
