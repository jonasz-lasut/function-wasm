# Operator grant policy

`--sandbox-policy-file` is the **one authority that enables a sandbox
capability**: a [Cedar](https://www.cedarpolicy.com) document with the
operator's grant policy, the top layer of the three-layer decision. It decides
*which callers* a module's manifest may be granted a private `/tmp`
(`usePrivateTmp`), environment bound to step credentials (`setEnv`,
`spendCredential`), the step credentials it reads from its request
(`spendCredential`) or egress (`grantEgress`, which is also the host
allowlist) for. It's evaluated **default-deny** (a `forbid` overrides a
`permit`): the runtime refuses a capability unless a permit matches it. Without a
`--sandbox-policy-file` no sandbox capability is grantable at all and a runtime
offers only the default sandbox (nothing but the request). The document belongs
to the operator boundary alone. A module's manifest can only request, and
the Input's `compositionPolicy` can only narrow. Neither can widen past
it.

The schema both layers share (the principal, the entity types, the eight
actions with their resources and context) and the rules by which each layer
decides are in the [Cedar policy reference](cedar-policy.md); this page is
the operator's side of it.

The principal every rule sees is the caller: `principal.namespace` and
`principal.xrKind` come from the observed composite resource (a
`RunFunctionRequest` doesn't include the Composition name, so `principal.composition`
is presently always empty). The capability actions are `usePrivateTmp`, `setEnv`,
`spendCredential` and `grantEgress` (`dialAddress` is on the [egress
page](http-egress.md), `requireSignature` and `trustSigner` on
[signatures](signatures.md)); for a step credential the resource is
`Credential::"<name>"`, for egress the host or pattern within a
boundary-correct `HostPattern` hierarchy, and the context carries the method
and path. A separate, caller-independent action `requireSignature` (over the
`Repository` hierarchy) decides which repositories require a cosign
signature - see [signing](signatures.md). A policy that lets only `team-a`
use a private `/tmp` and lets any namespace reach `*.example.com`:

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

One action isn't a capability: `composeResource` is the operator's ceiling
on what any module may compose, judged over the module's response after the
run, and it narrows only where a rule scopes it (a runtime with no policy
file, or a file with no such rule, serves every module's output as before).
A `forbid` alone scopes it and permits nothing. A ceiling needs a
`permit` beside it:

```cedar
forbid (principal, action == Action::"composeResource", resource)
when { resource in Group::"rbac.authorization.k8s.io" || resource == Kind::"core/Secret" };

permit (principal, action == Action::"composeResource", resource);
```

Because the policy is default-deny for capabilities, a document that governs
one capability refuses every *other* capability a module requires unless it
also permits it: with a `--sandbox-policy-file` set, permit every capability
you mean to allow. `function validate --sandbox-policy-file …` reports the same verdicts offline
(the principal comes from `--xr`, else a zero principal that matches no
per-tenant condition).
