# Cedar policy reference

This page is for operators writing the policy file that `--sandbox-policy-file`
names and for composition authors writing a step's `compositionPolicy`. It
assumes you know Cedar's `permit` and `forbid` syntax and the
[module manifest](../guests/module-manifests.md)'s `requires`. Use it to write a
rule and to read the refusal it produces. Mounting the operator's file is in the
[operator grant policy](grant-policy.md), signing setup in
[signatures](signatures.md) and egress budgets in [HTTP egress](http-egress.md).

A module gets a capability beyond the default sandbox only when three layers
agree: its manifest requests it (`requires`), the Input's `compositionPolicy`
permits it and the operator's policy file permits it. Both policy layers are
[Cedar](https://www.cedarpolicy.com) documents over one schema. A rule means the
same in both, and a layer can only narrow what the layer before it allows. The
[trust model](trust-model.md) explains the split.

## A minimal policy

With this operator policy, modules in the `team-a` namespace can use a private `/tmp`
and send GET requests to `example.com` and its subdomains:

```cedar
permit (principal, action == Action::"usePrivateTmp", resource)
when { principal.namespace == "team-a" };

permit (principal, action == Action::"grantEgress", resource)
when { resource in HostPattern::"example.com" &&
       context.method == "GET" };
```

A `compositionPolicy` is written the same way. The rest of this page defines
the names these rules use.

## The layers

| layer | written by | actions it may use |
|---|---|---|
| operator policy | the operator | `usePrivateTmp`, `setEnv`, `spendCredential`, `grantEgress`, `composeResource`, `requireResource`, `requireSignature`, `trustSigner`, `dialAddress` |
| composition policy | the composition author | `pullModule`, `spendCredential`, `usePrivateTmp`, `setEnv`, `grantEgress`, `composeResource`, `requireResource` |

The runtime compiles the operator policy at startup and doesn't reread it. A
change needs a restart. The composition policy is the Input's
`compositionPolicy` string, compiled per Composition and cached by content hash.
The [Input reference](input-reference.md) says where it goes in the Input and
that the runtime reads it from the Input only, never from the composite
resource.

`requireSignature`, `trustSigner` and `dialAddress` are the operator's alone:
the runtime never evaluates a `compositionPolicy` rule for them. `pullModule` is
the composition author's alone. The operator policy has no say in where an
XR-chosen module comes from, only in what it's granted once chosen.

## The principal

The runtime decides each request for the principal `Request::"self"`, the caller
of the step. Its attributes are strings:

| attribute | value |
|---|---|
| `principal.namespace` | the observed composite resource's `metadata.namespace` |
| `principal.xrKind` | the observed composite resource's `kind` |
| `principal.composition` | always empty: a `RunFunctionRequest` doesn't include the Composition name |

A request with no observed composite resource gets the zero principal, with
every attribute empty. It matches no condition that tests one, and that's safe
because both layers only narrow. `function validate` builds the principal from
`--xr` and uses the zero principal without it.

`requireSignature` and `trustSigner` see the placeholder principal
`Module::"module"`, which has no attributes. A rule for either that conditions
on `principal` never matches.

## Entity types

Rules name entities in their `resource` scope, and `in` follows each type's
hierarchy. `in` is reflexive: an entity is `in` itself.

- `Repository` is a module's location without its tag or digest. An OCI
  reference is `registry/repository`, normalized as go-containerregistry does
  (`docker.io/x` is `index.docker.io/x`), with the registry in its
  [canonical form](#the-canonical-registry). For an HTTP URL, it's `scheme://host/path`
  with a lowercase host and without its query. Every path-boundary prefix is an
  ancestor, with and without its trailing slash: `ghcr.io/team` is an ancestor
  of `ghcr.io/team/mod`, and `ghcr.io/team-other` and `ghcr.io/teamsuffix`
  aren't.
- `HostPattern` is an egress rule's `host`, in lowercase without a trailing dot,
  or its `hostPattern` as written (`*.example.com`). Its `host` attribute is the
  same string, for `like`. The DNS label suffixes are ancestors:
  `api.example.com` and `*.example.com` are both `in HostPattern::"example.com"`
  and `in HostPattern::"com"`. `example.com.attacker.net` and `badexample.com`
  aren't.
- `Capability` is `privateTmp` or `env`. `Credential` is the name of a
  pipeline-step credential.
- `Kind` is `<group>/<Kind>` of a resource a module composed, or asked
  Crossplane for, read from its `apiVersion` and `kind`: `apps/Deployment`,
  `policy/PodDisruptionBudget`, `core/Secret` (the core group is spelled
  `core`). The API version is dropped, so `policy/v1` and `policy/v1beta1` are
  one kind.
- `Group` is an API group (`apps`, `policy`, `core`) and the parent of every
  `Kind` in it: `resource in Group::"apps"` is every kind of the apps group and
  no other.

### The canonical registry

A registry has one spelling in a policy, whichever way a reference writes
it. The runtime reads the registry part of every OCI reference into its canonical
form before it builds the `Repository` entity, so a rule over
`Repository::"ghcr.io/acme/prod"` covers `GHCR.IO/acme/prod/m`,
`ghcr.io:443/acme/prod/m` and `ghcr.io./acme/prod/m` alike. Write the
canonical form in a rule. A rule written with another spelling doesn't match
any module.

- The host is lowercase.
- The host has no trailing dot.
- The port is absent when it's the default of the scheme the runtime uses:
  `443`, or `80` for `localhost` and the loopback addresses, which the runtime
  reaches over plain HTTP. Any other port is part of the name:
  `registry.example.org:5000` and `registry.example.org` are two registries.
- An IPv6 address is in its shortest form, in brackets: `[::1]:5000`.
- Docker Hub is `index.docker.io`, whether the reference says `docker.io` or
  `index.docker.io`, or leaves the registry out. `docker.io` is the one alias.
  The runtime doesn't rewrite any other host: `registry-1.docker.io` stays
  `registry-1.docker.io`, and a rule over `index.docker.io` doesn't cover it.

A single-label host keeps the port or the dot that marks it as a registry:
`registry:443/team/fn` stays `registry:443`, since `registry/team/fn` is a
Docker Hub repository.

The runtime refuses a registry that isn't a host name or an IP address with
an optional port. It refuses user information (`user@ghcr.io`), a character
a host can't contain, an empty label, and a shorthand IP address such as
`127.1`. The refusals, the logs and `function validate` print a module's
canonical reference.

## Actions

Each action takes one resource type and a context. The table lists them and the
notes below it cover what a cell can't:

| action | layer | resource | context | what a matching `permit` grants |
|---|---|---|---|---|
| `pullModule` | composition | `Repository` | none | an XR-chosen source (`module.from`) may be fetched from that location, and an XR-chosen HTTP source's `manifestURL` is fenced by the same rules |
| `spendCredential` | both | `Credential` | `repository`, for an XR-chosen pull credential only | the credential an env binding reads, one a module reads whole (`requires.credentials`) or an XR-chosen module's pull credential |
| `usePrivateTmp` | both | `Capability::"privateTmp"` | none | the private `/tmp` the manifest requires |
| `setEnv` | both | `Capability::"env"` | `keys`, the set of every environment variable name the manifest binds | the env bindings as a whole |
| `grantEgress` | both | `HostPattern` | `method` (upper case) and `path` (the rule's `pathPrefix`, empty when it has none) | one rule's host and method |
| `composeResource` | both | `Kind` | `change`, `name` and `target` | nothing: it fences the module's output |
| `requireResource` | both | `Kind` | `name`, `labels` and `namespace`, each present only where the selector sets it | nothing: it fences what the module asks Crossplane for |
| `requireSignature` | operator | `Repository` | none | nothing: it demands a cosign signature for modules at that location |
| `trustSigner` | operator | `Repository` | `issuer` and `identity` | nothing by itself: it makes a keyless signer trusted for `requireSignature` |
| `dialAddress` | operator | none | `ip` | nothing at request time: it adds to the egress block list |

- `spendCredential` has `context.repository`, the module's `Repository` entity,
  only for an XR-chosen OCI source's pull credential in the composition layer.
  It's absent for an env binding's credential and for `requires.credentials`, so
  test it with `context has repository`.
- `setEnv` is decided once for all bindings, and `spendCredential` then decides
  each binding's credential.
- `grantEgress` is decided once per rule and per method the rule lists.
- `composeResource` is decided once per change the module made: `change` is
  `add`, `modify` or `remove`, `name` is the composition resource name (empty
  for the composite resource) and `target` is `composite` or `composed`. See
  [the output fence](#the-output-fence).
- `requireResource` is decided once per resource selector in the response's
  `requirements`: `name` is the selector's `matchName`, `labels` is a record
  of its `matchLabels` with the label keys as attributes
  (`context.labels has team`, `context.labels["app.kubernetes.io/part-of"]`),
  and `namespace` is the selector's namespace. A selector matches by name or
  by labels, never both, and an attribute the selector doesn't set is absent,
  so test it with `has` first. See [the read fence](#the-read-fence).
- `trustSigner`'s `issuer` is the OIDC issuer of a keyless signature's Fulcio
  certificate and `identity` its subject alternative name, both strings for `==`
  and `like`. [Signatures](signatures.md) has the precedence with `--cosign-key`.
- `dialAddress` must leave `principal` and `resource` unconstrained. Its `ip` is
  the resolved address, tested with `isInRange(ip("CIDR"))` or `isLoopback()`. A
  `forbid` adds a blocked prefix and a `permit` a hole in the default list.

## Evaluation per layer

### The operator layer

The operator layer is default-deny, as Cedar evaluates it: a request is
permitted when at least one `permit` matches and no `forbid` does. A policy that
permits one capability still refuses every other capability a module requires.
With no `--sandbox-policy-file` nothing is grantable, and a module that requires
anything is refused with a sentence that mentions the flag.

### The composition layer

The composition layer evaluates the same way, in two regimes.

The `module.from` fences are default-deny. A module the composite resource
chooses from an OCI or HTTP source must be permitted by a `pullModule` rule, and
an Input with no `compositionPolicy` is refused for such a source. An XR-chosen
OCI source may specify a step credential only where a `spendCredential` rule
permits it, and `context.repository` lets one rule cover both halves. A static
source, specified in the Input, isn't fenced.

The sandbox actions are scoped default-permit. The layer narrows `usePrivateTmp`,
`setEnv`, `grantEgress` and `spendCredential` (for env bindings and
`requires.credentials`) only when one of its rules, `permit` or `forbid`, scopes
that action with `action == Action::"setEnv"` or
`action in [Action::"setEnv", ...]`. Until then the manifest and the operator
decide that action alone. Once scoped, a matching `permit` and no matching
`forbid` allow it. A rule with an unconstrained action
(`permit (principal, action, resource);`) scopes nothing. A `forbid` alone scopes
an action and permits nothing. A ceiling needs a `permit` beside it.

`composeResource` and `requireResource` follow the same rule in both layers.
They're the two actions where the operator layer isn't default-deny.

The runtime evaluates the composition layer first and whole. A composition
author reads their own layer's refusal even where the operator would also deny.
A `compositionPolicy` that isn't valid Cedar is refused at admission, before
anything is resolved.

### The output fence

`composeResource` fences what a module writes. It's default-permit in both
layers because a default-deny would have refused every module's output on
upgrade. After the run, the runtime compares the response's desired state with the
request's by composition resource name and judges every change:

- A name the response has and the request didn't is an `add`.
- A name both have whose resource differs is a `modify`. The comparison is by
  value over the body, connection details and ready flag, so a guest that
  re-encodes a map in another order doesn't change it.
- A name the request had and the response dropped is a `remove`. A response with
  no desired state drops everything.
- A kind replaced under one name is a `remove` of the old kind and an `add` of
  the new one.

The composite resource's own desired state is judged as
`Kind::"<xr group>/<xrKind>"` with `context.target == "composite"` and an empty
name. A status write is a `modify`.

Every layer that scopes the action must permit each change. A refusal is a fatal
result that states the module, the kind, the name, the change and the layer, and
none of the module's output is applied. The runtime doesn't judge a response that
already contains a fatal result.

The fence judges the desired state. What a module asks Crossplane for is
judged by [the read fence](#the-read-fence). A module can also write `context`
and `conditions`, which neither fence sees.

### The read fence

`requireResource` fences what a module asks Crossplane for. Through the
response's `requirements` a module can request any resource the Function's RBAC
lets Crossplane read, and Crossplane returns it in the next request's
`required_resources`. After the run, the runtime judges every
`ResourceSelector` in `requirements.resources` and in the deprecated
`requirements.extra_resources`, in key order with the current map first, as
`Kind::"<group>/<Kind>"` with the selector's `name`, `labels` and `namespace`
in the context where the selector sets them. The runtime judges the ask, since
the ask is what it sees. Crossplane reads the resources after the response.

The fence is scoped default-permit in both layers, as the output fence is and
for the same reason: nothing judged the ask before, and a default-deny would
refuse every module that reads an EnvironmentConfig on upgrade. Every layer
that scopes the action must permit each selector. A refusal is a fatal result
that states the module, the kind, the name or labels, the namespace where set,
the requirements entry and the layer. The runtime doesn't relay the refused
ask, so Crossplane doesn't fetch. Unlike the output fence, the read fence judges
a response with a fatal result too, because the ask travels whatever the
results say. A selector
with no `apiVersion` or `kind` can't be judged by any rule and is refused with
its own sentence where a layer scopes the action.

The runtime judges the requirements before it diffs the desired state, so a
response with a refused selector and a refused kind reports the selector.
`requirements.schemas`, the CRD schemas a module may request, isn't fenced,
since a schema is public metadata of a kind. The request's
`required_resources` isn't edited either: it holds what Crossplane fetched for
an ask the runtime permitted on the previous call. Scoping the Function's RBAC
remains the outer boundary, per Function; the fence narrows it per Composition
and per tenant.

## Worked examples

One rule per action. The refusal each produces is in
[Refusal sentences](#refusal-sentences).

### `pullModule`

```cedar
permit (principal, action == Action::"pullModule",
        resource in Repository::"ghcr.io/example");
```

This permits an XR-chosen module at `ghcr.io/example/greeter` and refuses
`ghcr.io/example-evil/greeter`: the hierarchy follows path boundaries. A `principal.namespace` condition narrows it per tenant.

### `spendCredential`

In a `compositionPolicy`, for the pull credential of an XR-chosen module:

```cedar
permit (principal, action == Action::"spendCredential",
        resource == Credential::"regcred")
when { context has repository &&
       context.repository in Repository::"ghcr.io/example" };
```

This permits `regcred` for a module chosen from `ghcr.io/example/greeter`. It
refuses `ghcr.io/other/greeter`, and it refuses an env binding or a
`requires.credentials` entry that names `regcred`, because neither has a
repository. Permit those with a second rule that doesn't test
`context.repository`.

In the operator policy, which never sees a repository:

```cedar
permit (principal, action == Action::"spendCredential",
        resource == Credential::"cmdb");
```

A module whose manifest lists `credentials: [cmdb, db]` is refused for `db`.

### `setEnv`

```cedar
permit (principal, action == Action::"setEnv", resource)
when { principal.namespace == "team-a" &&
       context.keys.contains("DATABASE_URL") };
```

This permits the env bindings as a set for `team-a` when `DATABASE_URL` is among
the bound names. Each binding's credential still needs a `spendCredential`
permit in the same layer.

### `usePrivateTmp` and `grantEgress`

The [minimal policy](#a-minimal-policy) shows both. In a `compositionPolicy`, a
`usePrivateTmp` rule scopes the action and narrows it for every request of the
step. A `grantEgress` permit for `example.com` covers
`api.example.com`, `example.com` and `*.example.com` with `GET`, and refuses a
rule that lists `methods: [GET, POST]` on its `POST`. `context.path` is the
rule's `pathPrefix`, so `context.path like "/v1/*"` narrows by path.

### `requireSignature`

```cedar
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/secure" };
```

Modules pulled from under `ghcr.io/secure` need a cosign signature that a
`--cosign-key` key verifies, or a keyless one by a trusted signer. Modules from
`ghcr.io/open/greeter` run unsigned. A required HTTP source is refused before
any fetch, since only OCI artifacts can be signed. A `Path` source is never
required.

### `trustSigner`

```cedar
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/acme/modules" };

permit (principal, action == Action::"trustSigner", resource)
when {
  resource in Repository::"ghcr.io/acme/modules" &&
  context.issuer == "https://token.actions.githubusercontent.com" &&
  context.identity like "https://github.com/acme/modules/.github/workflows/release.yml@refs/tags/v*"
};
```

A keyless signature on a module under that repository counts only when its
Fulcio certificate was issued to the `acme/modules` release workflow on a version
tag, authenticated by GitHub Actions. `like` takes `*` as a wildcard. The runtime
verifies the signature against the trusted root first. The rule only sets which
verified signers are trusted, so a rule with no `when` trusts
every identity the root's authorities certified.

### `dialAddress`

```cedar
forbid (principal, action == Action::"dialAddress", resource)
when { context.ip.isInRange(ip("203.0.113.0/24")) || context.ip.isLoopback() };

permit (principal, action == Action::"dialAddress", resource)
when { context.ip.isInRange(ip("10.96.0.0/12")) };
```

The runtime judges a dialed address against an operator `forbid` first, then the
`permit` holes, then the default block list. Here the `forbid` blocks a public
range and loopback, and the `permit` opens an in-cluster range the default list
refuses. A `permit` inside a range a `forbid` covers never takes effect. A `||` is the union of its tests, a bare address literal is a
`/32` or `/128`, and a rule with no `when` applies to every address. The block
list and budgets are on the [HTTP egress](http-egress.md) page.

### `composeResource`

To confine a step to adding PodDisruptionBudgets:

```cedar
permit (principal, action == Action::"composeResource", resource in Group::"policy")
when { context.change == "add" };
```

To keep every module from composing RBAC or Secrets, an operator needs a
`permit` beside the `forbid`:

```cedar
forbid (principal, action == Action::"composeResource", resource)
when { resource in Group::"rbac.authorization.k8s.io" || resource == Kind::"core/Secret" };

permit (principal, action == Action::"composeResource", resource);
```

`function validate` can't judge output offline. It marks a step whose
`compositionPolicy` scopes the action with `composeResource` on its OK line, and
with `--resolve` it warns where a kind the module's manifest declares it composes
would be refused.

### `requireResource`

To let a step read the EnvironmentConfig its composite resource specifies:

```cedar
permit (principal, action == Action::"requireResource",
        resource == Kind::"apiextensions.crossplane.io/EnvironmentConfig")
when { context has name };
```

An operator who keeps every module's reads inside the tenant's namespace,
Secrets off limits and the cluster-scoped Crossplane kinds open:

```cedar
forbid (principal, action == Action::"requireResource", resource)
when { resource == Kind::"core/Secret" };

permit (principal, action == Action::"requireResource", resource)
when { context has namespace && context.namespace == principal.namespace };

permit (principal, action == Action::"requireResource",
        resource in Group::"apiextensions.crossplane.io");
```

A selector that omits the namespace and selects a namespaced kind matches no
`permit`, so a module can't read across namespaces by leaving the field out.
Guard a value test with `has`. Cedar treats a rule that reads an absent
attribute as unmatched, and an unmatched `forbid` is no fence.

`function validate` can't judge an ask offline. It marks a step whose
`compositionPolicy` scopes the action with `requireResource` on its OK line, and
with `--resolve` it warns where a kind the module's manifest declares it
[requires](../guests/module-manifests.md#the-requiresresources-declaration) would be refused.

## Refusal sentences

A refused capability is a fatal result that states the module (`module oci
ghcr.io/example/greeter@sha256:… requires …`) and the layer that refused it.
`function validate` prints the same sentences under the step. This table gives
the sentence for each `requires` entry, by who refuses it:

| `requires` entry | no `--sandbox-policy-file` | the composition layer refuses | the operator layer refuses |
|---|---|---|---|
| `requires.filesystem.privateTmp` | `requires a private /tmp (requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities` | `requires a private /tmp (requires.filesystem.privateTmp), which the compositionPolicy does not permit for this request` | `requires a private /tmp (requires.filesystem.privateTmp), which the operator policy (--sandbox-policy-file) does not permit for this request` |
| `requires.egress.http[i]`, per method | `requires egress (requires.egress.http), but the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)` | `requires egress GET to host "api.example.com" (requires.egress.http[0]), which the compositionPolicy does not permit` | `requires egress GET to host "api.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit` |
| `requires.env` (`setEnv`) | `requires env [API_TOKEN] (requires.env), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities` | `requires env [API_TOKEN] (requires.env), which the compositionPolicy does not permit (setEnv)` | `requires env [API_TOKEN] (requires.env), which the operator policy (--sandbox-policy-file) does not permit (setEnv)` |
| an env binding's credential (`spendCredential`) | as in the preceding row | `requires env API_TOKEN from credential "apikeys", which the compositionPolicy does not permit (spendCredential)` | `requires env API_TOKEN from credential "apikeys", which the operator policy (--sandbox-policy-file) does not permit (spendCredential)` |
| `requires.credentials[i]` | `requires credential "cmdb" (requires.credentials[0]), but the runtime has no --sandbox-policy-file, which is required to grant step credentials (spendCredential)` | `requires credential "cmdb" (requires.credentials[0]), which the compositionPolicy does not permit` | `requires credential "cmdb" (requires.credentials[0]), which the operator policy (--sandbox-policy-file) does not permit` |
| a change to the desired state (`composeResource`, after the run) | not refused: no policy file narrows the output | `composed apps/Deployment "deployment" (modify), which the compositionPolicy does not permit (composeResource)`; for the composite resource `composed platform.example.org/WebApp (composite, modify), which …` | `composed core/Secret "db-password" (add), which the operator policy (--sandbox-policy-file) does not permit (composeResource)` |
| a resource selector in the response's requirements (`requireResource`, after the run) | not refused: no policy file narrows the reads | `required core/Secret "db-password" in namespace "team-b" (requirements.resources["db"]), which the compositionPolicy does not permit (requireResource)`; for a match by labels `required core/Secret with labels team=a,tier=gold (requirements.resources["secrets"]), which …` | `required core/Secret "db-password" in namespace "team-b" (requirements.resources["db"]), which the operator policy (--sandbox-policy-file) does not permit (requireResource)` |

### Signature refusals

When a rule requires a signature, a module is refused with one of these:

- no key or trusted root to verify with: `cannot verify module oci
  ghcr.io/secure/greeter@sha256:…: the operator policy requires a cosign
  signature, but the runtime has neither --cosign-key nor --cosign-trusted-root
  to verify it`
- an HTTP source: `module.http "https://example.com/fn.wasm" requires a cosign
  signature (operator policy), but only OCI modules can be signature-verified`
- a verified keyless signer no rule trusts: `cannot verify module oci …: no
  valid cosign signature for <ref>: keyless signature by <identity> (issuer
  <issuer>) is not trusted for <repository>: no trustSigner rule in the
  operator policy permits it`

### The module.from fences

These refuse before the module is resolved, prefixed `cannot resolve module:`:

- no `compositionPolicy` at all: `module.from: status.module of the composite
  resource names a OCI source, but the Input has no compositionPolicy: a module
  the composite resource chooses must be permitted by the compositionPolicy's
  pullModule rules, or its author could point the runtime at any host`
- no matching `pullModule` permit: `module.from: status.other of the composite
  resource names ref "index.docker.io/someone/else", which the compositionPolicy
  does not permit (pullModule)` (or `names manifestURL "…"` for an HTTP
  source's manifest)
- a pull credential no `spendCredential` permit covers: `module.from:
  status.module of the composite resource names credentials "registry", which
  the compositionPolicy does not permit (spendCredential) for
  "ghcr.io/example/greeter": a module chosen by the composite resource cannot
  spend a step credential (the registry host would be its author's) unless the
  compositionPolicy permits it for that repository; otherwise pull it with the
  runtime's Docker config or anonymously`

A `compositionPolicy` that doesn't parse is `compositionPolicy is invalid:
cannot compile the compositionPolicy as Cedar: <Cedar's error>`.

## Load-time errors

The runtime compiles the operator policy when it starts, and `function validate`
does when it runs. A document that can't be compiled stops both. The runtime
prints the error and exits 1. `function validate` prefixes it with `function
validate: ` and exits 2.

- `cannot read operator policy: open <path>: <error>`
- `cannot compile the operator policy <path>: <Cedar's parse error>`
- `operator policy: dialAddress rule "<id>": <reason>`, where `<id>` is the
  rule's id as Cedar numbers the file (`policy0` for the first rule) and the
  reason is one of:
  - `must scope the action as == Action::"dialAddress", not an "in" set`
  - `must not constrain the principal or resource: the dial has neither identity`
  - `takes at most one when condition`
  - `may only use a when condition, not unless`
  - `the condition must be a single ip test`
  - `unsupported operation "isMulticast" (an ip test uses isInRange, isLoopback or ||)`
  - `an ip test may only test context.ip`
  - `isInRange takes context.ip and one ip("CIDR") literal`, `isLoopback
    takes context.ip and no argument`, `isInRange takes one ip("CIDR")
    literal`, or `"<value>" is not an ip literal: <reason>`

A policy with a `usePrivateTmp` rule makes the runtime create a temporary
directory under `TMPDIR` at startup, and exit 1 if it can't: `the operator
policy grants a private /tmp (usePrivateTmp), but the runtime cannot create one
under <dir>: <error>`. `function validate` doesn't run this check. The startup
warnings about a cosign flag without its matching policy rule are on the
[signatures page](signatures.md#startup-warnings).

## Testing a policy

`function validate` applies both layers to a Composition offline, with the flags
the runtime is started with:

```shell
function validate composition.yaml \
  --sandbox-policy-file policy.cedar \
  --xr xr.yaml \
  --resolve
```

`--xr` gives the principal, and without it `function validate` uses the zero
principal. The options, output and exit codes are on the
[offline validation page](../guests/validate.md).

## Further reading

The design record is the
[three-layer authorization model](../one-pager-three-layer-authz.md).
