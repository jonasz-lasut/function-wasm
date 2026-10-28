# Operator grant policy

This page is for operators of function-wasm. It shows how to write the Cedar
policy that `--sandbox-policy-file` loads, mount it into the function's pod and
check it offline with `function validate`. It covers the operator layer only:
the schema and how each layer decides are in the
[Cedar policy reference](cedar-policy.md).

The policy file is the only thing that enables a sandbox capability. A module's manifest requests a capability and the Input's `compositionPolicy` can narrow it, but neither can grant more than the operator's policy does.

Evaluation is default-deny: a capability is refused unless a `permit` matches it, and a `forbid` overrides a `permit`. Without a `--sandbox-policy-file`, no capability is grantable and every module runs in the default sandbox, which has nothing but the request. A file that permits one capability still refuses every other one, so permit each capability you mean to allow.

## Actions and principal

The capability actions are:

- `usePrivateTmp`: a private `/tmp`.
- `setEnv`: environment variables bound to step credentials.
- `spendCredential`: the step credentials a module reads.
- `grantEgress`: HTTP egress, which is also the host allowlist.

The operator layer also decides `dialAddress`, on the [egress page](http-egress.md), and `requireSignature` and `trustSigner` on [signatures](signatures.md). `composeResource`, below, fences a module's output rather than granting a capability.

The principal's attributes, and the resources and context of each action, are in the [Cedar policy reference](cedar-policy.md).

## Write the policy

This policy lets only `team-a` use a private `/tmp`, and lets any
namespace reach `*.example.com` with GET:

```cedar
permit (principal, action == Action::"usePrivateTmp", resource)
when { principal.namespace == "team-a" };

permit (principal, action == Action::"grantEgress", resource)
when { resource in HostPattern::"example.com" && context.method == "GET" };
```

## Mount it and set the flag

Put the policy in a ConfigMap in the namespace the Function runs in (here `crossplane-system`):

```shell
kubectl create configmap function-wasm-policy -n crossplane-system \
  --from-file=policy.cedar
```

Then mount it through the Function's `DeploymentRuntimeConfig`:

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

The runtime compiles the policy at startup and doesn't reread it. After changing the ConfigMap, restart the function's pod.

## Check it offline

`function validate` reports the same verdicts as the runtime, without a cluster:

```shell
function validate composition.yaml --resolve \
  --sandbox-policy-file policy.cedar --xr xr.yaml
```

The principal comes from `--xr`. Without it, the zero principal matches no per-tenant condition. The command and its other flags are in [Check a composition](../guests/validate.md).

## Fence what modules compose

`composeResource` isn't a capability. It's a ceiling on what any module may compose, judged over the response after the run. It applies only where a rule scopes it. Otherwise every module's output is applied unchanged.

A `forbid` alone scopes the action and permits nothing. A ceiling needs a `permit` beside it. This policy refuses RBAC resources and Secrets and permits everything else:

```cedar
forbid (principal, action == Action::"composeResource", resource)
when { resource in Group::"rbac.authorization.k8s.io" || resource == Kind::"core/Secret" };

permit (principal, action == Action::"composeResource", resource);
```
