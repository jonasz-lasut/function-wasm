# WASM Sandbox

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Draft, revision 0.2

How the sandbox could grant a module *some* filesystem, network or
environment access without giving up what makes it safe to run other people's
modules.

## Today

A module runs in a fresh wasmtime store per request with WASI preview 1 and:
no pre-opened directories, no environment variables, no sockets (wasip1 has
none), the clock and entropy, stdout/stderr inherited from the pod, one host
import (`wasmfn.log`), an epoch deadline (`--module-timeout`) and a memory
cap (`--module-memory-limit`). Everything a module needs arrives in the
`RunFunctionRequest` — observed and desired state, context, credentials,
required resources — and everything it produces leaves in the response. This
covers composition logic completely; it does not cover fetching a template
from a volume, calling an external system, or reading a file a sidecar wrote.

## Principles

1. **Capabilities are explicit grants.** Nothing is ambient. The operator
   sets a ceiling with runtime flags; a Composition asks for what its module
   needs; the module gets the intersection. Deny by default.
2. **The Composition asks, never the composite resource.** Sandbox settings
   live next to `module` in the Input and are not readable through `*From`
   fields — an XR author who can pick a module must not be able to widen its
   permissions.
3. **The host enforces, the guest asks.** Filesystem paths and network
   requests are checked on the host side against the grant; policy is never
   trusted to the module.
4. **Bounded and observable.** Every grant carries limits (bytes, requests,
   time) and every use is counted (metrics) and traceable (logs with the
   module digest).
5. **The ABI stays language-agnostic.** New capabilities are either WASI
   itself or a host import with a documented byte-level contract, so Rust
   and TinyGo guests are not second-class.

## Proposed shape

```yaml
apiVersion: wasm.fn.crossplane.io/v1beta1
kind: Input
module:                          # the module-source-schema one-pager's shape; today: module.oci
  type: OCI
  oci: {ref: ghcr.io/example/greeter:v1@sha256:…}
sandbox:                         # a top-level sibling of module, policy and limits
  filesystem:
    mounts:
    - name: templates            # an operator-provided mount, --guest-mount templates=/etc/wasm/templates:ro
      path: /templates           # where the guest sees it
    scratch: true                # a private, empty, writable /tmp for the duration of the request
  network:
    http:                        # egress through the host, HTTP(S) only
    - host: api.example.com      # exact host, or hostPattern: "*.internal.example.com"
      methods: [GET, POST]
      pathPrefix: /v1/           # optional
  env:                           # non-secret configuration; secrets keep coming through step credentials
    LOG_LEVEL: debug
```

Operator flags set the ceiling: `--guest-mount name=hostpath[:ro]` (repeatable;
only named mounts can be referenced), `--allow-scratch`, `--allow-http` with a
policy file (allowed hosts/patterns, blocked CIDRs, per-request budgets:
`timeout`, `maxRequests`, `maxResponseBytes`), `--allow-env`. A Composition
asking for something outside the ceiling gets a fatal result naming the
grant, before the module runs.

## Mechanics

**Filesystem — WASI pre-opens, no ABI change.** wasmtime already supports
`PreopenDir(hostPath, guestPath, dirPerms, filePerms)`; a mount becomes a
read-only pre-open (`DIR_READ`/`FILE_READ`), `scratch` a per-store
`os.MkdirTemp` pre-opened read-write and removed when the store is dropped.
Go, TinyGo and Rust guests see them through their standard file APIs. Pre-open
semantics already stop path escapes; the operator controls which host paths
exist at all. Cost: none on the request path beyond the temp dir.

**Network — a host import first, WASI HTTP later.** wasip1 modules cannot
open sockets, whatever the runtime, so the guest asks the host to perform an
HTTP request: `wasmfn.http(req_ptr, req_len) -> u64` with a protobuf/JSON
request `{method, url, headers, body}` and a response `{status, headers,
body}` returned through guest memory the same way `wasmfn_run` returns its
result. The host applies the allowlist (host, method, path prefix), resolves
DNS itself, refuses link-local, loopback and cluster CIDRs unless listed,
terminates TLS with its own roots (or the pod's proxy settings), enforces the
budgets, and records `function_wasm_module_http_requests_total{outcome}` plus
one log line per request (method, host, status, bytes; never the body). The
guest SDK exposes `wasmfn.HTTPClient()` — an `*http.Client` whose transport is
the import — so Go code that takes an `http.Client` (cloud SDKs, OpenAPI
clients) works unmodified; Rust and TinyGo call the import directly. When
wasmtime-go gains component-model support and guest toolchains emit WASI 0.2/0.3
components (Rust today, standard Go not before a `wasip3` target lands),
`wasi:http` can replace the import for those guests, with the same host
policy in front of it. Raw sockets stay out: they move TLS and address policy
into the guest, where the host cannot see them.

**Environment — `SetEnv` from the grant.** Non-secret values only; the
request already carries the step credentials, and keeping secrets out of the
environment keeps them out of anything the guest might log or write to
scratch.

## Threat model in one paragraph

With network on, a malicious or careless module can exfiltrate what it sees
in the request — including step credentials — to any host it is allowed to
reach; the allowlist, the CIDR denylist and the per-request audit line are
therefore not optional parts of the network grant, and `--cosign-key` is
strongly recommended wherever egress is granted. With filesystem on, the risk
is reading host files the operator did not mean to share, so only explicit,
named, read-only mounts are exposed and scratch is private per request. All
grants keep the existing per-request deadline and memory cap; the HTTP
budgets add response size and request count. Nothing here changes the fact
that the module runs with the trust the Composition author gave it — the
sandbox protects the runtime and the operator's boundaries, not the data the
Composition chose to hand the module.

## Phasing

1. **Filesystem:** named read-only mounts and scratch. Smallest change, no
   ABI change, immediately useful for templates and data files.
2. **HTTP egress:** the `wasmfn.http` import, `wasmfn.HTTPClient()`, the
   policy file, metrics and audit logging. Needs a documented ABI extension
   in `docs/abi.md` and a fixture in the engine tests.
3. **Environment:** trivial once the Input schema exists.
4. **WASI HTTP through components:** revisit when wasmtime-go supports the
   component model and at least one supported guest toolchain targets it.

## Open questions

- Should `sandbox` be allowed at all with `*From` module sources, or only for
  modules the Composition names statically? (Default proposal: allowed, since
  the grant is the Composition's; a stricter mode could refuse the pairing.)
- Per-request budgets as flags only, or lowerable per Input?
- Should the audit log line be a structured `Result` on the response (visible
  in `crossplane render --include-function-results`) as well as a log?
- Scratch size cap: a byte quota is not something WASI pre-opens give us; a
  tmpfs volume with a size limit on the pod is the pragmatic answer.
