# A request, step by step

This page is for someone evaluating function-wasm. It follows one Crossplane
request through the runtime, so you can see what's checked before a module
runs, what the module can do and how a failure ends. It assumes you know
[what function-wasm is](introduction.md). Each step links to the page with the
detail.

A module gets a capability only when three layers agree. The module's
[manifest](guests/module-manifests.md) has to request it, and the
Composition's `compositionPolicy` and the operator's policy, the Cedar file
named by `--sandbox-policy-file`, both have to permit it. The
[design record](one-pager-three-layer-authz.md) has the reasoning.

## 1. Admission

The runtime decodes the Input, compiles the `compositionPolicy` and checks
`limits` against its ceilings. Malformed Cedar, or a `timeout` or `memory`
limit over its ceiling, is a fatal result. A `concurrency` limit over its
ceiling is capped, with no error. A step that takes its module from the composite
resource (`module.from`) is fenced by the `compositionPolicy`: `pullModule`
for the repository, `spendCredential` for a credential. The fields are in the
[Input reference](operators/input-reference.md).

An OCI or HTTP source is pinned by digest and a `path` source is hashed by
content, so resolving a module never waits on a registry.

## 2. Loading

Where the operator requires a signature, the runtime verifies it first, once
per manifest digest per process and before any cache
([signatures](operators/signatures.md)).

The runtime then looks the digest up in the compiled modules in memory (kept
ten minutes after last use), the Wasmtime artifacts on disk and the fetched
modules on disk. Only a module this pod has never seen is fetched, checked
against its digest and compiled. A component that fails the world's typecheck
is refused here, and so is a core module, the ABI v1 shape removed in 1.0.0.
The stores are described under
[cache storage](operators/runtime-flags.md#cache-storage). The
[cache design record](one-pager-cache.md) has the reasoning.

The runtime then checks the manifest, if the module has one, against the other
two layers. A `requires` entry that isn't permitted, or a `config` outside the
module's schema, is a fatal result before anything runs.

## 3. Running

Each request runs in a fresh instance. The module receives the caller's
request bytes, less every step credential it wasn't granted. It gets WASI
without network access. The filesystem is a private `/tmp` removed after the
request, and the environment variables are those its manifest binds to step
credentials, if the three layers granted them.

Granted HTTP requests go through the host ([HTTP egress](operators/http-egress.md)).
Guest logs go to the runtime's logger with the module reference attached, and
stdout and stderr go to the pod's log.

## 4. Response

The runtime returns the response as the module produced it. Where a policy
layer scopes `composeResource`, every change to the desired state must first
be permitted. A refused change is a fatal result that specifies the kind, the
name, the change and the layer
([Cedar policy reference](operators/cedar-policy.md#the-output-fence)).

A trap, a timeout, the memory limit or an unusable module is also a fatal
result, and the function pod keeps running. The timeout counts guest compute
only: time waiting for the host's HTTP answers is credited back. A request
whose deadline passes while it waits for a run slot gets
`waiting for a run slot: deadline exceeded` and never runs.
[Troubleshooting](operators/troubleshooting.md) lists the results and their
causes.

The full host and guest contract is in [ABI v2](abi-v2.md).
