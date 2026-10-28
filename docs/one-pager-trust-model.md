# Trust model

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.9

Who decides what code runs, what that code can see and reach, and what the
runtime guarantees to each of them. Written after the 2026-08-16 review,
which found two ways the previous model leaked. This revision closes both.
Revision 1.1 added the Composition-owned `policy` that fenced XR-chosen
modules - the Input schema is the module-source-schema one-pager's. Revision
1.2 records the sandbox's grants - filesystem and environment, and what
enforces the network boundary now that HTTP egress through the host is
implemented (sandbox one-pager, revision 1.0). Revision 1.3 records the
operator-authored Cedar grant policy (`--sandbox-policy-file`) that narrows the
sandbox grants per caller and the per-repository signature rule that makes
`--cosign-key` per-repository, both on the operator boundary only (policy-engine
one-pager). Revision 1.4 records the three-layer authorization model
(three-layer-authz one-pager), where the `policy` allowlists became the
Input's `compositionPolicy` (Cedar) and the `sandbox` block became the
module manifest's `requires`. Every capability is the AND of manifest ∧
composition layer ∧ operator layer. Revision 1.5 records the signature
format: the runtime verifies cosign 3's key-based Sigstore bundles, found
among the OCI 1.1 referrers of the module's manifest, and no longer reads
cosign 2's legacy `sha256-<hex>.sig` signatures. Revision 1.6 makes
`spendCredential` a confidentiality boundary: a module's request contains
only the step credentials its manifest names (`requires.env` bindings and
`requires.credentials`) and both Cedar layers permit, every other one
edited out at the wire level - the Go runtime forwarded every step
credential but the pull credential. Everything below is implemented.


## Parties

- The **operator** installs the Function, sets its flags (limits, the
  cosign key, the served directory, the runtime's Docker config) and
  controls the pod: its volume, network policy and resource limits.
- A **Composition author** references a module in a pipeline step's Input
  (`module.type` + `oci`/`http`/`path`), states its digest, may specify a
  step credential for the pull, sets the `compositionPolicy` - their own Cedar
  layer, fencing modules the XR may choose and optionally narrowing
  sandbox capabilities - the `limits` of the run, and passes `config`.
  They're trusted with the request their step receives, exactly as the
  author of a native function's Composition is.
- A **composite resource author** may, when the Composition says so
  (`module.from`), choose which module runs by writing a field of their XR.
  They're trusted with less: they can pick code - within the
  `compositionPolicy`'s `pullModule` permits - not widen what it may do or
  spend what it may not: the runtime reads `compositionPolicy` and `limits`
  from the Input only.
- A **module author** publishes signed or unsigned artifacts, declaring in
  the manifest what the module can't run without (`requires`), the step
  credentials it reads among them; their code runs in the sandbox with
  whatever the request contains less the step credentials it wasn't
  granted, and a manifest can only make a run fail earlier, never widen
  it.

## What pins the code

Each remote module is pinned by a digest stated in the Composition or the
XR field: the manifest digest of an OCI reference (`repo[:tag]@sha256:…`; a
tag alone is refused - the manifest states its layer's digest and the
registry client verifies both) or `http.digest`. Served files (`path`) are
unpinned by design: the operator put them on the pod. The caches key on
these digests and the runtime verifies every fetch before it stores or
compiles it,
so nothing that runs can change without the Composition (or the XR field
it delegated to) changing.

With `--cosign-key`, only OCI modules carrying a key-based cosign signature
run, and `http`/`path` sources are refused. Verification is a precondition
of *running*, checked once per digest per process before the runtime looks in
any cache tier. An artifact a runtime without a key (or with a since-rotated key)
left on a shared or persisted volume is never served by a keyed one.

The signature is cosign 3's: a Sigstore bundle (v0.3) holding a DSSE
envelope around an in-toto statement, pushed as an OCI 1.1 referrer whose
subject is the module's manifest. The runtime lists the referrers through
the registry's referrers API, or through the referrers tag schema's
`sha256-<hex>` index where the API answers 404 (GHCR, `registry:3`), over
its own registry client with the pull's credentials. Since the listings
aren't pinned, a referrer counts only if its subject is the pinned digest,
and the runtime fetches the bundle with a size bound and verifies it against
its digest. A module is
signed when an envelope signature over the DSSE pre-authentication encoding
verifies with a configured key and the statement it signs is a cosign
signature (`predicateType` `https://sigstore.dev/cosign/sign/v1`) whose
subject is the manifest digest; the runtime reads the statement only after the
signature verifies. Other attestations, such as SLSA provenance, never
count, whichever key signed them. For a key-based bundle the configured
keys are the trust root: its verification material (key hint and Rekor
entries) isn't consulted, as the legacy path never consulted Rekor.

cosign 2's legacy signature, a simple-signing payload under the
`sha256-<hex>.sig` tag, isn't read: a module signed only that way is
unsigned to this runtime and must be re-signed with cosign 3.

The runtime verifies keyless (Fulcio/Rekor) signatures (#116) through
sigstore-verify, the Sigstore organization's second Rust library, against
the Sigstore trusted root `--cosign-trusted-root` names. The signing time
comes from the bundle's RFC 3161 timestamps and its Rekor entry's signed
entry timestamp (a source that's present but fails is fatal, and there is
no fallback to the clock). The inclusion proof and checkpoint are verified
and the certificate chains to a Fulcio authority at that time. Only then is
the statement read, under the same rules as a key-based bundle's. The
signer the certificate names is a further party in this model. Through
`trustSigner` rules in the same policy file that requires the signature,
the operator decides per repository which OIDC issuer and identity may
vouch for a module. The rules are default-deny, so a trusted root alone
admits nothing. Key-based bundles keep the key path: the trusted root is
never consulted for a bundle with a key hint, and a keyless bundle met
without one is refused by name rather than mistaken for a key failure. The
root is a file read at startup (`--cosign-trusted-root`, the air-gapped
mode) or, since revision 1.8 (#183), fetched over TUF from a well-known
instance (`--cosign-trust-instance public-good|staging|github`) and
refreshed while serving: the TUF root embedded in the sigstore-trust-root
crate is the trust anchor, the walk (root, timestamp, snapshot, targets,
each signed and expiring) pins `trusted_root.json` by length and hashes, the
runtime caches the verified metadata under the module cache root, and a pod that
can't fetch starts from that cache or, with none, serves while refusing
keyless signatures by name until a retry succeeds - fail-closed per
module, never a crash loop that takes key-signed and unsigned modules down
with it, and never the crate's embedded snapshot, a freeze the operator
didn't choose. A changed root forgets the verified-digest memo, so a signer
a rotation dropped is re-verified on the module's next request, while a
failed refresh keeps the last good root. Verification never touches the
network. Opt-in only: the runtime doesn't open any connection the operator
didn't ask for.

An operator `--sandbox-policy-file` makes the signature rule per-repository.
A `requireSignature` rule demands a signature for the repositories it
names, `--cosign-key` supplies the keys, and a required module the runtime
can't verify is refused (fail-closed). This replaces the all-or-nothing
default: a repository outside every rule then runs unsigned. The runtime
warns at startup when a key is set but the policy doesn't require a
signature anywhere (policy-engine one-pager). The choice of *which*
repositories must be signed is operator policy. The crypto is unchanged.

## Credentials and the `compositionPolicy` fence

A step credential (`module.oci.credentials`) is spent on a module the
Composition itself names, or on an XR-chosen module the Composition's
`compositionPolicy` explicitly permits. Without such a permit a source read
from the XR may not name one. The XR author would also pick the registry
host, and a registry responding with a `Basic` challenge receives the
secret (the review demonstrated it: credential captured). XR-chosen modules
are then pulled with the runtime's own Docker config (`DOCKER_CONFIG`,
mounted by the operator; its entries are bound to their registry host) or
anonymously; an XR object with `credentials` is a fatal result explaining
why and pointing at the policy.

The Composition-owned `compositionPolicy` - a top-level Input field, never
read through `module.from` - fences what an XR-chosen module may be and
spend:

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
  over the boundary-correct `Repository` hierarchy, so a permitted prefix
  never admits a sibling namespace or adjacent host (a trailing slash is
  optional), and a ref or URL with dot segments is refused before matching;
  anything else is a fatal result that states the policy and the ref. A
  `compositionPolicy` is required whenever `module.from` names an OCI or
  HTTP source: an unfenced XR author could point the runtime at any host
  and read what its answer says.
- `spendCredential`: the step credentials an XR-chosen object may name,
  spent only on a ref `pullModule` admits - the pull check runs first, so
  a permitted credential is never sent to a host the Composition didn't
  admit, and `context.repository` lets a permit co-locate credential and
  repository. With no matching permit, none - the preceding rule. A
  credential without a permit is a fatal result.

A static source isn't subject to the fence: the Composition named it and
is trusted with it. `internal/module/policy.go` is the one place the rule
lives.

The credential that pulled the module is the host's: it's removed from the
request before the guest sees it, and a manifest that binds it to an env
variable or requires it whole is refused. Of the step's other credentials
the guest receives only those granted to it: each one its manifest names
(a `requires.env` binding's credential or one it reads whole through
`requires.credentials`) and both Cedar layers permit (`spendCredential`:
the composition layer's scoped default-permit with no `context.repository`
and the operator's default-deny). The runtime edits every other one out of
the forwarded request at the wire level (`protowire.rs` changes only those
map entries, so fields newer than the vendored proto still arrive at the
guest). A module with no manifest, or one that doesn't require a
credential, sees none. A credential the manifest requires that the step
doesn't declare is a fatal result before the run, as an env binding's is.
This is a deliberate divergence from the Go runtime and from a native
function, which receive every step credential the Composition declared.
Declaring a credential for the step no longer hands it to any module that
runs there, so `spendCredential` keeps a secret from a module and not only
out of its environment. A credential the guest was granted can leave only
where egress is granted. Without a grant it can't phone home (no network).
With one, it can reach only the hosts its manifest listed within both
Cedar layers: the grant is the policy layers' alone, so nothing an XR
author writes widens where a credential may go.

## The guest's request and sandbox

The whole `RunFunctionRequest` less every step credential it wasn't
granted (the pull credential always): observed and desired state, context,
extra resources, `config`, and the credentials its manifest names that both
Cedar layers permit. It runs in a fresh
Wasmtime store with WASI preview 1 and no sockets, two host imports
(`wasmfn.log`, and `wasmfn.http`, which performs a request only within an
egress grant its manifest requires and both Cedar layers permit
(`grantEgress`) and refuses otherwise), a wall-clock deadline and a linear-memory cap; stdout
and stderr go to the pod's log. It has no pre-opened directories and no
environment unless its manifest's `requires` asks and the policy layers
permit. Then it gets a private `/tmp` created for the request and removed
after it, and exactly the environment variables its bindings resolve from
step credentials, never the runtime's own environment. The private `/tmp`
is the only directory a module ever gets: host directories aren't mountable
by design, so no operator path is readable by a module. The runtime reads the
policy layers from the Input and the operator's file only, so an XR author can't
widen them.

Everything it produces leaves in the response, which the host forwards
verbatim (adding `meta` when the guest omitted it). A trap, exit, timeout,
memory exhaustion, bad ABI, or a host-side panic on its request is a fatal
result - never a crashed process, so one module can't take the runtime
down for the Compositions sharing it (see the resource-governance
one-pager for the bounds).

Through the module they pick, an XR author can write desired state and
results into their own XR - the same power any module of that Composition
has over that XR - unless the Composition's `compositionPolicy` or the
operator's policy fences `composeResource` (revision 1.9, #174). Then every
change the module makes to the desired state (a resource added, modified
or removed, the XR's own status included) must be permitted by kind, and
a refused change is a fatal result that doesn't apply anything the module
composed. They can't pick a module outside the
`compositionPolicy`'s `pullModule` permits or one the runtime's signature
policy refuses, spend a step credential no `spendCredential` permit names
for that repository, widen the run's `limits` or the sandbox, or read
another XR's request.

## Supply chain of the runtime itself

The image builds from `golang:<go.mod version>` on a digest-pinned
Chainguard `glibc-dynamic` base (chosen over distroless/cc-debian13 for
scanning clean, with Renovate bumping the digest). It runs as `nonroot`, and
`supplychain.yml` signs and attests it. `grype-scan.yml` scans the
latest release weekly, and wasmtime-go bumps arrive as their own Renovate
PRs so a review sees the sandbox's own security fixes alone.

## Threats considered

| threat | answer |
|---|---|
| tag moved to malicious content | tags alone are refused; a tag next to a digest is decoration |
| registry or CDN serves other bytes | manifest verified against the reference, layer against the manifest, module against the layer digest - twice (registry client, host) |
| tampered cache volume | blobs verified against their name on every read; artifacts validated by Wasmtime; signature checked before any tier |
| XR author aims a credential at their host | refused unless a `compositionPolicy` `spendCredential` permit names the credential and a `pullModule` permit admits the host (the preceding section) |
| XR author widens the policy, limits or sandbox through the XR | impossible: `compositionPolicy` and `limits` are top-level Input fields, the runtime reads only `module.from` from the XR, and the sandbox is the manifest's ask under both Cedar layers |
| XR author picks unsigned code | `--cosign-key` refuses it |
| module reads or exfiltrates a step credential | a module sees only the step credentials its manifest names and both Cedar layers permit (`spendCredential`); the runtime edits every other one, the pull credential always, out of its request before the module runs. No network by default. Where both Cedar layers permit egress (`grantEgress`) a module reaches only the hosts, methods and paths its own manifest's `requires.egress.http` rules declare, over a default block list covering loopback, link-local, private and cluster ranges (`dialAddress` adds more) - every resolved address is judged, the checked address is dialled, and every request leaves an audit line with the module digest; `--cosign-key` is strongly recommended wherever egress is granted |
| module attacks the host | Wasmtime sandbox; guest-controlled offsets are bounds-checked and sliced without overflow; host panics are recovered |
| module reads or writes host files | never: host directories aren't mountable into a module (no flag offers it). A module is granted at most its private `/tmp`: a fresh directory per request that gets removed afterward and is bounded by the filesystem behind `$TMPDIR`, with `..` escapes refused by Wasmtime inside the pre-open (`EPERM`) |
| module reads the runtime's environment | never: WASI environ is empty, or exactly the manifest's `requires.env` bindings resolved from step credentials the request already included |
| module starves the runtime | deadline, memory cap, compile semaphore, bounded tiers |
| SSRF through `module.from` (`type: HTTP` or `OCI`) | a `compositionPolicy` `pullModule` permit is required for an XR-chosen network source. The runtime fetches only from repositories the Composition permitted; permits match a normalized location over the boundary-correct hierarchy and dot segments are refused, so a prefix can't be escaped; the runtime sends only GET requests; restrict the pod's egress with a NetworkPolicy where internal endpoints matter |
| decompression bomb in a tar layer | extraction bounded at eight times the module size limit |

## What comes next

The sandbox one-pager designs grants - a private `/tmp`, HTTP egress through
the host, environment variables - that widen what a module can reach (host
mounts were built and then removed: a module's inputs come through the
request). They're implemented under the rules here. The ask is the module
manifest's and the grant the policy layers' (never read from the XR). A
module sees only the step credentials granted to it, and egress keeps
those from leaving through any host the manifest didn't list and a layer
refuses (the rules name hosts, methods and paths, and the operator's
policy caps the hosts and blocks internal ranges).
`--cosign-key` is strongly recommended wherever egress is granted.

