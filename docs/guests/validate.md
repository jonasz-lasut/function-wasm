# Check a composition

This page is for module authors and composition authors who want to know,
before the first reconcile, whether the runtime admits a Composition's
function-wasm steps. It covers `function validate`, the `validate` subcommand of
the runtime binary. It doesn't run a module.

Crossplane never installs a function's Input CRD, so the runtime is the only
gate an [Input](../operators/input-reference.md) passes. `function validate`
runs that gate offline. It applies the same checks to every function-wasm step
of the Compositions (or bare `Input` documents) you give it.

It takes the same flags as the serving runtime, such as `--module-timeout`,
`--module-memory-limit`, `--sandbox-policy-file` and `--cosign-key`. Pass the
flags your pod runs with, or the verdict can differ from the cluster's.

```shell
cargo run --release -p function-wasm -- validate \
  example/composition.yaml --module-dir=. --resolve
# or, with the released image (its entrypoint is the runtime):
docker run --rm -v "$PWD:/w" ghcr.io/jonasz-lasut/function-wasm:<version> validate /w/composition.yaml \
  --sandbox-policy-file /w/policy.cedar
```

Each step prints one line, with its warnings indented below. A step checked through `--resolve` also prints the module's digest and what its request contains. These lines come from the conformance fixtures, so their paths aren't a real project's:

```text
testdata/validate/credentials.yaml: Composition/credentials pipeline[1] permitted: OK (path fn.wasm, compositionPolicy)
  module: sha256:09e6ac0cbcc0abcea78afa7df96114b5e4c3bf4020eb97a9aa188e17473d9714, 255 B, ABI v2; manifest: cmdb-reader 0.1.0, credentials cmdb
  credentials: cmdb
testdata/validate/credentials.yaml: Composition/credentials pipeline[2] fenced: refused: module module file fn.wasm requires credential "cmdb" (requires.credentials[0]), which the compositionPolicy does not permit
```

The refusal is the runtime's own sentence, so a message you find here is the one the pod logs. [Troubleshooting](../operators/troubleshooting.md) explains each.

## Flags

By default `validate` checks the Input alone: the `compositionPolicy` compiles, `limits` stay within the ceilings, and the `module` source is well formed. These flags change the check or its output.

- `--resolve` resolves and fetches each module. It checks the signature against `--cosign-key` and compiles the module with Wasmtime for the runtime's own verdict on size, ABI and host imports. It then decides the manifest's `requires` as the runtime does, and lists on a `credentials:` line the step credentials the module's request would contain. With none listed, the module receives none. OCI pulls use the local Docker config and never a step credential. When the module's [manifest](module-manifests.md) declares a kind in `composes` that a policy layer would refuse, the step gets a warning such as `the module declares it composes core/Secret (add), which the compositionPolicy does not permit (composeResource)`.
- `--xr xr.yaml` materializes `module.from` sources against that composite resource, as the observed XR would. Without it, a `from` source is checked only for the `compositionPolicy` it requires, and a field the XR leaves unset under `module.allowEmpty` is reported as no module.
- `--function-name` keeps only the steps of one function.
- `--output json` prints one JSON object per step, one per line, for CI annotations.
- `-` reads stdin.

A step whose `compositionPolicy` scopes `composeResource` ends its OK line's parenthesis with `composeResource`, as in `OK (path fn.wasm, compositionPolicy, composeResource)`. The policy fences the module's output, which only a run can judge, so `validate` reports the scoping and not a verdict.

## Warnings and exit codes

Warnings never change the exit code. The runtime warns about:

- A `Path` source in a Composition: `module.type Path names a file under the runtime's --module-dir and carries no digest; a cluster Composition should pin an OCI or HTTP source by digest`
- A module that requires egress when the runtime can't verify signatures, under `--resolve`: `the module requires egress but is not signature-verified: neither --cosign-key nor --cosign-trusted-root was given`
- A limit equal to its ceiling: `limits.timeout <value> equals --module-timeout: it narrows nothing`, and the same for `limits.memory` against `--module-memory-limit`
- A field the runtime ignores: `unknown field "<name>" is ignored by the runtime`
- The deprecated `v1beta1` `apiVersion`: `apiVersion wasm.fn.crossplane.io/v1beta1 is deprecated and is removed in function-wasm 2.0.0; write apiVersion: wasm.fn.crossplane.io/v1 (the same fields)`

The exit code is:

- 0 when every step is admitted.
- 1 when at least one step is refused.
- 2 when the tool itself failed: an unreadable file, unparsable YAML or a bad flag.

To run it as part of the development loop, see [Render locally](render-locally.md).
