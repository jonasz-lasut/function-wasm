# WASM sandbox

* Owner: @jonasz-lasut (Jonasz Małecki)
* Reviewers: function-wasm maintainers
* Status: implemented, revision 1.9 (rewritten in the present tense over the Rust workspace; host mounts removed (Jonasz, 2026-08-16); Cedar is the one authority that enables a capability (Jonasz, 2026-08-19); the ask is stated in the module's manifest)

How the sandbox grants a module some filesystem, network or environment
access without giving up what makes it safe to run other people's modules.
The operator-facing reference is [HTTP egress](operators/http-egress.md) and
[Cedar policy](operators/cedar-policy.md). This record gives the reasoning.

## Background

A module runs in a fresh Wasmtime store per request. By default it has the
clock and entropy, stderr and stdout inherited from the pod, the typed `log`
import, an epoch deadline (`--module-timeout`) and a memory cap
(`--module-memory-limit`). It has nothing else: the guest sees an empty environment, and WASI offers it neither a directory nor a socket. Everything a module needs arrives in the
`RunFunctionRequest` (observed and desired state, context, credentials,
required resources) and everything it produces leaves in the response. That
covers composition logic in full. It doesn't cover a call to an external
system or a secret read from the environment. The grants below are for those.

## Principles

1. Capabilities are explicit grants. Nothing is ambient, and the operator
   denies by default.
2. The policy layers are the Composition's and the operator's, never the
   composite resource's. `compositionPolicy` is read from the Input only,
   not through `module.from`: an XR author who can pick a module must not be
   able to widen what modules may do.
3. The host enforces and the guest asks. Paths and network requests are
   checked on the host against the grant; the module is never trusted with
   policy.
4. Grants are bounded in bytes, requests and time. The metrics count their
   use and each log line has the module's digest.
5. The ABI is language-agnostic. A capability is WASI itself or a host
   import with a documented contract, so no guest language is second-class.
6. A module may request a capability, never take it. Its manifest lists the
   egress rules, the private `/tmp` and the env bindings it can't run
   without ([module manifest](one-pager-module-manifest.md)); a capability is
   granted only where both policy layers permit it
   ([three-layer authorization](one-pager-three-layer-authz.md)). The manifest
   can make a run fail earlier, never succeed.

## Design

The ask is stated in the module's manifest, not in the Input:

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

The manifest loader (`crates/function/src/manifest.rs`, `egress_rules.rs`,
`sandboxenv.rs`) checks these formats. A rule has exactly one of `host` and `hostPattern`, host
names are bare, path prefixes are normalized and binding names are
identifiers.

The operator's Cedar `--sandbox-policy-file` is the one authority that enables
a capability: `usePrivateTmp` for the private `/tmp`, `setEnv` (with
`spendCredential` per binding) for the environment, `grantEgress` for egress
(also the host allowlist) and `dialAddress` for the CIDR rules. It's evaluated
default-deny, so with no policy file every `requires` entry is refused and the
module gets only the default sandbox, which is all most modules need
(Jonasz, 2026-08-19). The Input's `compositionPolicy` may narrow any of
those actions (scoped default-permit: it narrows only the actions it writes
rules for). `admission::admit_requires` decides between load and run, and the
run gets its sandbox or a fatal result such as `module … requires a private /tmp
(requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file,
which is required to grant sandbox capabilities`.

At startup the runtime exits on a `--sandbox-policy-file` that doesn't parse
(a malformed IP rule included). It also exits on an unwritable `$TMPDIR` when
the policy can grant a private `/tmp`. Both mistakes would otherwise fail
every request that hit them.

## Mechanics

### Filesystem

`privateTmp` is a per-run directory under the host's temp dir (`$TMPDIR`),
named `function-wasm-private-tmp-*` and created before the store
(`crates/engine/src/sandbox.rs`). It's pre-opened read/write at `/tmp`,
and removed when the run's value drops, on every path out (success, trap,
exit, deadline, host error); a removal failure is logged, never a result. It's
the only directory a guest is ever given, so a module can't read an
operator-declared host path. The ABI is unchanged:
guests use their language's file APIs, and their temp directory already
defaults to `/tmp` on WASI. The cost on the request path
is one `mkdir` and one removal. `docs/abi-v2.md` states the guest-visible
contract.

The directory has no byte quota of its own. Only the filesystem behind
`$TMPDIR` limits it. Put a tmpfs `emptyDir` with a `sizeLimit` there to cap a
request's writes: a full tmpfs fails the guest's write, not the runtime
([runtime flags](operators/runtime-flags.md)).

### Network

A module can't open a socket. It asks through `wasi:http` (0.3 `client.send`
and 0.2 `outgoing-handler`), and `crates/engine/src/wasihttp.rs` bridges both
onto one `HttpRequester` grant that `crates/function/src/egress.rs`
provides. The policy stays in the runtime, and every guest language
with a `wasi:http` client has egress. The host:

- applies the grant on the first request and on every redirect hop: host or
  pattern, method, and a normalized path prefix (dot segments are refused);
- resolves DNS itself and judges every address a name resolves to against the
  block list (`DEFAULT_BLOCKED_CIDRS`: loopback, link-local, RFC 1918, CGNAT,
  ULA, NAT64, IPv4-compatible, unspecified, multicast, reserved); the
  operator's `dialAddress` rules punch holes or add blocks, and an explicit
  block takes precedence;
- dials the address it checked without re-resolving or using a proxy, and
  handles TLS itself;
- enforces fixed per-run budgets (`timeout` 10 s, 16 requests, 4 MiB of
  response, 5 redirects) and the process-wide rate limit
  (`--egress-rate-limit-per-minute`, `--egress-rate-limit-burst`); a request
  is also cut at the run's remaining deadline;
- counts `function_wasm_module_http_requests_total{outcome}` and writes one log line
  per request (method, host, path without query, status or reason, bytes and
  duration, never headers or body).

A failure returns to the guest as `internal-error` with the refusal string,
never as a trap. Raw sockets stay out: they would move TLS and address policy
into the guest, where the host can't see them.

### Environment

The manifest's `requires.env` bindings (`{name, fromCredential: {name, key}}`)
become the run's WASI environment (`sandboxenv::materialize`, applied in
`sandbox.rs`), resolved against the request's own credentials. The runtime's
environment is never inherited, so without a granted binding the guest's
`environ` is empty. Each binding needs a `setEnv` and a `spendCredential`
permit in both Cedar layers. Literal values aren't allowed: non-secret
configuration is the Input's `config`, read through `GetConfig`. The pull
credential (`module.oci.credentials`) is refused as a source, since a module
must never see the secret that fetched it. The same rule keeps it out of
`requires.credentials`.

## Threat model

With egress granted, a malicious or careless module can leak what it
sees in the request, step credentials included. The destination is any host
the grant allows. The
host allowlist, the block list and the per-request audit line are required
parts of the grant for that reason, and `--cosign-key` is recommended wherever
egress is granted. The private `/tmp` is per request, empty at the start and
removed at the end: one module can't leave state for the next or read
another's. Environment values come only from step credentials the step
already receives. A binding doesn't widen what the request included.
The per-request deadline and memory cap apply under every grant. The
sandbox protects the runtime and the operator's boundaries. It doesn't protect
the data the Composition passes to the module.

## Alternatives

**Host mounts.** A Composition could map an operator-declared host directory
into a module, read-only. The project built it, then removed it (Jonasz, 2026-08-16): a
module's inputs come through the request, so mapping part of the pod's
filesystem into a module is a boundary the runtime doesn't offer. Data a
module needs (templates, CA bundles) travels through `config`, the context or
a required resource.

**Extism's `http_request`.** Evaluated on 2026-08-16 with probes against
`extism/go-sdk` v1.7.1. Its `allowed_hosts` is a hostname glob checked once
before the request, on a hard-coded `http.DefaultClient`: it ignores the
method, the path and the resolved addresses (it dials a name that resolves to
loopback). It follows redirects to other hosts and honours `HTTP_PROXY`. A
refusal fails the guest call. The host function can't be replaced without forking
the SDK, so it couldn't meet the requirements.

**A custom host import for HTTP.** Under ABI v1 the guest called a JSON
`wasmfn.http` import. `wasi:http` superseded it, which guests' own
toolchains already speak, with the same host policy in front of it (ABI v1 was
removed in 1.0.0, `docs/abi-v2.md`).

**Enabling capabilities by flag.** Per-capability `--enable-sandbox-*` flags
were replaced by the Cedar policy as the only authority (Jonasz, 2026-08-19,
[Cedar policy](operators/cedar-policy.md)).

**The ask in the Input.** The Input once had a `sandbox` block. The
manifest's `requires` replaced it. A module states what it can't run without,
and the Input's `compositionPolicy` only narrows it
([three-layer authorization](one-pager-three-layer-authz.md)).

## Open questions

- Should the audit line also be a structured `Result` on the response, visible
  in `crossplane render --include-function-results`, as well as a log line?
- Should the per-request budgets be lowerable per Input, the way `limits`
  narrows the run's timeout and memory? Today they're fixed defaults
  (`egress.rs`), not a policy file or an Input field. The run's
  `limits.timeout` still bounds every request.
