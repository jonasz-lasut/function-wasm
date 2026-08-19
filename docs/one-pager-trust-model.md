# Trust Model

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.3

Who decides what code runs, what that code can see and reach, and what the
runtime guarantees to each of them. Written after the 2026-08-16 review,
which found two ways the previous model leaked; both are closed here.
Revision 1.1 adds the Composition-owned `policy` that fences XR-chosen
modules — the Input shape is the module-source-schema one-pager's; revision
1.2 records the sandbox's grants — filesystem and environment, and what
enforces the network boundary now that HTTP egress through the host is
implemented (sandbox one-pager, revision 1.0); revision 1.3 records the
operator-authored Cedar grant policy (`--sandbox-policy-file`) that narrows the
sandbox grants per caller and the per-repository signature requirement that makes
`--cosign-key` per-repository, both on the operator boundary only (policy-engine
one-pager). Everything below is implemented.


## Parties

- The **operator** installs the Function, sets its flags (limits, the
  cosign key, the served directory, the runtime's Docker config) and owns
  the pod — its volume, its network policy, its resource limits.
- A **Composition author** references a module in a pipeline step's Input
  (`module.type` + `oci`/`http`/`path`), states its digest, may name a step
  credential for the pull, sets the `policy` for modules the XR may choose,
  the `limits` of the run, and passes `config`. They are trusted with the
  request their step receives, exactly as the author of a native function's
  Composition is.
- A **composite resource author** may, when the Composition says so
  (`module.from`), choose which module runs by writing a field of their XR.
  They are trusted with less: they can pick code — within the Composition's
  `policy` — not widen what it may do or spend what it may not: `policy`,
  `limits` and `sandbox` are read from the Input only.
- A **module author** publishes signed or unsigned artifacts; their code
  runs in the sandbox with whatever the request carries.

## What pins the code

Every remote module is pinned by a digest stated in the Composition or the
XR field: the manifest digest of an OCI reference (`repo[:tag]@sha256:…`; a
tag alone is refused — the manifest names its layer's digest and the
registry client verifies both) or `http.digest`. Served files (`path`) are
unpinned by design: the operator put them on the pod. The caches key on
these digests and every fetch is verified before it is stored or compiled,
so nothing that runs can change without the Composition (or the XR field
it delegated to) changing.

With `--cosign-key`, only OCI modules carrying a key-based cosign signature
run, and `http`/`path` sources are refused. Verification is a precondition
of *running*, checked once per digest per process before any cache tier is
consulted: an artifact a keyless runtime (or one with a since-rotated key)
left on a shared or persisted volume is never served by a keyed one. Keyless
(Fulcio/Rekor) signatures are out of scope — sigstore-go alone is hundreds
of modules and needs network access to Rekor and TUF at run time.

An operator `--sandbox-policy-file` makes the requirement per-repository: a
`requireSignature` rule demands a signature for the repositories it names,
`--cosign-key` supplies the keys, and a required module the runtime cannot
verify is refused (fail-closed). This replaces the all-or-nothing default -
a repository no rule names then runs unsigned, so the runtime warns loudly at
startup when a key is set but the policy requires nothing (policy-engine
one-pager). The choice of *which* repositories must be signed is operator
policy; the crypto is unchanged.

## Credentials and `policy`

A step credential (`module.oci.credentials`) is spent on a module the
Composition itself names, or on an XR-chosen module the Composition's
`policy` explicitly allows. Without such a policy a source read from the XR
may not name one: the XR author would also pick the registry host, and a
registry that answers with a `Basic` challenge receives the secret — the
review demonstrated it, credential captured. XR-chosen modules are then
pulled with the runtime's own Docker config (`DOCKER_CONFIG`, mounted by
the operator; its entries are bound to their registry host) or anonymously;
an XR object with `credentials` is a fatal result explaining why and
pointing at the policy.

The Composition-owned `policy` — a top-level Input field, never read
through `module.from` — fences what an XR-chosen module may be and spend:

```yaml
module:
  type: OCI
  from: status.module                       # the XR names {ref, credentials}
policy:
  repositoryAllowList: ["ghcr.io/example-org/"]   # prefixes the XR's ref must match
  credentialsAllowList: ["registry"]              # step credentials the XR object may name
```

- `policy.repositoryAllowList`: an XR-chosen `ref` (or `url` for `HTTP`)
  must lie within one of the entries — prefixes over the normalized location
  (`registry/repository`, `scheme://host/path`) matched at a path or host
  boundary, so a prefix never admits a sibling namespace or adjacent host (a
  trailing slash is optional), and a ref or URL with dot segments is refused
  before matching; anything else is a fatal result naming the policy and the
  ref. Required whenever `module.from` names an OCI or HTTP source: an
  unfenced XR author could point the runtime at any host and read what its
  answer says.
- `policy.credentialsAllowList`: the step credentials an XR-chosen object may
  name, spent only on a ref the repository list admits — the repository check
  runs first, so a listed credential never reaches a host the Composition did
  not admit, and a credentials list without a repository list is refused by
  the CRD and the runtime (`policy.credentialsAllowList requires
  policy.repositoryAllowList`). Absent or empty, none — the rule above. A
  credential outside the list is a fatal result.

A static source is not subject to the policy: the Composition named it and
is trusted with it. `internal/module/policy.go` is the one place the rule
lives.

The credential that pulled the module is the host's: it is removed from the
request before the guest sees it. Every other step credential is forwarded,
as it would be to a native function — the Composition author declared them
for the step. Without an egress grant the guest cannot phone home with
them (no network); with one, it can reach only the hosts the Composition
listed, within the operator's policy — the grant is the Composition's
alone, so nothing an XR author writes widens where a credential may go.

## What the guest sees and can do

The whole `RunFunctionRequest` less the pull credential: observed and
desired state, context, extra resources, `config`. It runs in a fresh
wasmtime store with WASI preview 1 and no sockets, two host imports
(`wasmfn.log`, and `wasmfn.http`, which performs a request only within a
`sandbox.egress` grant the operator's Cedar `--sandbox-policy-file` enables
(`grantEgress`) and refuses otherwise), a wall-clock deadline and a linear-memory cap; stdout
and stderr go to the pod's log. It has no pre-opened directories and no
environment unless the Composition's `sandbox` grants them within the
operator's flags: a private `/tmp` created for the request and removed after
it (the only directory a module is ever given — host directories are
deliberately not mountable, so no operator path is readable by a module),
exactly the listed environment variables — never the runtime's own
environment. Grants are read from the Input only, so an XR author cannot
widen them.

Everything it produces leaves in the response, which the host forwards
verbatim (adding `meta` when the guest omitted it). A trap, exit, timeout,
memory exhaustion, bad ABI, or a host-side panic on its request is a fatal
result — never a crashed process, so one module cannot take the runtime
down for the Compositions sharing it (see the resource-governance
one-pager for the bounds).

An XR author who can pick a module can, through it, write desired state and
results into their own XR — the same power any module of that Composition
has over that XR. They cannot pick a module outside the Composition's
`policy.repositoryAllowList` or one the runtime's signature policy refuses,
spend a step credential the Composition did not list for that repository,
widen the run's `limits` or the sandbox, or read another XR's request.

## Supply chain of the runtime itself

The image is built from `golang:<go.mod version>` on a digest-pinned
Chainguard `glibc-dynamic` base (chosen over distroless/cc-debian13 for
scanning clean; Renovate bumps the digest), runs as `nonroot`, and is signed
and attested by `supplychain.yml`; `grype-scan.yml` scans the latest release
weekly; wasmtime-go bumps arrive as their own Renovate PRs so the sandbox's
own security fixes are judged alone.

## Threats considered

| threat | answer |
|---|---|
| tag moved to malicious content | tags alone are refused; a tag next to a digest is decoration |
| registry or CDN serves other bytes | manifest verified against the reference, layer against the manifest, module against the layer digest — twice (registry client, host) |
| tampered cache volume | blobs verified against their name on every read; artifacts validated by wasmtime; signature checked before any tier |
| XR author aims a credential at their host | refused unless `policy.credentialsAllowList` names the credential and `policy.repositoryAllowList` admits the host (above) |
| XR author widens policy, limits or sandbox through the XR | impossible: they are top-level Input fields, only `module.from` is read from the XR |
| XR author picks unsigned code | `--cosign-key` refuses it |
| module exfiltrates a step credential | no network by default; the pull credential is withheld. Where the operator's Cedar `--sandbox-policy-file` grants egress (`grantEgress`) a module reaches only the hosts, methods and paths its Composition's `sandbox.egress.http` rules grant (read from the Input only, never the XR), which the operator's Cedar `--sandbox-policy-file` (`grantEgress`) may narrow, over a default block list covering loopback, link-local, private and cluster ranges (`dialAddress` adds more) - every resolved address is judged, the checked address is dialled, and every request leaves an audit line with the module digest; `--cosign-key` is strongly recommended wherever egress is granted |
| module attacks the host | wasmtime sandbox; guest-controlled offsets are bounds-checked and sliced without overflow; host panics are recovered |
| module reads or writes host files | never: host directories are not mountable into a module (no flag offers it); the only directory a module can be granted is its private `/tmp` — a fresh directory per request, removed afterwards, bounded by the filesystem behind `$TMPDIR`, with `..` escapes refused by wasmtime inside the pre-open (`EPERM`) |
| module reads the runtime's environment | never: WASI environ is empty, or exactly the Composition's `sandbox.env` (non-secret by convention — the values are visible in the Composition) |
| module starves the runtime | deadline, memory cap, compile semaphore, bounded tiers |
| SSRF through `module.from` (`type: HTTP` or `OCI`) | `policy.repositoryAllowList` is required for an XR-chosen network source, so the runtime only fetches from repositories the Composition named; prefixes match a normalized location and dot segments are refused, so a prefix cannot be escaped; the runtime only GETs; restrict the pod's egress with a NetworkPolicy where internal endpoints matter |
| decompression bomb in a tar layer | extraction bounded at eight times the module size limit |

## What comes next

The sandbox one-pager designs grants — a private `/tmp`, HTTP egress through
the host, environment variables — that widen what a module can reach (host
mounts were built and then removed: a module's inputs come through the
request). All of them are implemented (revision 1.1 of that one-pager) under
the rules here: grants are the Composition's (never read
from the XR), egress keeps step credentials from leaving through any host
the Composition did not list (the grant names hosts, methods and paths; the
operator's policy caps the hosts and blocks internal ranges), and
`--cosign-key` is strongly recommended wherever egress is granted.

