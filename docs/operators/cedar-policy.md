# Cedar policy reference

function-wasm decides what a module gets beyond the default sandbox with
three AND-combined layers: the module's manifest requests a capability
(`requires`), the Input's `compositionPolicy` permits it, and the operator's
`--sandbox-policy-file` permits it. The two policy layers are
[Cedar](https://www.cedarpolicy.com) documents over one shared schema, so a
rule means the same in both; each layer can only narrow what the layer
above it allows. The [trust model](trust-model.md) says why the split is
drawn there; this page is the reference for writing the two documents.

## The two layers

| layer | where it lives | written by | default | actions it may use |
|---|---|---|---|---|
| operator policy | the file `--sandbox-policy-file` names, compiled once at startup and immutable for the process | the operator | deny for capabilities: one no `permit` matches is refused, and no file means no capability at all. The output fence (`composeResource`) narrows only where a rule scopes it | `usePrivateTmp`, `setEnv`, `spendCredential`, `grantEgress`, `composeResource`, `requireSignature`, `trustSigner`, `dialAddress` |
| composition policy | the Input's `compositionPolicy` string, compiled per Composition (cached by content hash) | the composition author | deny for the `module.from` fences (`pullModule`, and `spendCredential` for an XR-chosen pull credential); scoped default-permit for the sandbox actions and the output fence (below) | `pullModule`, `spendCredential`, `usePrivateTmp`, `setEnv`, `grantEgress`, `composeResource` |

`requireSignature`, `trustSigner` and `dialAddress` are the operator's
alone: a `compositionPolicy` rule for any of them is never evaluated. `pullModule` is the
composition author's alone: the operator policy has no say in where an
XR-chosen module comes from, only in what it is granted once chosen. Where
`compositionPolicy` sits in the Input, and that it is read from the Input
only, never from the composite resource, is in the
[Input reference](input-reference.md).

## The principal

Every decision is made for the principal `Request::"self"`, the caller of
the step, with three string attributes:

| attribute | value |
|---|---|
| `principal.namespace` | the observed composite resource's `metadata.namespace` |
| `principal.xrKind` | the observed composite resource's `kind` |
| `principal.composition` | always empty: a `RunFunctionRequest` carries no Composition name |

A request with no observed composite resource gets the zero principal,
every attribute empty, which matches no condition that tests one. That is
safe because both layers only narrow: a rule that cannot match grants
nothing. `function validate` builds the principal from `--xr`, and uses the
zero principal without it.

`requireSignature` and `trustSigner` are evaluated with a placeholder
principal (`Module::"module"`) that has no attributes: a signature
requirement and a signer's trust are caller-independent, and a rule for
either that conditions on `principal` never matches.

## Entity types

| entity type | id | attributes | hierarchy (`in`) | used by |
|---|---|---|---|---|
| `Request` | `self` | `namespace`, `xrKind`, `composition` | none | the principal of every action but `requireSignature` and `trustSigner` |
| `Repository` | a module's normalized location: `registry/repository` for an OCI reference, without its tag or digest and normalized as go-containerregistry does (`docker.io/x` is `index.docker.io/x`); `scheme://host/path` for an HTTP URL, host lowercased, no query | none | every path-boundary prefix, with and without its trailing slash: `ghcr.io/team` and `ghcr.io/team/` are ancestors of `ghcr.io/team/mod`, `ghcr.io/team-other` and `ghcr.io/teamsuffix` are not. `in` is reflexive, so an entity is `in` itself | `pullModule` (resource), `requireSignature` and `trustSigner` (resource), `spendCredential` (`context.repository`) |
| `HostPattern` | an egress rule's `host`, lowercased with any trailing dot dropped, or its `hostPattern` as written (`*.example.com`) | `host`: the same string, for `like` | the DNS label suffixes: `api.example.com` is `in HostPattern::"example.com"` and `in HostPattern::"com"`; the pattern `*.example.com` is `in HostPattern::"example.com"` too. `example.com.attacker.net` and `badexample.com` are not | `grantEgress` (resource) |
| `Capability` | `privateTmp` or `env` | none | none | `usePrivateTmp` and `setEnv` (resource) |
| `Credential` | the name of a pipeline-step credential | none | none | `spendCredential` (resource) |
| `Kind` | `<group>/<Kind>` of a resource a module composed, read from its `apiVersion` and `kind`: `apps/Deployment`, `policy/PodDisruptionBudget`; the core group is spelled `core` (`core/Secret`) and the API version is absent, so `policy/v1` and `policy/v1beta1` are one kind | none | its one `Group` | `composeResource` (resource) |
| `Group` | an API group: `apps`, `policy`, `core`, `nop.crossplane.io` | none | none | the parent of every `Kind` in it, so `resource in Group::"apps"` is every kind of the apps group and no other |
| `Module` | `module` | none | none | the placeholder principal of `requireSignature` and `trustSigner` |
| `Action` | one of the nine below | | | the action of every rule |

## Actions

| action | layer | resource | context | what a matching `permit` grants |
|---|---|---|---|---|
| `pullModule` | composition | `Repository::"<location>"` | none | an XR-chosen source (`module.from`) may be fetched from that location; an XR-chosen HTTP source's `manifestURL` is fenced by the same rules |
| `spendCredential` | both | `Credential::"<name>"` | composition layer only, and only for an XR-chosen OCI source's pull credential: `repository`, the module's `Repository` entity with its hierarchy. Absent for an env binding's credential and for `requires.credentials`, so test it with `context has repository` | the pull credential an XR-chosen module names (composition layer); the credential a `requires.env` binding reads; a credential the module reads whole (`requires.credentials`) |
| `usePrivateTmp` | both | `Capability::"privateTmp"` | none | the private `/tmp` the manifest requires |
| `setEnv` | both | `Capability::"env"` | `keys`: the set of every environment variable name the manifest binds, decided once for all of them | the env bindings as a whole; each binding's credential is then decided by `spendCredential` |
| `grantEgress` | both | `HostPattern::"<host or pattern>"` | `method` (upper case) and `path` (the rule's `pathPrefix`, empty when it has none); one decision per rule and per method it lists | that rule's (host, method) |
| `composeResource` | both | `Kind::"<group>/<Kind>"` | `change`: `add`, `modify` or `remove`; `name`: the composition resource name (empty for the composite resource); `target`: `composite` or `composed`. One decision per change the module made to the desired state | nothing: it is a fence on the module's output, judged after the run over the response (the [output fence](#the-output-fence) below). Scoped default-permit in both layers |
| `requireSignature` | operator | `Repository::"<location>"` | none (placeholder principal) | nothing: it demands that a module at that location carry a cosign signature, verified with `--cosign-key` or, keyless, with a trusted root (`--cosign-trusted-root` or `--cosign-trust-instance`) and a `trustSigner` rule, before any cache is read. The [signatures](signatures.md) page has the precedence with `--cosign-key` alone |
| `trustSigner` | operator | `Repository::"<location>"` | `issuer`, the OIDC issuer of a keyless signature's Fulcio certificate, and `identity`, its subject alternative name (a workflow URI or an email), both strings for `==` and `like` (placeholder principal) | nothing by itself: a keyless cosign signature by that signer, verified against the trusted root (`--cosign-trusted-root` or `--cosign-trust-instance`), counts as the module's signature for `requireSignature`. See [signatures](signatures.md) |
| `dialAddress` | operator | none: the rule must leave `principal` and `resource` unconstrained | `ip`, the resolved address, tested with `isInRange(ip("CIDR"))` or `isLoopback()` | nothing at request time: the rules compile at load into the egress block list, a `forbid` adding a blocked prefix and a `permit` a hole in the default list |

## How each layer decides

### The operator layer

Default-deny, as Cedar evaluates it: a request is permitted when at least
one `permit` matches and no `forbid` does. A document that governs one
capability therefore refuses every other capability a module requires
unless it permits that too. With no `--sandbox-policy-file` nothing is
grantable, and a module that requires anything is refused with a sentence
naming the flag. The file is read once, at startup; a change needs a
restart.

### The composition layer

The same evaluation, in two regimes:

- **The `module.from` fences are default-deny.** A module the composite
  resource chooses from an OCI or HTTP source must be permitted by a
  `pullModule` rule, and an Input with no `compositionPolicy` at all is
  refused for such a source. An XR-chosen OCI source may name a step
  credential only where a `spendCredential` rule permits it, with
  `context.repository` carrying the module's location so a rule can
  co-locate both halves. A static source (named in the Input) is not
  subject to the fence.
- **The sandbox actions are scoped default-permit.** The layer narrows
  `usePrivateTmp`, `setEnv`, `grantEgress` and `spendCredential` (for env
  bindings and `requires.credentials`) only when at least one of its rules,
  `permit` or `forbid`, scopes that action: `action == Action::"setEnv"`
  or `action in [Action::"setEnv", ...]`. An action no rule scopes is not
  narrowed by this layer; the manifest and the operator decide it alone.
  Once scoped, the action is decided exactly as the operator layer decides
  it: a matching `permit` and no matching `forbid`. A rule whose action is
  unconstrained (`permit (principal, action, resource);`) scopes nothing.

The composition layer is evaluated first and whole, so the author closest
to the fix reads their own layer's refusal even where the operator would
also deny. A `compositionPolicy` that is not valid Cedar is refused at
admission, before anything is resolved.

### The output fence

`composeResource` is not a capability a module requests; it is a fence on
what a module writes. After the run, the runtime compares the response's
desired state with the request's, by composition resource name, and judges
every change: a name the response has and the request did not is an `add`;
one both have whose resource differs (its body, connection details or
ready flag, compared by value, so a guest that re-encodes a map in another
order changed nothing) is a `modify`; one the request had and the response
dropped is a `remove`, and a response with no desired state at all drops
everything. A kind replaced under one name is a `remove` of the old kind and
an `add` of the new one, never a `modify` of either. The composite
resource's own desired state follows the same rules as `Kind::"<xr
group>/<xrKind>"` with `context.target == "composite"` and an empty name: a
status write is a `modify`. Each change must be permitted by every layer
that scopes the action; a refusal is a fatal result naming the module, the
kind, the name, the change and the layer, and nothing the module composed
is applied. A response that already carries a fatal result is never applied
by Crossplane, so it is not judged.

Both layers decide it **scoped default-permit**, the operator's included:
a policy with no rule scoping `composeResource` does not narrow a module's
output, and a runtime with no `--sandbox-policy-file` does not either.
That is the one action for which the operator layer is not default-deny,
because a module's output was never gated before the fence existed and a
minor may only widen ([compatibility](../contract/compatibility.md)).
Once a layer scopes the action, Cedar's usual rule applies: a matching
`permit` and no matching `forbid`. A `forbid` alone scopes the action and
permits nothing, so an operator's ceiling needs a `permit` beside it.

## Worked examples

One rule per action, in the shape the runtime's own tests hold it to.

### `pullModule`

```cedar
permit (principal, action == Action::"pullModule",
        resource in Repository::"ghcr.io/example");
```

Permits an XR-chosen module at `ghcr.io/example/greeter` and at
`ghcr.io/example` itself. Refuses `ghcr.io/example-evil/greeter`,
`ghcr.io/examplesuffix/greeter` and `evil.example.net/ghcr.io/example`: the
hierarchy is built at path boundaries, never by string prefix. A
`principal.namespace` condition narrows it per tenant like any other rule.

### `spendCredential`

In a `compositionPolicy`, for the pull credential of an XR-chosen module:

```cedar
permit (principal, action == Action::"spendCredential",
        resource == Credential::"regcred")
when { context has repository &&
       context.repository in Repository::"ghcr.io/example" };
```

Permits `regcred` for a module chosen from `ghcr.io/example/greeter`.
Refuses it for `ghcr.io/other/greeter`, refuses any other credential, and
refuses an env binding or a `requires.credentials` entry that names
`regcred`, because neither carries a repository: permit those with a second
rule that does not test `context.repository`.

In the operator policy, which never sees a repository:

```cedar
permit (principal, action == Action::"spendCredential",
        resource == Credential::"cmdb");
```

A module whose manifest lists `credentials: [cmdb, db]` is refused for the
second one: `requires credential "db" (requires.credentials[1]), which the
operator policy (--sandbox-policy-file) does not permit`.

### `usePrivateTmp`

```cedar
permit (principal, action == Action::"usePrivateTmp", resource)
when { principal.namespace == "prod" };
```

Permits a private `/tmp` for a composite resource in the `prod` namespace
and refuses one in `dev`. In a `compositionPolicy`, this rule also scopes
`usePrivateTmp`, so the layer now narrows that action for every request of
the step.

### `setEnv`

```cedar
permit (principal, action == Action::"setEnv", resource)
when { principal.namespace == "team-a" &&
       context.keys.contains("DATABASE_URL") };
```

Permits the manifest's env bindings as a set for `team-a` when
`DATABASE_URL` is among the bound names; every other namespace is refused
with `requires env [DATABASE_URL] (requires.env), which the operator policy
(--sandbox-policy-file) does not permit (setEnv)`. Each binding's
credential still needs a `spendCredential` permit in the same layer.

### `grantEgress`

```cedar
permit (principal, action == Action::"grantEgress", resource)
when { resource in HostPattern::"example.com" &&
       context.method == "GET" };
```

Permits a rule for `api.example.com`, for `example.com` and for the
pattern `*.example.com`, each with `GET`. Refuses
`example.com.attacker.net` and `badexample.com`, and refuses a rule that
lists `methods: [GET, POST]` on its `POST`: `requires egress POST to host
"api.example.com" (requires.egress.http[0]), which the operator policy
(--sandbox-policy-file) does not permit`. `context.path` is the rule's
`pathPrefix`, so `context.path like "/v1/*"` narrows by path.

### `requireSignature`

```cedar
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/secure" };
```

Every module pulled from under `ghcr.io/secure` must carry a cosign
signature that one of the `--cosign-key` keys verifies, or a keyless one
by a signer a `trustSigner` rule trusts; a module from
`ghcr.io/open/greeter` runs unsigned. With the rule but no `--cosign-key`,
a required module is refused: `cannot verify module oci
ghcr.io/secure/greeter@sha256:…: the operator policy requires a cosign
signature, but the runtime has neither --cosign-key nor --cosign-trusted-root
to verify it`. A required
HTTP source is refused before any fetch, since only OCI artifacts carry a
signature: `module.http "https://example.com/fn.wasm" requires a cosign
signature (operator policy), but only OCI modules can be
signature-verified`. A `Path` source is never required by a rule.

### `trustSigner`

```cedar
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples" };

permit (principal, action == Action::"trustSigner", resource)
when {
  resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples" &&
  context.issuer == "https://token.actions.githubusercontent.com" &&
  context.identity like "https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-pkg.yml@refs/tags/v*"
};
```

A keyless signature on a module under `ghcr.io/jonasz-lasut/wasmfn/examples`
counts only when its Fulcio certificate was issued to this repository's
release workflow running on a version tag, authenticated by GitHub Actions.
`context.identity` is the certificate's subject alternative name as Fulcio
wrote it (`https://github.com/<owner>/<repo>/.github/workflows/<file>@<ref>`
for a GitHub Actions run, an email for a human signer) and `context.issuer`
its OIDC issuer; `like` takes `*` as a wildcard. The signature is verified
first, against the trusted root (`--cosign-trusted-root`, or the one
`--cosign-trust-instance` fetches); the rule only decides whether a
verified signer is trusted for the repository, so a rule with no `when`
trusts every identity the root's authorities ever certified, which is
rarely meant. A verified signature no rule trusts is refused: `cannot
verify module oci …: no valid cosign signature for <ref>: keyless signature
by <identity> (issuer <issuer>) is not trusted for <repository>: no
trustSigner rule in the operator policy permits it`.

### `dialAddress`

```cedar
forbid (principal, action == Action::"dialAddress", resource)
when { context.ip.isInRange(ip("10.0.0.0/8")) || context.ip.isLoopback() };

permit (principal, action == Action::"dialAddress", resource)
when { context.ip.isInRange(ip("10.96.0.0/12")) };
```

Compiles at load into three blocked prefixes (`10.0.0.0/8`,
`127.0.0.0/8` and `::1/128`: `isLoopback()` is both loopback ranges) and
one allowed prefix. At dial time the block list is judged in that order: an
operator `forbid` first, then the `permit` holes, then the default block
list. So the `permit` here never takes effect, because the `forbid` covers
`10.96.0.0/12` and a `forbid` wins; to open an in-cluster service range
inside `10.0.0.0/8`, drop the `forbid` (the default block list already
refuses `10.0.0.0/8`) and keep the `permit`. A `||` is the union of its
tests, a bare address literal is a `/32` or `/128`, and a `dialAddress`
rule with no `when` applies to every address. The guest is told only
`sandbox.egress: <host> resolves to an address the egress policy blocks`;
the resolved address and the prefix that blocked it go to the runtime's
audit line. The default block list and the budgets are on the
[HTTP egress](http-egress.md) page.

### `composeResource`

The pdb-addon Composition, whose README promises that the add-on only adds
disruption budgets:

```cedar
permit (principal, action == Action::"composeResource", resource in Group::"policy")
when { context.change == "add" };
```

A module of that step that modifies the Deployment the platform step
composed is refused with `module oci ghcr.io/example-org/webapp-addons/pdb@sha256:… composed apps/Deployment "deployment" (modify), which the compositionPolicy does not permit (composeResource)`.

An operator who never wants a module to compose RBAC or Secrets, whatever
a Composition says; the second rule is required, since a `forbid` alone
scopes the action and permits nothing:

```cedar
forbid (principal, action == Action::"composeResource", resource)
when { resource in Group::"rbac.authorization.k8s.io" || resource == Kind::"core/Secret" };

permit (principal, action == Action::"composeResource", resource);
```

A per-tenant rule in the shape the other actions take: modules may write
their own XR's status everywhere, and compose workloads in `team-a` only:

```cedar
permit (principal, action == Action::"composeResource", resource)
when { context.target == "composite" ||
       (principal.namespace == "team-a" && resource in Group::"apps") };
```

`function validate` cannot judge output offline; it marks a step whose
`compositionPolicy` scopes the action with `composeResource` on its OK
line, and with `--resolve` warns where a kind the module's manifest
declares it composes would be refused.

## What a refusal says

A refused capability is a fatal result naming the module (`module oci
ghcr.io/example/greeter@sha256:… requires …`) and the layer that refused
it, in these sentences, which `function validate` prints verbatim under the
step:

| requirement | no `--sandbox-policy-file` | the composition layer refuses | the operator layer refuses |
|---|---|---|---|
| `requires.filesystem.privateTmp` | `requires a private /tmp (requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities` | `requires a private /tmp (requires.filesystem.privateTmp), which the compositionPolicy does not permit for this request` | `requires a private /tmp (requires.filesystem.privateTmp), which the operator policy (--sandbox-policy-file) does not permit for this request` |
| `requires.egress.http[i]`, per method | `requires egress (requires.egress.http), but the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)` | `requires egress GET to host "api.example.com" (requires.egress.http[0]), which the compositionPolicy does not permit` | `requires egress GET to host "api.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit` |
| `requires.env` (`setEnv`) | `requires env [API_TOKEN] (requires.env), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities` | `requires env [API_TOKEN] (requires.env), which the compositionPolicy does not permit (setEnv)` | `requires env [API_TOKEN] (requires.env), which the operator policy (--sandbox-policy-file) does not permit (setEnv)` |
| an env binding's credential (`spendCredential`) | as above | `requires env API_TOKEN from credential "apikeys", which the compositionPolicy does not permit (spendCredential)` | `requires env API_TOKEN from credential "apikeys", which the operator policy (--sandbox-policy-file) does not permit (spendCredential)` |
| `requires.credentials[i]` | `requires credential "cmdb" (requires.credentials[0]), but the runtime has no --sandbox-policy-file, which is required to grant step credentials (spendCredential)` | `requires credential "cmdb" (requires.credentials[0]), which the compositionPolicy does not permit` | `requires credential "cmdb" (requires.credentials[0]), which the operator policy (--sandbox-policy-file) does not permit` |
| a change to the desired state (`composeResource`, after the run) | not refused: no policy file narrows the output | `composed apps/Deployment "deployment" (modify), which the compositionPolicy does not permit (composeResource)`; for the composite resource `composed platform.example.org/WebApp (composite, modify), which …` | `composed core/Secret "db-password" (add), which the operator policy (--sandbox-policy-file) does not permit (composeResource)` |

The `module.from` fences refuse before the module is resolved, prefixed
`cannot resolve module:`:

- no `compositionPolicy` at all: `module.from: status.module of the
  composite resource names a OCI source, but the Input has no
  compositionPolicy: a module the composite resource chooses must be
  permitted by the compositionPolicy's pullModule rules, or its author
  could point the runtime at any host`;
- no matching `pullModule` permit: `module.from: status.other of the
  composite resource names ref "index.docker.io/someone/else", which the
  compositionPolicy does not permit (pullModule)` (or `names manifestURL
  "…"` for an HTTP source's manifest);
- a pull credential no `spendCredential` permit covers: `module.from:
  status.module of the composite resource names credentials "registry",
  which the compositionPolicy does not permit (spendCredential) for
  "ghcr.io/example/greeter": a module chosen by the composite resource
  cannot spend a step credential (the registry host would be its author's)
  unless the compositionPolicy permits it for that repository; otherwise
  pull it with the runtime's Docker config or anonymously`.

A `compositionPolicy` that does not parse is `compositionPolicy is
invalid: cannot compile the compositionPolicy as Cedar: <Cedar's error>`.

## Load-time errors

The operator policy is compiled when the runtime starts and when `function
validate` runs. A document that cannot be compiled stops both: the runtime
prints the error and exits 1, `function validate` prints it prefixed
`function validate: ` and exits 2.

- `cannot read operator policy: open <path>: <error>`
- `cannot compile the operator policy <path>: <Cedar's parse error>`
- `operator policy: dialAddress rule "<id>": <reason>`, where `<id>` is the
  rule's id as Cedar numbers the file (`policy0`, `policy1`, ...; rules are
  checked in id order so the first error reported is always the same) and
  the reason one of:
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

Three more checks run at the runtime's startup, not in `function validate`:
when the policy carries any `usePrivateTmp` rule the runtime makes one
temporary directory under `TMPDIR` and exits 1 if it cannot (`the operator
policy grants a private /tmp (usePrivateTmp), but the runtime cannot create
one under <dir>: <error>`), and when `--cosign-key` is set but the policy
has no `requireSignature` rule it logs a warning, since that combination
verifies nothing: `--cosign-key is set but the operator policy has no
requireSignature rule: no module will be signature-verified`; likewise
`--cosign-trusted-root is set but the operator policy has no trustSigner
rule: no keyless signature will be trusted` (or `--cosign-trust-instance
is set …`), and the mirror image, `the operator policy has trustSigner
rules but neither --cosign-trusted-root nor --cosign-trust-instance is
set: every keyless signature will be refused`.

## Testing a policy

`function validate` runs the same code over a Composition offline, with the
flags the runtime is started with:

```shell
function validate composition.yaml \
  --sandbox-policy-file policy.cedar \   # the operator layer; a malformed file is exit 2
  --xr xr.yaml \                         # the principal (namespace, kind) and module.from values
  --resolve \                            # fetch each module and decide its manifest's requires
  --cosign-key cosign.pub                # verify what requireSignature demands
```

The step's `compositionPolicy` is compiled and applied as it would be in
the cluster; the text output marks a step that carries one with
`compositionPolicy` beside `OK`, and lists on a `credentials:` line the step
credentials the module's request would carry. Without `--xr` the principal
is the zero principal, which matches no per-tenant condition, and a
`module.from` source is reported as the composite resource's choice with
its fence checked only for the policy's presence. Exit codes: 0 when every
step is admitted, 1 when at least one is refused, 2 when the tool itself
failed. A module that requires egress on a run without `--cosign-key` gets
the warning `the module requires egress but is not signature-verified:
neither --cosign-key nor --cosign-trusted-root was given`.

## Further reading

The design records behind the model:
[Three-Layer Authorization Model](../one-pager-three-layer-authz.md),
[Policy Engine (Cedar) Evaluation](../one-pager-policy-engine.md),
[Trust Model](../one-pager-trust-model.md) and
[WASM Sandbox](../one-pager-sandbox.md).
