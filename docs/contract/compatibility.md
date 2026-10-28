# Compatibility

function-wasm 1.x is one line with one promise. What a Composition, an
operator's flags, a dashboard or a CI job depends on today keeps working on
every 1.x release. Stable for the whole line:

- the Input, `wasm.fn.crossplane.io/v1`: its field names, types and
  semantics, and what becomes of a field the runtime doesn't know (the
  runtime ignores it and `function validate` warns naming it; `function
  validate` refuses the removed `policy` and `sandbox` fields by name, and
  the runtime ignores them as it does any unknown field);
- the runtime flags, what each means and its default;
- the wording of refusals and fatal results, as the conformance goldens pin
  it (`crates/function/testdata/conformance/`; a golden changes only by a
  deliberate re-record, `UPDATE_GOLDENS=1`, called out in the commit):
  operators grep logs and XR conditions for these strings;
- the module manifest (`wasmfn.yaml`): its fields and what each one
  requests;
- the ABI v2 world, versioned on its own terms in
  [`docs/abi-v2.md`](../abi-v2.md);
- the metric names and labels (`function_wasm_*`, `grpc_server_*`);
- `function validate`: its exit codes (0 admitted, 1 refused, 2 the tool
  failed) and the field names of its `--output json`;
- the `guestfn` commands, their arguments and flags.

A minor release may add to any of these without changing what's there. An
addition is an Input field, a flag, a manifest field, a refusal for a rule
that didn't exist or a metric. A minor release may also widen what's
accepted, so a document one release refused is admitted by the next. A
major release is what it takes to remove or rename any of them or to change
a pinned wording or a default. Narrowing what's accepted takes a major
release too. What the list doesn't name (the layout of a log line, the
on-disk cache format, when a sweep runs) isn't promised.

## Deprecation policy

Removing anything on the list takes two steps. A deprecated thing keeps
working for at least one minor release, with a warning under the steps that
use it in `function validate` and one runtime log line per load or request
that does, and is listed as deprecated in the release notes; the next major
removes it. `wasm.fn.crossplane.io/v1beta1` is the first instance:
accepted throughout 1.x with the warning, removed in 2.0.0. ABI v1 was the
precedent, deprecated in 0.6.0 and removed in 1.0.0.
