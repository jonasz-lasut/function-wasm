# Compatibility

This page is for anyone who depends on function-wasm across releases:
composition authors, operators and module authors. It lists what's stable
throughout 1.x, what a minor release may add and how a removal is announced.

What a Composition, a flag, a dashboard or a CI job relies on keeps working
on every 1.x release, with one exception: a minor release may add a refusal
for a rule that didn't exist before.

## What's stable

- The Input, `wasm.fn.crossplane.io/v1`: its field names, types and
  semantics. A field the runtime doesn't know is ignored, and `function
  validate` warns naming it; the removed `policy` and `sandbox` fields it
  refuses by name.
- Each runtime flag's meaning and default.
- The wording of refusals and fatal results, as the conformance goldens pin
  it (`crates/function/testdata/conformance/`). Operators grep logs and XR
  conditions for these strings.
- The module manifest (`wasmfn.yaml`): its fields and what each requests.
- The ABI v2 world, which has its own version (see [ABI v2](../abi-v2.md)).
- The metric names and labels (`function_wasm_*`, `grpc_server_*`).
- `function validate`: its exit codes (0 admitted, 1 refused, 2 the tool
  failed) and the field names of `--output json`.
- The `guestfn` commands, their arguments and flags.

The log line layout, the on-disk cache format and when a sweep runs aren't
on the list, so they aren't promised.

## Minor and major releases

A minor release may add to the list without changing what's already on it:
new Input fields, flags, manifest fields and metrics, and refusals for rules
that didn't exist. It may also widen what's accepted, so a document one
release refused is admitted by the next.

Removing or renaming anything on the list takes a major release, after the
deprecation below, and so does changing a pinned wording or a default or
narrowing what's accepted.

## Deprecation policy

A removal runs in two steps:

1. The next release deprecates it. A deprecated thing keeps working for at
   least one minor release. `function validate` warns under the steps that
   use it, and the runtime logs one line per load or request that does.
2. The next major release removes it.

`wasm.fn.crossplane.io/v1beta1` is the current instance: it's accepted
throughout 1.x with the warning and removed in 2.0.0. ABI v1 set the
precedent: deprecated in 0.6.0, removed in 1.0.0.
