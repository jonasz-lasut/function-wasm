# Trust Model

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.0

Who decides what code runs, what that code can see and reach, and what the
runtime guarantees to each of them. Written after the 2026-08-16 review,
which found two ways the previous model leaked; both are closed here.
Everything below is implemented; the extensions it points to live in the
module-source-schema one-pager, which is a draft.

## Parties

- The **operator** installs the Function, sets its flags (limits, the
  cosign key, the served directory, the runtime's Docker config) and owns
  the pod — its volume, its network policy, its resource limits.
- A **Composition author** references a module in a pipeline step's Input,
  states its digest, may name a step credential for the pull, and passes
  `config`. They are trusted with the request their step receives, exactly
  as the author of a native function's Composition is.
- A **composite resource author** may, when the Composition says so
  (`ociFrom`/`httpFrom`/`pathFrom`), choose which module runs by writing a
  field of their XR. They are trusted with less: they can pick code, not
  widen what it may do or spend what it may not.
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

## Credentials

A step credential (`module.oci.credentials`) is spent only on a module the
Composition itself names. A source read from the XR may not name one: the
XR author would also pick the registry host, and a registry that answers
with a `Basic` challenge receives the secret — the review demonstrated it,
credential captured. XR-chosen modules are pulled with the runtime's own
Docker config (`DOCKER_CONFIG`, mounted by the operator; its entries are
bound to their registry host) or anonymously; an XR object with
`credentials` is a fatal result explaining why.

The credential that pulled the module is the host's: it is removed from the
request before the guest sees it. Every other step credential is forwarded,
as it would be to a native function — the Composition author declared them
for the step. Today the guest cannot phone home with them (no network); the
sandbox one-pager's egress design must keep it that way for anything the
Composition did not grant.

## What the guest sees and can do

The whole `RunFunctionRequest` less the pull credential: observed and
desired state, context, extra resources, `config`. It runs in a fresh
wasmtime store with WASI preview 1 and no pre-opened directories, no
environment, no sockets, one host import (`wasmfn.log`), a wall-clock
deadline and a linear-memory cap; stdout and stderr go to the pod's log.
Everything it produces leaves in the response, which the host forwards
verbatim (adding `meta` when the guest omitted it). A trap, exit, timeout,
memory exhaustion, bad ABI, or a host-side panic on its request is a fatal
result — never a crashed process, so one module cannot take the runtime
down for the Compositions sharing it (see the resource-governance
one-pager for the bounds).

An XR author who can pick a module can, through it, write desired state and
results into their own XR — the same power any module of that Composition
has over that XR. They cannot pick a module the Composition's signature
policy refuses, spend the step's credentials, or read another XR's request.

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
| XR author aims a credential at their host | refused (above) |
| XR author picks unsigned code | `--cosign-key` refuses it |
| module exfiltrates a step credential | no network today; the pull credential is withheld; egress grants (the sandbox one-pager, a draft) must be Composition-only |
| module attacks the host | wasmtime sandbox; guest-controlled offsets are bounds-checked and sliced without overflow; host panics are recovered |
| module starves the runtime | deadline, memory cap, compile semaphore, bounded tiers |
| SSRF through `httpFrom` | the runtime only GETs; a response is used only when it matches the stated digest; restrict the pod's egress with a NetworkPolicy where internal endpoints matter |
| decompression bomb in a tar layer | extraction bounded at eight times the module size limit |

## What comes next

Refusing credentials on XR-chosen modules is safe but blunt: a platform team
that wants tenants to pick modules *from the platform's private registry*
must mount a Docker config into the runtime. A Composition-owned `policy`
(`repositoryAllowList`, `credentialsAllowList`) that fences what an
XR-chosen module may be and spend is designed in the module-source-schema
one-pager, together with the Input shape it needs; nothing of it is
implemented, and this document describes only what is.
