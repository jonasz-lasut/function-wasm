# Compatibility

This page is for anyone who depends on function-wasm across releases:
composition authors, operators and module authors. It lists what's stable
throughout 1.x, what a minor release may add and how a removal is announced.
function-oci, the OCI runtime, is alpha and has a section of its own,
[function-oci at 0.x](#function-oci-at-0x).

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

## function-oci at 0.x

function-oci runs an OCI image in a Firecracker microVM
([Install function-oci](../oci/install.md)). It's alpha and 0.x, and
function-wasm's 1.x promise doesn't extend to it. Until function-oci's own
1.0.0, a release may break anything the package has. A release that
renames or removes something refuses the old name by name, with no
deprecation step. The design record keeps the alpha label until the
contract and the engine's flags have held through two minor
releases
([OCI runtime](../one-pager-oci-runtime.md#two-packages-one-workspace)).

What function-oci shares with function-wasm is the Input, the module
manifest and the `guestfn_*` metric names. Whether those are stable for
function-oci isn't decided: that waits for function-wasm's 1.0.0.

`oci@0.1` is the contract an image follows
([OCI modules](../oci/modules.md#the-contract)). The image's command starts
a `FunctionRunnerService` gRPC server on port 9443, and the runtime calls
`RunFunction` on it once per request, as the image's configured user, in
its configured working directory, with its configured environment plus the
`requires.env` bindings the module was granted. The process's output is its
log: a line that's a JSON object with `level` and `msg` logs at that level,
and any other line logs at info. An answer is a run, and any other ending
is a fatal result on the step.

A module's manifest declares the contract it follows, `contract: oci@0.1`,
and function-oci refuses a version its engine doesn't offer. The contract
has a version of its own, and while it's 0.x a minor version may break
images built for the one before
([Module manifests](../guests/module-manifests.md)). Its stability starts
at `oci@1.0`, with function-oci's 1.0.0.

The wording of a fatal result isn't part of the contract. `oci@0.1` says
what an ending means, and the sentence that reports it, such as `process
exited with status <n>`, may change in any release. The conformance
goldens pin none of function-oci's sentences.

Among what a 0.x release may change:

- The engine's flags and their defaults: `--vmm`, `--guest-kernel`,
  `--guest-init` and `--vm-vcpus`
  ([Install function-oci](../oci/install.md#flags)).
- The wording of the engine's fatal results and of the refusals only
  function-oci prints.
- The metrics only function-oci emits:
  `guestfn_module_snapshot_duration_seconds`, and the `snapshot` value of
  the `cache` label on `guestfn_module_cache_events_total`.
- Alpha features behind flags of their own, VM snapshots among them
  ([Per-image microVM snapshots](../one-pager-vm-snapshots.md)).
