# Design records

The design documents live under `docs/` as one-pagers, one per feature,
each headed by its owner, reviewers and status (`Draft`, `Implemented` with
a revision, `Withdrawn` or `Superseded`). They describe the design in the
Go era's file layout; the design decisions hold, and the paths map to the
`crates/` of today's Rust workspace as
[AGENTS.md](https://github.com/jonasz-lasut/function-wasm/blob/main/AGENTS.md)
describes. The status below is each document's own, at the time of writing.

| design record | status |
|---|---|
| [ABI v2 and the Rust Host](../one-pager-abi-v2.md) | Implemented, revision 0.6 |
| [Admission and Inspection Tooling](../one-pager-admission-tooling.md) | Implemented, revision 1.0 |
| [Module and Compiled Artifacts Cache](../one-pager-cache.md) | Implemented, revision 2.2 |
| [Governance and Performance Additions](../one-pager-governance-perf.md) | Implemented, revision 0.2 |
| [Guest Language Support](../one-pager-language-support.md) | Implemented, revision 0.7 |
| [Local Loop and Run Diagnostics](../one-pager-local-loop.md) | Withdrawn |
| [Manifests for Manifest-less Sources](../one-pager-manifest-less-sources.md) | Implemented, revision 0.1 |
| [Module Manifest](../one-pager-module-manifest.md) | Implemented, revision 1.3 |
| [Module Source Schema](../one-pager-module-source-schema.md) | Implemented, revision 2.2 |
| [Nix Development Environment](../one-pager-nix-devenv.md) | Withdrawn |
| [Policy Engine (Cedar) Evaluation](../one-pager-policy-engine.md) | Implemented, revision 0.4 |
| [Request-Delivered Secrets and Files](../one-pager-request-secrets.md) | Superseded by the three-layer authorization model |
| [Runtime Resource Governance](../one-pager-resource-governance.md) | Implemented, revision 1.3 |
| [WASM Sandbox](../one-pager-sandbox.md) | Implemented, revision 1.8 |
| [Three-Layer Authorization Model](../one-pager-three-layer-authz.md) | Implemented, revision 1.2 |
| [Trust Model](../one-pager-trust-model.md) | Implemented, revision 1.9 |
| [Use-Case Examples](../one-pager-use-case-examples.md) | Implemented, revision 1.6 |

The guest ABI is not a one-pager: [ABI v2](../abi-v2.md) is the contract
itself, versioned on its own terms.
