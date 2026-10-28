# WASM sandbox

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.8

How the sandbox grants a module *some* filesystem, network or environment
access without giving up what makes it safe to run other people's modules.
Revision 0.3 added the Input's `sandbox` types (`internal/sandbox` validated
them for shape). Revision 0.4 implemented the filesystem (phase 1) and
environment (phase 3) grants, and phase 2 - HTTP
egress through the host (the `wasmfn.http` import,
`wasmfn.HTTPClient()`, `internal/egress`). Revision 1.0 is the merged result, every phase but the
component-model one implemented. Revision 1.1 removes host mounts (Jonasz,
2026-08-16). A Composition can no longer map an operator-declared host
directory into a module (the request is a module's only view of the world
beyond what it writes into its private `/tmp`), so the filesystem grant is
`privateTmp` alone. `--enable-sandbox-mounts`/`--sandbox-mount` are
gone. Revision 1.5 adds the optional operator Cedar grant policy
(`--sandbox-policy-file`): when set it narrows every sandbox capability grant
(the private `/tmp` as well as environment and egress) per caller and per
condition, evaluated default-deny. Revision 1.7 drops the `--enable-sandbox-*` flags
(Jonasz, 2026-08-19): the Cedar policy is now the one authority that enables a
sandbox capability (`usePrivateTmp`, `setEnv`, `grantEgress`), so with no policy
file the runtime refuses every sandbox grant and offers only the default
sandbox - which is all most modules need (policy-engine one-pager).
Revision 1.8 moves the ask out of the Input entirely (the three-layer-authz
one-pager): the `sandbox` block is gone from `input/v1beta1`; a module
declares what it can't run without in its manifest's `requires` (egress
rules, `filesystem.privateTmp`, env credential bindings), the Input's
`compositionPolicy` may narrow it, and the operator's Cedar policy must
permit it - the mechanics below are unchanged. Status by phase:

| phase | capability | status |
|---|---|---|
| 0 | Input types, shape validation (since moved to the manifest's `requires`) | implemented |
| 1 | `requires.filesystem.privateTmp` (policy `usePrivateTmp`); host mounts left out on purpose | implemented |
| 2 | `requires.egress.http` (policy `grantEgress`, also the host allowlist; CIDR rules `dialAddress`, in `--sandbox-policy-file`) | implemented |
| 3 | `requires.env` credential bindings (policy `setEnv` ∧ `spendCredential`) | implemented |
| 4 | WASI HTTP through components | not started |


## Today

Without a grant a module runs in a fresh Wasmtime store per request with
WASI preview 1. It has the clock and entropy, stdout/stderr inherited
from the pod, two host imports (`wasmfn.log`, and `wasmfn.http`, which
performs nothing without a grant), an epoch deadline (`--module-timeout`)
and a memory cap (`--module-memory-limit`). It has neither pre-opened
directories nor environment variables nor sockets (wasip1 has none).
Everything a module needs
arrives in the `RunFunctionRequest` - observed and desired state, context,
credentials, required resources - and everything it produces leaves in the
response. This covers composition logic in full. It doesn't cover a
template fetched from a volume, a call to an external system or a file a
sidecar wrote - which is what the grants below are for.


## Principles

1. **Capabilities are explicit grants.** Nothing is ambient. The module's
   manifest asks. The Input's `compositionPolicy` and the operator's Cedar
   policy must both permit. The module gets exactly its admitted request.
   Deny by default at the operator.
2. **The policy layers are the Composition's and the operator's, never the
   composite resource's.** `compositionPolicy` is read from the Input only,
   not through `module.from` - an XR author who can pick a module must not
   be able to widen what modules may do.
3. **The host enforces, the guest asks.** Filesystem paths and network
   requests are checked on the host side against the grant; policy is never
   trusted to the module.
4. **Bounded and observable.** Every grant has limits (bytes, requests,
   time) and every use is counted (metrics) and traceable (logs with the
   module digest).
5. **The ABI remains language-agnostic.** New capabilities are either WASI
   itself or a host import with a documented byte-level contract, so Rust
   and TinyGo guests aren't second-class.
6. **A module may request a capability, never take it.** The module
   manifest (`docs/one-pager-module-manifest.md`) lists the egress rules,
   the private `/tmp` and the env bindings a module can't run without -
   the request of the three-layer decision
   (`docs/one-pager-three-layer-authz.md`); the runtime refuses a module a
   policy layer doesn't permit, before the run - the manifest can only
   make a run fail earlier, the Cedar layers grant.

## Shape

The ask is stated in the module's manifest (`wasmfn.yaml` →
the artifact's manifest layer; the module-manifest one-pager), not in the
Input:

```yaml
# wasmfn.yaml
requires:
  filesystem:
    privateTmp: true             # a private, empty, writable /tmp for the duration of the request; nothing else is mountable
  egress:
    http:                        # egress through the host, HTTP(S) only
    - host: api.example.com      # exact host, or hostPattern: "*.internal.example.com" - exactly one
      methods: [GET, POST]       # at least one; nothing is admitted implicitly
      pathPrefix: /v1/           # optional
  env:                           # secret env: bindings to step credentials; non-secret configuration is the Input's config
  - name: DATABASE_URL
    fromCredential: {name: db, key: url}
```

The rule and binding formats are enforced by `internal/egress.ValidateRules` (host XOR
`hostPattern`, bare host names, normalized path prefixes) and
`internal/sandbox.ValidateBindings` (identifier names, credential name and
key) when the manifest is loaded or parsed - the gate that always runs.

The operator's Cedar `--sandbox-policy-file` is the one authority that enables a
sandbox capability - `usePrivateTmp` for the private `/tmp`, `setEnv` (with
`spendCredential` per binding) for the environment, `grantEgress` for egress
(also the host allowlist, with the CIDR block/allow rules as `dialAddress`). It's
evaluated default-deny, so with no policy file every `requires` entry is refused and
a runtime offers only the default sandbox. The Input's `compositionPolicy` may
narrow any of those actions further (scoped default-permit: it narrows only the
actions it writes rules for - the three-layer-authz one-pager). The per-run
egress budgets are fixed defaults and the rate limit is
`--egress-rate-limit-per-minute`/`-burst`. No flag maps a host
directory into a module: what a module can read is its request, what it can
write is its private `/tmp`. `internal/authz.LoadOperatorPolicy`,
`internal/egress.New` and `internal/sandbox.NewCeiling` check the policy once at
startup - a `--sandbox-policy-file` that doesn't parse (a malformed ip-rule
included), or an unwritable `$TMPDIR` when the policy can grant a private
`/tmp` - and refuse to start rather than fail every request that would hit the mistake.
Between load and run `admission.AdmitRequires` decides the module's ask.
The run gets its sandbox or a fatal result: with no policy, `module …
requires a private /tmp (requires.filesystem.privateTmp), but the runtime has
no --sandbox-policy-file, which is required to grant sandbox capabilities`; a
host the operator's Cedar policy doesn't grant is `module … requires egress
GET to host "evil.example.com" (requires.egress.http[0]), which the operator
policy (--sandbox-policy-file) does not permit`.


## Mechanics

**Filesystem - one WASI pre-open, the ABI unchanged (implemented).**
`privateTmp` is an `os.MkdirTemp` under the runtime's `os.TempDir()` -
`$TMPDIR`, `/tmp` in the image - created before the store, pre-opened
read/write at `/tmp` with `WasiConfig.PreopenDir(dir, "/tmp", DIR_READ|
DIR_WRITE, FILE_READ|FILE_WRITE)`, and the host removes it after it closes the
store on every path out of `Run` (success, trap, exit, deadline, host error; a
removal failure is logged, never a result). It's the only directory a
guest is ever given: host directories aren't mountable by design, so no
operator-declared path can be read by a module and no message ever mentions
one. `engine.RunOptions` has `PrivateTmp` and `Env` next to the run's
limits. With none set the store is byte-for-byte the default one. Go,
TinyGo and Rust guests see the directory through their standard file APIs
(`os.TempDir()` and `env::temp_dir()` already point at `/tmp` on WASI).
Wasmtime resolves each path inside its pre-open: `..` that would leave it
fails with `EPERM` (pinned by `internal/engine` tests through raw
`path_open`). The pre-open is descriptor 3, which isn't a contract: guests
discover it with `fd_prestat_*` as their runtimes do. Cost on the request
path: one `mkdir` and one `rm -rf`. `docs/abi.md` ("Sandbox") states the
guest-visible contract.

**Network - a host import first, WASI HTTP later (implemented, revision
0.4).** wasip1 modules can't open sockets, whatever the runtime. The
guest calls the host to perform an HTTP request: `wasmfn.http(req_ptr,
req_len) -> u64` with a JSON request `{method, url, headers, body}` and a
JSON response `{status, headers, body}` or `{status: 0, error}`, written by
the host into a buffer it obtains from the guest's own `wasmfn_alloc`
(called re-entrantly) and returned as `ptr<<32|len` - the byte protocol is
in `docs/abi.md`. The import is always provided and type-checked at load.
The grant decides per request. The host (`internal/egress`) applies the
grant (host or pattern, method, normalized path prefix - dot segments are
refused), resolves DNS itself and judges **every** address a name resolves
to against the block list - loopback, link-local, RFC 1918, CGNAT, ULA,
NAT64, IPv4-compatible, unspecified, multicast, reserved by default; `allowedCIDRs` punch
holes, `blockedCIDRs` add. It then dials the address it checked (without
re-resolution and without a proxy) and handles TLS itself, with its own
roots. It follows redirects re-checking each hop and enforces the budgets
(`timeout` 10 s, `maxRequests` 16 per run, `maxResponseBytes` 4 MiB,
`maxRedirects` 5). A request is also cut at the run's remaining deadline.
It records
`function_wasm_module_http_requests_total{outcome}` (`ok`, `refused`,
`budget`, `error`) plus one log line per request (method, host, path
without query, status or reason, bytes, duration - never headers or body).
Every failure is a response with status 0 and an error - never a trap. The Go
glue the scaffold vendors (`internal/wasmfn`) exposes `wasmfn.HTTPClient()`,
an `*http.Client` whose transport is the import, so Go code that takes an
`http.Client` (cloud SDKs, OpenAPI clients) works unmodified (about 3 MB on a
raw-proto guest); the TinyGo and
Rust scaffolds include a helper over the import (`HTTPGet`/`HTTPDo`,
`http::get`/`http::send`). When wasmtime-go gains component-model support and
guest toolchains emit WASI 0.2/0.3 components (Rust today, standard Go not
before a `wasip3` target lands), `wasi:http` can replace the import for
those guests, with the same host policy in front of it. Raw sockets stay
out: they move TLS and address policy into the guest, where the host can't
see them.
The evaluation on 2026-08-16 (probes against `extism/go-sdk` v1.7.1) tested
Extism's built-in `http_request` as a replacement for this import. Its
`allowed_hosts` is a hostname glob checked once before the request, on a
hard-coded `http.DefaultClient`. It ignores the method, the path and the
resolved addresses (it dials a name that resolves to loopback).
It follows redirects to other hosts, honours `HTTP_PROXY` and fails the
guest call on a refusal. You can't replace the host function without
forking the SDK. It doesn't meet the preceding requirements. AGENTS.md
records the decision to keep this import ("Not Extism").

**Environment - `SetEnv` from the admitted bindings (implemented).** The
manifest's `requires.env` bindings (`{name, fromCredential: {name, key}}`)
become `WasiConfig.SetEnv(keys, values)` on the run's store, sorted by key;
the runtime's own environment is never inherited, and without a granted
binding the guest's `environ` is empty. Each binding reads one key of a
step credential and needs a `setEnv` and a `spendCredential` permit in both
Cedar layers; literal values are left out on purpose: non-secret
configuration is the Input's `config`, read through `wasmfn.GetConfig`. The
pull credential (`module.oci.credentials`) is refused as a source - the
module must never see the secret that fetched it. Resolution happens once
the manifest is read (`sandbox.Materialize` in `fn.go`), against the
request's own credentials; shape validation (`ValidateBindings`) runs with
the manifest, the policy decision in `admission.AdmitRequires`.

## Threat model in one paragraph

With network on, a malicious or careless module can exfiltrate what it sees
in the request - including step credentials - to any host it's allowed to
reach. The allowlist, the CIDR denylist and the per-request audit line are
required parts of the network grant for that reason, and `--cosign-key` is
strongly recommended wherever egress is granted. With the filesystem the
risk would be reading host files the operator didn't mean to share, so
host directories aren't mountable at all. The only directory a
module gets is its private `/tmp`, per request, empty at the start and
removed at the end - one module can't leave state for the next or read
another's. It has no byte quota of its own: it's bounded by the filesystem
behind the runtime's `$TMPDIR` - point it at a tmpfs `emptyDir` with a
`sizeLimit` to cap what one request may write (a full tmpfs fails the
guest's write, not the runtime). Environment values come only from step
credentials the pipeline step already receives - a binding widens nothing
beyond what the request already included. The existing per-request deadline
and memory cap apply under every grant, and the HTTP budgets add response
size and request count. The module still runs with the
trust the Composition author gave it - the sandbox protects the runtime and
the operator's boundaries, not the data the Composition chose to hand the
module.

## Phasing

0. **Input types (implemented, since superseded):** the `sandbox` subtree
   with shape validation - added first, with a "not implemented yet"
   refusal, so the schema was settled before any behaviour; the ask later
   moved to the manifest's `requires` (revision 1.8, the three-layer-authz
   one-pager).
1. **Filesystem (implemented):** the private `/tmp`
   (policy `usePrivateTmp`). The smallest change, with the ABI unchanged. Named
   read-only host mounts were built and then removed (revision 1.1): a
   module's inputs come through the request, not through the pod's
   filesystem.
2. **HTTP egress (implemented):** the `wasmfn.http` import,
   `wasmfn.HTTPClient()`, the Cedar `--sandbox-policy-file` (`grantEgress` enabling
   egress and the host allowlist, `dialAddress` the CIDR rules), metrics and audit logging; the ABI
   extension is in `docs/abi.md`, the WAT fixture in the engine tests and a
   Go guest fixture (`internal/testwasm/testdata/httpguest`) in the host
   tests. The TinyGo and Rust scaffolds include a helper over the import
   (`http.go` + `http_wasip1.go`; `src/http.rs`) with a swappable host for
   native tests, and all three examples fetch `config.greetingUrl` through it.
   The host tests run every guest with and without a grant.
3. **Environment (implemented):** the policy `setEnv` action; env is a
   manifest `requires.env` credential binding, which `setEnv` and
   `spendCredential` must both permit.
4. **WASI HTTP through components:** revisit when wasmtime-go supports the
   component model and at least one supported guest toolchain targets it.

## Open questions

- A `path` or `http` source has no artifact layer for a manifest, but
  may point at its `wasmfn.yaml` by reference (`http.manifestURL`/`manifestDigest`,
  `manifestPath`) - the request layer for a source without an OCI layer,
  decided by the same two Cedar layers (docs/one-pager-manifest-less-sources.md,
  implemented). An OCI artifact pushed without a manifest layer still has an
  empty ask.
- Per-request budgets in the egress policy file only, or lowerable per Input
  the way `limits` narrows the run's timeout and memory? (v0.2.0 fixed them as defaults, not a file or per Input, with the rate
  limit a flag; the run's `limits.timeout` still bounds every
  request.)
- Should the audit log line be a structured `Result` on the response (visible
  in `crossplane render --include-function-results`) as well as a log?
- Data files a module needs (templates, CA bundles): through the request -
  `config`, context, or a resource the Composition requires - not through a
  host mount; the private `/tmp` covers scratch space. Host mounts stay out
  unless a use case appears that can't go through the request.
- Private `/tmp` size cap: WASI pre-opens don't come with a byte quota. A
  tmpfs volume with a size limit behind `$TMPDIR` is the answer for
  now (documented in the runtime flags table, `docs/operators/runtime-flags.md`).
