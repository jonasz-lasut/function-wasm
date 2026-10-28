# Module manifests

A module published with `guestfn push` from a project that has a
`wasmfn.yaml` includes a **manifest** beside it in the same OCI artifact (a
second layer, `application/vnd.wasmfn.manifest.v1+json`, covered by the
manifest digest the Composition pins and by a cosign signature): the
sandbox capabilities it can't run without (`requires`: egress rules,
`filesystem.privateTmp`, `env` credential bindings, the step `credentials`
it reads whole from its request - non-secret configuration is the Input's
`config`), a JSON Schema for its `config`,
its ABI and the oldest runtime that serves it. The runtime reads it once
per digest (into `/tmp/function-wasm-cache/manifests`) and, after
admission and load, decides each `requires` entry by the three-layer rule -
the manifest requests, the `compositionPolicy` and the operator's
`--sandbox-policy-file` permit - which is **narrowing only**. A manifest can
make a run fail earlier and say why, it can never make a run possible or
widen a grant. A required capability a layer doesn't permit is a fatal
result before the module runs - `module oci ghcr.io/example/greeter@sha256:… requires egress GET to host "api.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit`,
`… requires a private /tmp (requires.filesystem.privateTmp), which the compositionPolicy does not permit for this request`,
`… requires credential "cmdb" (requires.credentials[0]), which the operator policy (--sandbox-policy-file) does not permit`,
`… requires runtime v0.3.0 or newer, this is v0.2.1` - and so is a
`config` outside the schema: `… config does not match the module's schema:
/greeting: got number, want string`. A module without a manifest gets the
default sandbox (nothing but the request, with no step credential); a `path` or `http` source has no
OCI manifest layer but may point at its `wasmfn.yaml` by reference
(`module.manifestPath`, `module.http.manifestURL`/`manifestDigest`) to get
one too. `guestfn push` prints the `requires:` block under the `module:`
block, `guestfn inspect <ref>` shows what a module requires, and `function
validate --resolve` applies the same check offline.

What each policy layer can say about a required capability (the actions
`usePrivateTmp`, `setEnv`, `spendCredential` and `grantEgress`, the resource
and context each decides on, and the exact sentence a refusal produces) is
in the [Cedar policy reference](../operators/cedar-policy.md).

## The `composes` declaration

A manifest may also declare what the module writes, beside what it needs:

```yaml
composes:
  - kind: policy/PodDisruptionBudget   # <group>/<Kind>, the core group spelled core
    changes: [add]                     # add, modify, remove; all three when omitted
```

The declaration is advisory. The policy layers fence a module's output
with `composeResource` rules judged over its actual response after the
run ([the output fence](../operators/cedar-policy.md#the-output-fence));
the runtime never reads `composes`, so a declaration can neither make an
output pass nor refuse a run early. It feeds the tooling: `function
validate --resolve` warns where a declared kind and change would be
refused by the step's `compositionPolicy` or the operator policy (`the
module declares it composes policy/PodDisruptionBudget (add), which the
compositionPolicy does not permit (composeResource)`), `guestfn inspect`
shows it in the manifest summary, and `guestfn scaffold composition`
writes the matching `composeResource` permits into the `compositionPolicy`
skeleton it already derives from `requires`. A field inside `composes` the
runtime doesn't know is refused by `guestfn build` (a typo in the file you
are editing) and ignored by a runtime reading a published manifest (it's
advisory there).
