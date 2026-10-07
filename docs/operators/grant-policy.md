# Operator grant policy

`--sandbox-policy-file` is the **sole authority that enables a sandbox
capability**: a [Cedar](https://www.cedarpolicy.com) document, the operator's
grant policy and the top layer of the three-layer decision, that decides
*which callers* a module's manifest may be granted a private `/tmp`
(`usePrivateTmp`), environment bound to step credentials (`setEnv`,
`spendCredential`), the step credentials it reads from its request
(`spendCredential`) or egress (`grantEgress`, which is also the host
allowlist) for. It is evaluated **default-deny** (a `forbid` overrides a
`permit`): a capability no permit matches is refused. Without a
`--sandbox-policy-file` no sandbox capability is grantable at all and a runtime
offers only the default sandbox (nothing but the request). The document lives
on the operator boundary alone - a module's manifest can only request, and
the Input's `compositionPolicy` can only narrow, so neither can widen past
it.

The schema both layers share (the principal, the entity types, the seven
actions with their resources and context) and the rules by which each layer
decides are in the [Cedar policy reference](cedar-policy.md); this page is
the operator's side of it.

The principal every rule sees is the caller: `principal.namespace` and
`principal.xrKind` come from the observed composite resource (a
`RunFunctionRequest` carries no Composition name, so `principal.composition`
is presently always empty). The actions are `usePrivateTmp`, `setEnv`,
`spendCredential` and `grantEgress`; for a step credential the resource is
`Credential::"<name>"`, for egress the host or pattern within a
boundary-correct `HostPattern` hierarchy, and the context carries the method
and path. A separate, caller-independent action `requireSignature` (over the
`Repository` hierarchy) decides which repositories must carry a cosign signature
- see [signing](signatures.md). A policy that lets only `team-a` use a
private `/tmp` and lets any namespace reach `*.example.com`:

```cedar
permit (principal, action == Action::"usePrivateTmp", resource)
when { principal.namespace == "team-a" };

permit (principal, action == Action::"grantEgress", resource)
when { resource in HostPattern::"example.com" && context.method == "GET" };
```

Mount it and point the flag at it:

```yaml
spec:
  deploymentTemplate:
    spec:
      template:
        spec:
          containers:
          - name: package-runtime
            args:
            - --sandbox-policy-file=/etc/function-wasm/policy.cedar
            volumeMounts:
            - {name: policy, mountPath: /etc/function-wasm, readOnly: true}
          volumes:
          - name: policy
            configMap: {name: function-wasm-policy}
```

Because the policy is default-deny, a document that governs one capability
refuses every *other* capability a module requires unless it also
permits it: with a `--sandbox-policy-file` set, permit every capability you mean to
allow. `function validate --sandbox-policy-file …` reports the same verdicts offline
(the principal comes from `--xr`, else a zero principal that matches no
per-tenant condition).
