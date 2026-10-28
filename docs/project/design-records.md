# Design records

The design documents live under `docs/` as one-pagers, one per feature,
each headed by its owner, reviewers and status (draft, implemented with a
revision, withdrawn or superseded). They describe the design in the
Go era's file layout; the design decisions hold, and the paths map to the
`crates/` of today's Rust workspace as
[AGENTS.md](https://github.com/jonasz-lasut/function-wasm/blob/main/AGENTS.md)
describes. The status below is each document's own, at the time of writing.

| design record | status |
|---|---|
| [ABI v2 and the Rust host](../one-pager-abi-v2.md) | Implemented, revision 0.6 |
| [Admission and inspection tooling](../one-pager-admission-tooling.md) | Implemented, revision 1.0 |
| [Module and compiled artifacts cache](../one-pager-cache.md) | Implemented, revision 2.2 |
| [Governance and performance additions](../one-pager-governance-perf.md) | Implemented, revision 0.2 |
| [Guest language support](../one-pager-language-support.md) | Implemented, revision 0.7 |
| [Local loop and run diagnostics](../one-pager-local-loop.md) | Withdrawn |
| [Manifests for manifest-less sources](../one-pager-manifest-less-sources.md) | Implemented, revision 0.1 |
| [Module manifest](../one-pager-module-manifest.md) | Implemented, revision 1.3 |
| [Module source schema](../one-pager-module-source-schema.md) | Implemented, revision 2.2 |
| [Nix development environment](../one-pager-nix-devenv.md) | Withdrawn |
| [Policy engine (Cedar) evaluation](../one-pager-policy-engine.md) | Implemented, revision 0.4 |
| [Request-delivered secrets and files](../one-pager-request-secrets.md) | Superseded by the three-layer authorization model |
| [Runtime resource governance](../one-pager-resource-governance.md) | Implemented, revision 1.3 |
| [WASM sandbox](../one-pager-sandbox.md) | Implemented, revision 1.8 |
| [Three-layer authorization model](../one-pager-three-layer-authz.md) | Implemented, revision 1.2 |
| [Trust model](../one-pager-trust-model.md) | Implemented, revision 1.9 |
| [Use-case examples](../one-pager-use-case-examples.md) | Implemented, revision 1.6 |

The guest ABI isn't a one-pager: [ABI v2](../abi-v2.md) is the contract
itself, versioned on its own terms.
