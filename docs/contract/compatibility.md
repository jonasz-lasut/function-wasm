# Compatibility

This page is for anyone who depends on function-wasm across releases:
composition authors, operators and module authors. It lists what's stable
throughout 1.x, what a minor release may add and how a removal is announced.

What a Composition, a flag, a dashboard or a CI job relies on keeps working
on every 1.x release, with one exception: a minor release may add a refusal
for a rule that didn't exist before.

## What's stable

- The Input, `guestfn.crossplane.io/v1`: its field names, types and
  semantics. A field the runtime doesn't know is ignored, and `function
  validate` warns naming it; the removed `policy` and `sandbox` fields it
  refuses by name. `config` is an object of `from` and `allowEmpty` since
  0.6 (#262): a value written there is refused, and the refusal states
  the field it belongs in.
- Each runtime flag's meaning and default.
- The wording of refusals and fatal results, as the conformance goldens pin
  it (`crates/function/testdata/conformance/`). Operators grep logs and XR
  conditions for these strings.
- The module manifest (`guestfn.yaml`): its fields and what each requests.
- The ABI v2 world, which has its own version (see [ABI v2](../abi-v2.md)).
- The metric names and labels (`guestfn_*`, `grpc_server_*`).
- The cache directory, `/tmp/guestfn-cache`, where an operator mounts a
  volume or an `emptyDir`.
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

ABI v1 set the precedent: deprecated in 0.6.0, removed in 1.0.0. Before
1.0 a rename may skip the deprecation step: the Input's group moved from
`wasm.fn.crossplane.io` to `guestfn.crossplane.io` outright, and the
runtime refuses the old group by name. The project's own names moved the
same way at 0.6, with no alias for the old ones: the metric series
`function_wasm_*` became `guestfn_*`, the cache directory
`/tmp/function-wasm-cache` became `/tmp/guestfn-cache` and the stamped
version variable `FUNCTION_WASM_VERSION` became `GUESTFN_VERSION`.
