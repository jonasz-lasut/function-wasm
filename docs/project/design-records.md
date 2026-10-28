# Design records

The design records live under `docs/` as one-pagers, one per feature, each
headed by its owner, reviewers and status (draft, implemented with a
revision, withdrawn or superseded). Each describes the design in today's Rust workspace,
the alternatives it rejected and the questions still open. The
status below is each document's own, at the time of writing.

| design record | status |
|---|---|
| [ABI v2 and the Rust host](../one-pager-abi-v2.md) | Implemented, revision 0.7 |
| [Compiled-artifact cache key](../one-pager-cache-key.md) | Draft |
| [Module and compiled artifacts cache](../one-pager-cache.md) | Implemented, revision 2.3 |
| [Module manifest](../one-pager-module-manifest.md) | Implemented, revision 1.4 |
| [Module source schema](../one-pager-module-source-schema.md) | Implemented, revision 2.3 |
| [Runtime resource governance](../one-pager-resource-governance.md) | Implemented, revision 1.4 |
| [Three-layer authorization model](../one-pager-three-layer-authz.md) | Implemented, revision 1.3 |
| [Trust model](../one-pager-trust-model.md) | Implemented, revision 1.10 |
| [Use-case examples](../one-pager-use-case-examples.md) | Implemented, revision 1.7 |
| [WASM sandbox](../one-pager-sandbox.md) | Implemented, revision 1.9 |

The guest ABI isn't a one-pager: [ABI v2](../abi-v2.md) is the contract
itself, with its own version.
