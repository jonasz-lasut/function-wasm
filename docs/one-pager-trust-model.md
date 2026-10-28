# Trust model

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.10 (rewritten in the present tense over the Rust workspace; keyless signatures (#116) and the trusted root over TUF (#183) folded into the body as the design)

Who decides what code runs, what that code can see and reach, and what the
runtime guarantees to each of them. The 2026-08-16 review found two ways an
earlier model leaked: a step credential could be aimed at a host an XR author
chose, and a module could read every credential its step declared. The model
below closes both. The operator-facing summary is the manual's
[trust model](operators/trust-model.md). This record keeps the reasoning.

## Parties

- The **operator** installs the Function, sets its flags (limits, the
  signature keys and trusted root, the served directory, the runtime's Docker
  config) and controls the pod: its volume, network policy and resource
  limits.
- A **Composition author** references a module in a pipeline step's Input
  (`module.type` plus `oci`, `http` or `path`), states its digest, may
  specify a step credential for the pull, sets the `compositionPolicy` and the
  `limits` of the run, and passes `config`. The `compositionPolicy` is their
  own Cedar layer: it fences the modules an XR may choose and may narrow
  sandbox capabilities. They're trusted with the request their step receives,
  exactly as the author of a native function's Composition is.
- A **composite resource author** may, when the Composition says so
  (`module.from`), choose which module runs by writing a field of their XR.
  They're trusted with less: they can pick code, within the
  `compositionPolicy`'s `pullModule` permits, but can't widen what it may do
  or spend what it may not. The runtime reads `compositionPolicy` and
  `limits` from the Input only.
- A **module author** publishes signed or unsigned artifacts and declares in
  the manifest what the module can't run without (`requires`), the step
  credentials it reads among them. Their code runs in the sandbox with the
  request less the step credentials it wasn't granted. A manifest can only
  make a run fail earlier, never widen it.
- A **signer**, for a keyless signature, is the OIDC identity the certificate
  names. The operator decides per repository which issuer and identity may
  vouch for a module (`trustSigner` rules).

The Input's schema is in the
[module source schema](one-pager-module-source-schema.md). Every capability a
module gets is the AND of three layers, the manifest, the composition layer
and the operator layer, described in the
[three-layer authorization](one-pager-three-layer-authz.md) record; the
sandbox's grants are in the [sandbox](one-pager-sandbox.md) record.

## What pins the code

Each remote module is pinned by a digest stated in the Composition or the XR
field: the manifest digest of an OCI reference (`repo[:tag]@sha256:…`; a tag
alone is refused) or `http.digest`. The manifest states its layer's digest,
and the registry client verifies both. Served files (`path`) are unpinned by
design: the operator put them on the pod. The caches key on these digests and
the runtime verifies every fetch before it stores or compiles it, so nothing
that runs can change without the Composition (or the XR field it delegated
to) changing.

With `--cosign-key`, only OCI modules carrying a valid signature run, and
`http` and `path` sources are refused. Verification is a precondition of
running, checked once per digest per process before the runtime looks in any
cache tier, so a runtime with a key never serves an artifact that a runtime
without one (or with a since-rotated key) left on a shared or persisted
volume.

### The signature format

The signature is cosign 3's: a Sigstore bundle (v0.3) holding a DSSE envelope
around an in-toto statement, pushed as an OCI 1.1 referrer whose subject is
the module's manifest. The runtime lists the referrers through the
registry's referrers API, or through the `sha256-<hex>` index of the
referrers tag schema where the API answers 404 (GHCR, `registry:3`), over its
own registry client with the pull's credentials. Because the listings aren't
pinned, a referrer counts only if its subject is the pinned digest, and the
runtime fetches the bundle with a size bound and verifies it against its
digest.

A module is signed when an envelope signature over the DSSE
pre-authentication encoding verifies and the statement it signs is a cosign
signature (`predicateType` `https://sigstore.dev/cosign/sign/v1`) whose
subject is the manifest digest. The runtime reads the statement only after
the signature verifies. Other attestations, such as SLSA provenance, never
count, whichever key signed them. cosign 2's legacy `sha256-<hex>.sig`
signature isn't read: a module signed only that way is unsigned to this
runtime until it's re-signed with cosign 3.

### Key-based and keyless bundles

For a key-based bundle the keys `--cosign-key` names are the trust root. The
bundle's verification material (key hint and Rekor entries) isn't consulted.

A bundle that contains a certificate is keyless (#116). `sigstore-verify`
takes the signing time from the bundle's RFC 3161 timestamps and its Rekor
entry's signed entry timestamp (a source that's present but fails is fatal,
with no fallback to the clock). It verifies the inclusion proof and
checkpoint, then chains the certificate to a Fulcio authority at that time.
Only then is the
statement read, under the same rules as a key-based bundle's. The signer is
then held to the operator's `trustSigner` rules, in the policy file that
requires the signature: `context.issuer` and `context.identity` over the
repository hierarchy, default-deny, so a trusted root alone admits nothing.
A keyless bundle met by a runtime with no trusted root is refused by name
(both flags) and never fails the check for a key bundle beside it.

### The trusted root

The root is a file read at startup (`--cosign-trusted-root`, the air-gapped
mode) or fetched over TUF from a well-known instance
(`--cosign-trust-instance public-good|staging|github`, #183) and refreshed
while serving every `--cosign-tuf-refresh`. The TUF root embedded in the
`sigstore-trust-root` crate is the trust anchor. The walk (root, timestamp,
snapshot, targets, each signed and expiring) pins `trusted_root.json` by
length and hashes, and the runtime caches the verified metadata under the
module cache root. `--cosign-trusted-root` and `--cosign-trust-instance`
exclude each other.

A pod that can't fetch starts from that cache, re-verified offline from the
embedded TUF root. With no cache it serves, refuses keyless signatures
naming the instance and the last failure, and retries once a minute. That's
fail-closed per module, and it never becomes a crash loop that takes
key-signed and unsigned modules down with it. A changed root forgets the verified-digest
memo, so a signer a rotation dropped is re-verified on the module's next
request, while a failed refresh keeps the last good root. Verification never
touches the network. Both root flags are opt-in: the runtime doesn't open a
connection the operator didn't ask for.

### Which repositories must be signed

An operator `--sandbox-policy-file` makes the rule per repository. A
`requireSignature` rule demands a signature for the repositories it names,
`--cosign-key` and the trusted root supply what verifies it, and a required
module the runtime can't verify is refused (fail-closed). A repository
outside every rule runs unsigned. The runtime warns at startup when a key is
set but the policy requires a signature nowhere, when a trusted root is set
without a `trustSigner` rule, and when `trustSigner` rules have no root. The
choice of which repositories must be signed is operator policy, described in
the [Cedar policy](operators/cedar-policy.md) reference.

## Credentials and the `compositionPolicy` fence

A step credential (`module.oci.credentials`) is spent on a module the
Composition itself names, or on an XR-chosen module the Composition's
`compositionPolicy` explicitly permits. Otherwise a source read from the XR
may not specify one: the XR author would also pick the registry host, and a
registry that sends a `Basic` challenge receives the secret (the review
demonstrated it). XR-chosen modules are then pulled with the
runtime's own Docker config (`DOCKER_CONFIG`, mounted by the operator, its
entries bound to their registry host) or anonymously, and an XR object with
`credentials` is a fatal result that explains why and points at the policy.

The `compositionPolicy` is a top-level Input field, never read through
`module.from`. It fences what an XR-chosen module may be and spend:

```yaml
module:
  type: OCI
  from: status.module                       # the XR names {ref, credentials}
compositionPolicy: |
  permit (principal, action == Action::"pullModule",
          resource in Repository::"ghcr.io/example-org");
  permit (principal, action == Action::"spendCredential", resource == Credential::"registry")
  when { context.repository in Repository::"ghcr.io/example-org" };
```

- `pullModule`: an XR-chosen `ref` (or `url` for `HTTP`) must be permitted
  for its normalized location (`registry/repository`, `scheme://host/path`)
  over the boundary-correct `Repository` hierarchy. A permitted prefix never
  admits a sibling namespace or adjacent host (a trailing slash is
  optional), and a ref or URL with dot segments is refused before matching.
  Anything else is a fatal result that states the policy and the ref. A
  `compositionPolicy` is required whenever `module.from` names an OCI or
  HTTP source: an unfenced XR author could point the runtime at any host and
  read what its answer says.
- `spendCredential`: the step credentials an XR-chosen object may name,
  spent only on a ref `pullModule` admits. The pull check runs first, so a
  permitted credential never goes to a host the Composition didn't admit,
  and `context.repository` lets a permit co-locate credential and
  repository. With no matching permit there is none, and the credential is a
  fatal result.

A static source isn't subject to the fence: the Composition named it and is
trusted with it. `crates/function/src/from.rs` applies the rule, judged by
`crates/function/src/authz.rs`.

### Credentials the module receives

The credential that pulled the module is the host's. The runtime removes it
from the request before the guest sees it, and refuses a manifest that binds
it to an env variable or requires it whole. Of the step's other credentials
the guest receives only those granted to it: each one its manifest names (a
`requires.env` binding's credential, or one it reads whole through
`requires.credentials`) and both Cedar layers permit (`spendCredential`: the
composition layer's scoped default-permit and the operator's default-deny).
The runtime edits every other one out of the forwarded request at the wire
level. `crates/function/src/protowire.rs` drops only those map entries, so
fields newer than the vendored proto still arrive at the guest. A module with
no manifest, or one that doesn't require a credential, sees none. A
credential the manifest requires that the step doesn't declare is a fatal
result before the run, as an env binding's is.

Declaring a credential for the step doesn't hand it to every module that
runs there: `spendCredential` keeps a secret from a module, not
only out of its environment (issue #122, 2026-10). A granted credential can
leave only where egress is granted. Without a grant the module has no
network. With one it reaches only the hosts its manifest listed within both
Cedar layers, a grant that's the policy layers' alone, so nothing an XR
author writes widens where a credential may go.

## The guest's request and sandbox

The guest receives the whole `RunFunctionRequest` less every step credential
it wasn't granted (the pull credential always): observed and desired state,
context, extra resources, `config`, and the credentials its manifest names
that both Cedar layers permit. It runs in a fresh Wasmtime store with WASI
0.3 and 0.2 and no sockets. It has two ways out: the typed `log` import, and
`wasi:http`, which the host performs only within an egress grant its manifest
requires and both Cedar layers permit (`grantEgress`) and refuses otherwise.
A wall-clock deadline and a memory cap over linear memories and tables bound the run, and stdout and
stderr go to the pod's log.

The guest has no pre-opened directories and no environment unless its
manifest's `requires` asks and the policy layers permit. Then it gets a
private `/tmp` created for the request and removed after it, and exactly the
environment variables its bindings resolve from step credentials, never the
runtime's own. The private `/tmp` is the only directory a module ever gets:
host directories aren't mountable by design (a module's inputs come through
the request), so no operator path is readable by a module. The runtime reads
the policy layers from the Input and the operator's file only, so an XR
author can't widen them.

Everything the module produces leaves in the response, which the host
forwards verbatim, adding `meta` when the guest omitted it. A trap, exit,
timeout, memory exhaustion, bad ABI or host-side panic on its request is a
fatal result, never a crashed process, so one module can't take the runtime
down for the Compositions sharing it (the bounds are in the
[resource governance](one-pager-resource-governance.md) record).

Through the module they pick, an XR author can write desired state and
results into their own XR. Any module of that Composition has the same power
over that XR, unless the `compositionPolicy` or the operator's policy fences
`composeResource` (#174). Then each change the module makes to the desired
state (a resource added, modified or removed, the XR's own status included)
must be permitted by kind, and a refused change is a fatal result: none of
what the module composed is applied. They can't pick a module outside the
`compositionPolicy`'s `pullModule` permits or one the runtime's signature
policy refuses, spend a step credential no `spendCredential` permit names for
that repository, widen the run's `limits` or the sandbox, or read another
XR's request.

## Supply chain of the runtime itself

The release image builds in a Rust stage and runs on a digest-pinned
Chainguard `glibc-dynamic` base, chosen over distroless for scanning clean,
with Renovate bumping the digest. It runs as `nonroot`, and `supplychain.yml`
signs and attests it. `grype-scan.yml` scans the latest release weekly, and
Wasmtime bumps arrive as their own Renovate PRs so a review sees the
sandbox's own security fixes alone.

## Threats considered

| threat | answer |
|---|---|
| tag moved to malicious content | tags alone are refused; a tag next to a digest is decoration |
| registry or CDN serves other bytes | manifest verified against the reference, layer against the manifest, module against the layer digest, twice (registry client, host) |
| tampered cache volume | blobs verified against their name on every read; artifacts validated by Wasmtime; signature checked before any tier |
| XR author aims a credential at their host | refused unless a `compositionPolicy` `spendCredential` permit names the credential and a `pullModule` permit admits the host |
| XR author widens the policy, limits or sandbox through the XR | impossible: `compositionPolicy` and `limits` are top-level Input fields, the runtime reads only `module.from` from the XR, and the sandbox is the manifest's ask under both Cedar layers |
| XR author picks unsigned code | `--cosign-key` or a `requireSignature` rule refuses it |
| module reads or exfiltrates a step credential | a module sees only the step credentials its manifest names and both Cedar layers permit (`spendCredential`); the runtime edits every other one, the pull credential always, out of its request before the module runs. No network by default. Where both Cedar layers permit egress (`grantEgress`) a module reaches only the hosts, methods and paths its own manifest's `requires.egress.http` rules declare, over a default block list covering loopback, link-local, private and cluster ranges (`dialAddress` adds more). The runtime judges each resolved address and dials the checked one, and each request leaves an audit line with the module digest. A signature rule is strongly recommended wherever egress is granted |
| module attacks the host | Wasmtime sandbox; host panics are caught into a fatal result |
| module reads or writes host files | never: host directories aren't mountable into a module. A module gets at most its private `/tmp`, a fresh directory per request that's removed afterward. The filesystem behind `$TMPDIR` bounds it, and Wasmtime refuses `..` escapes inside the pre-open (`EPERM`) |
| module reads the runtime's environment | never: the WASI environment is empty, or exactly the manifest's `requires.env` bindings resolved from step credentials the request already included |
| module starves the runtime | deadline, memory cap, compile slots, bounded tiers |
| SSRF through `module.from` (`type: HTTP` or `OCI`) | a `compositionPolicy` `pullModule` permit is required for an XR-chosen network source. The runtime fetches only from repositories the Composition permitted; permits match a normalized location over the boundary-correct hierarchy and dot segments are refused, so a prefix can't be escaped; the runtime sends only GET requests; restrict the pod's egress with a NetworkPolicy where internal endpoints matter |
| decompression bomb in a tar layer | extraction bounded at eight times the module size limit |

## Alternatives

**Forward every step credential.** The Go runtime, like a native function,
forwarded every step credential but the pull credential. That made
`spendCredential` gate a convenience: a module denied an env binding still
read the key from its request. The runtime forwards only what the module was
granted.

**Verify key-based bundles through the keyless verifier.** One verification
path would be simpler, but `sigstore-verify` refuses a public-key bundle
that has neither a transparency log entry nor a timestamp. That's what
`cosign sign --key --tlog-upload=false` writes. Key bundles stay on a path
where the configured keys are the whole trust root.
