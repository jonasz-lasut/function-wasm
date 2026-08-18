# WASM Sandbox

* Owner: Jonasz Małecki (@jonasz-lasut)
* Reviewers: Function WASM Maintainers
* Status: Implemented, revision 1.4

How the sandbox grants a module *some* filesystem, network or environment
access without giving up what makes it safe to run other people's modules.
The Input types (`input/v1beta1`: `sandbox.filesystem`, `sandbox.egress`,
`sandbox.env`, `sandbox.envFrom`, validated for shape by `internal/sandbox`) shipped in
revision 0.3; revision 0.4 implemented the filesystem (phase 1) and
environment (phase 3) grants with their operator flags, and phase 2 — HTTP
egress through the host (`sandbox.egress`, `--enable-sandbox-egress`,
`--sandbox-egress-policy`, the `wasmfn.http` import, `wasmfn.HTTPClient()`,
`internal/egress`); revision 1.0 is the merged result, every phase but the
component-model one implemented. Revision 1.1 removes host mounts (Jonasz,
2026-08-16): a Composition can no longer map an operator-declared host
directory into a module — the request is a module's only view of the world
beyond what it writes into its private `/tmp` — so the filesystem grant is
`privateTmp` alone, and `--enable-sandbox-mounts`/`--sandbox-mount` are
gone. Status by phase:

| phase | grant | status |
|---|---|---|
| 0 | Input types, shape validation | implemented |
| 1 | `sandbox.filesystem.privateTmp` (`--enable-sandbox-private-tmp`); host mounts deliberately not offered | implemented |
| 2 | `sandbox.egress.http` (`--enable-sandbox-egress`, `--sandbox-egress-policy`) | implemented |
| 3 | `sandbox.env`, `sandbox.envFrom` (`--enable-sandbox-env`) | implemented |
| 4 | WASI HTTP through components | not started |


## Today

Without a grant a module runs in a fresh wasmtime store per request with
WASI preview 1 and: no pre-opened directories, no environment variables, no
sockets (wasip1 has none), the clock and entropy, stdout/stderr inherited
from the pod, two host imports (`wasmfn.log`, and `wasmfn.http`, which
performs nothing without a grant), an epoch deadline (`--module-timeout`)
and a memory cap (`--module-memory-limit`). Everything a module needs
arrives in the `RunFunctionRequest` — observed and desired state, context,
credentials, required resources — and everything it produces leaves in the
response. This covers composition logic completely; it does not cover
fetching a template from a volume, calling an external system, or reading a
file a sidecar wrote — which is what the grants below are for.


## Principles

1. **Capabilities are explicit grants.** Nothing is ambient. The operator
   sets a ceiling with runtime flags; a Composition asks for what its module
   needs; the module gets the intersection. Deny by default.
2. **The Composition asks, never the composite resource.** Sandbox settings
   live next to `module` in the Input and are not readable through
   `module.from` — an XR author who can pick a module must not be able to
   widen its permissions.
3. **The host enforces, the guest asks.** Filesystem paths and network
   requests are checked on the host side against the grant; policy is never
   trusted to the module.
4. **Bounded and observable.** Every grant carries limits (bytes, requests,
   time) and every use is counted (metrics) and traceable (logs with the
   module digest).
5. **The ABI stays language-agnostic.** New capabilities are either WASI
   itself or a host import with a documented byte-level contract, so Rust
   and TinyGo guests are not second-class.
6. **A module may declare a grant as required, never take it.** The module
   manifest (`docs/one-pager-module-manifest.md`) lists the egress rules
   and the private `/tmp` a module cannot run without; the runtime
   refuses a Composition that grants less, before the run — the manifest
   narrows, the Composition still asks and the operator still caps.

## Shape

```yaml
apiVersion: wasm.fn.crossplane.io/v1beta1
kind: Input
module:                          # the module-source-schema one-pager's shape
  type: OCI
  oci: {ref: ghcr.io/example/greeter:v1@sha256:…}
sandbox:                         # a top-level sibling of module, policy and limits
  filesystem:
    privateTmp: true             # a private, empty, writable /tmp for the duration of the request; nothing else is mountable
  egress:
    http:                        # egress through the host, HTTP(S) only
    - host: api.example.com      # exact host, or hostPattern: "*.internal.example.com" — exactly one
      methods: [GET, POST]       # at least one; nothing is admitted implicitly
      pathPrefix: /v1/           # optional
  env:                           # non-secret configuration; secrets keep coming through step credentials
    LOG_LEVEL: debug
```

These types ship in `input/v1beta1` with the CRD schema (a CEL rule for
host XOR hostPattern, patterns for host patterns and path prefixes — for
tooling only, since Crossplane never installs the Input CRD) and
`internal/sandbox.Validate`, which enforces the same rules (plus: bare host
names, normalized path prefixes, no NUL in an env value) and is the gate
that always runs.

Operator flags set the ceiling, one `--enable-sandbox-<feature>` switch per
capability (off by default, `ENABLE_SANDBOX_<FEATURE>` in the environment)
and `--sandbox-<feature>-…` for what it declares:
`--enable-sandbox-private-tmp`, `--enable-sandbox-egress` with
`--sandbox-egress-policy <file>` (allowed `hosts` and `hostPatterns`,
`blockedCIDRs`/`allowedCIDRs` on top of a default block list, per-run
budgets `timeout`, `maxRequests`, `maxResponseBytes`, `maxRedirects`),
`--enable-sandbox-env`. There is no flag that maps a host directory into a
module: what a module can read is its request, what it can write is its
private `/tmp`. `internal/sandbox.NewCeiling` and `internal/egress.New`
check the flags once at startup — an unwritable `$TMPDIR` with the private
`/tmp` enabled, an egress policy file without `--enable-sandbox-egress` or
one that does not parse — and refuse to start rather than fail every
request that would hit the mistake. At request time `Ceiling.Grant` and
`Egress.Grant` turn the Input's `sandbox` into what the run gets, or a fatal
result naming the grant and the flag (`sandbox.filesystem.privateTmp is
refused: the runtime was started without --enable-sandbox-private-tmp`,
`sandbox.egress.http[0].host "evil.example.com" is outside the runtime's
egress policy (allowed: …)`), before the module is resolved.


## Mechanics

**Filesystem — one WASI pre-open, no ABI change (implemented).**
`privateTmp` is an `os.MkdirTemp` under the runtime's `os.TempDir()` —
`$TMPDIR`, `/tmp` in the image — created before the store, pre-opened
read-write at `/tmp` with `WasiConfig.PreopenDir(dir, "/tmp", DIR_READ|
DIR_WRITE, FILE_READ|FILE_WRITE)`, and removed after the store is closed on
every path out of `Run` (success, trap, exit, deadline, host error; a
removal failure is logged, never a result). It is the only directory a
guest is ever given: host directories are deliberately not mountable, so no
operator-declared path can be read by a module and no message ever names
one. `engine.RunOptions` carries `PrivateTmp` and `Env` next to the run's
limits; with none set the store is byte-for-byte the default one. Go,
TinyGo and Rust guests see the directory through their standard file APIs
(`os.TempDir()` and `env::temp_dir()` already point at `/tmp` on WASI).
wasmtime resolves every path inside its pre-open: `..` that would leave it
fails with `EPERM` (pinned by `internal/engine` tests through raw
`path_open`), and the pre-open is descriptor 3 — not a contract, guests
discover it with `fd_prestat_*` as their runtimes do. Cost on the request
path: one `mkdir` and one `rm -rf`. `docs/abi.md` ("Sandbox") states the
guest-visible contract.

**Network — a host import first, WASI HTTP later (implemented, revision
0.4).** wasip1 modules cannot open sockets, whatever the runtime, so the
guest asks the host to perform an HTTP request: `wasmfn.http(req_ptr,
req_len) -> u64` with a JSON request `{method, url, headers, body}` and a
JSON response `{status, headers, body}` or `{status: 0, error}`, written by
the host into a buffer it obtains from the guest's own `wasmfn_alloc`
(called re-entrantly) and returned as `ptr<<32|len` — the byte protocol is
in `docs/abi.md`. The import is always provided and type-checked at load;
the grant decides per request. The host (`internal/egress`) applies the
grant (host or pattern, method, normalized path prefix — dot segments are
refused), resolves DNS itself and judges **every** address a name resolves
to against the block list — loopback, link-local, RFC 1918, CGNAT, ULA,
NAT64, IPv4-compatible, unspecified, multicast, reserved by default; `allowedCIDRs` punch
holes, `blockedCIDRs` add — then dials the address it checked (no
re-resolution, no proxy), terminates TLS with its own roots, follows
redirects re-checking each hop, enforces the budgets (`timeout` 10 s,
`maxRequests` 16 per run, `maxResponseBytes` 4 MiB, `maxRedirects` 5; a
request is also cut at the run's remaining deadline), and records
`function_wasm_module_http_requests_total{outcome}` (`ok`, `refused`,
`budget`, `error`) plus one log line per request (method, host, path
without query, status or reason, bytes, duration; never headers or body).
Every failure is a response with status 0 and an error — never a trap. The
guest SDK exposes `wasmfn.HTTPClient()` — an `*http.Client` whose transport
is the import — so Go code that takes an `http.Client` (cloud SDKs, OpenAPI
clients) works unmodified (about 3 MB on a raw-proto guest); the TinyGo and
Rust scaffolds carry a helper over the import (`HTTPGet`/`HTTPDo`,
`http::get`/`http::send`). When wasmtime-go gains component-model support and
guest toolchains emit WASI 0.2/0.3 components (Rust today, standard Go not
before a `wasip3` target lands), `wasi:http` can replace the import for
those guests, with the same host policy in front of it. Raw sockets stay
out: they move TLS and address policy into the guest, where the host cannot
see them.
Extism's built-in `http_request` was evaluated as a replacement for this
import (2026-08-16, probes against `extism/go-sdk` v1.7.1): its
`allowed_hosts` is a hostname glob checked once before the request, on a
hard-coded `http.DefaultClient` — no method or path, no judgment of the
resolved addresses (a name resolving to loopback was dialled), redirects to
other hosts followed, `HTTP_PROXY` honoured, refusal by failing the guest
call — and the host function cannot be replaced without forking the SDK. It
does not meet the requirements above; the decision to keep this import is
recorded in AGENTS.md ("Not Extism").

**Environment — `SetEnv` from the grant (implemented).** `sandbox.env[]`
(list of `{name, value | valueFrom}`) and `sandbox.envFrom[]` (list of
`{credential, prefix}`) become `WasiConfig.SetEnv(keys, values)` on the
run's store, sorted by key; the runtime's own environment is never
inherited, and without a grant the guest's `environ` is empty. Literal
values are set directly; `valueFrom.credential` reads a key of a step
credential; `envFrom` imports every key of a credential (with an optional
prefix). The pull credential (`module.oci.credentials`) is refused as a
source for both - the module must never see the secret that fetched it.
Resolution happens after `registryAuth` (`sandbox.Materialize` in `fn.go`),
the first point where the pull credential's name is known; shape validation
(`Validate`) and the ceiling check (`Grant`) run at admission.

## Threat model in one paragraph

With network on, a malicious or careless module can exfiltrate what it sees
in the request — including step credentials — to any host it is allowed to
reach; the allowlist, the CIDR denylist and the per-request audit line are
therefore not optional parts of the network grant, and `--cosign-key` is
strongly recommended wherever egress is granted. With the filesystem the
risk would be reading host files the operator did not mean to share, which
is why host directories are not mountable at all: the only directory a
module gets is its private `/tmp`, per request, empty at the start and
removed at the end — one module cannot leave state for the next or read
another's. It has no byte quota of its own: it is bounded by the filesystem
behind the runtime's `$TMPDIR` — point it at a tmpfs `emptyDir` with a
`sizeLimit` to cap what one request may write (a full tmpfs fails the
guest's write, not the runtime). Environment values are as
public as the Composition that carries them. All grants keep the existing
per-request deadline and memory cap; the HTTP budgets add response size and
request count. Nothing here changes the fact that the module runs with the
trust the Composition author gave it — the sandbox protects the runtime and
the operator's boundaries, not the data the Composition chose to hand the
module.

## Phasing

0. **Input types (implemented):** `sandbox.filesystem.privateTmp`,
   `sandbox.egress.http[]`, `sandbox.env[]`, `sandbox.envFrom[]` with shape validation - shipped
   first, with a "not implemented yet" refusal, so the schema was settled
   before any behaviour.
1. **Filesystem (implemented):** the private `/tmp`
   (`--enable-sandbox-private-tmp`). Smallest change, no ABI change. Named
   read-only host mounts were built and then removed (revision 1.1): a
   module's inputs come through the request, not through the pod's
   filesystem.
2. **HTTP egress (implemented):** the `wasmfn.http` import,
   `wasmfn.HTTPClient()`, `--enable-sandbox-egress` and the
   `--sandbox-egress-policy` file, metrics and audit logging; the ABI
   extension is in `docs/abi.md`, the WAT fixture in the engine tests and a
   Go guest fixture (`internal/testwasm/testdata/httpguest`) in the host
   tests. The TinyGo and Rust scaffolds carry a helper over the import
   (`http.go` + `http_wasip1.go`; `src/http.rs`) with a swappable host for
   native tests, and all three examples fetch `config.greetingUrl` through it,
   so the host tests run every guest with and without a grant.
3. **Environment (implemented):** `--enable-sandbox-env`.
4. **WASI HTTP through components:** revisit when wasmtime-go supports the
   component model and at least one supported guest toolchain targets it.

## Open questions

- Should `sandbox` be allowed at all with `module.from` sources, or only for
  modules the Composition names statically? (Default proposal, and what is
  implemented: allowed, since the grant is the Composition's; a stricter mode
  could refuse the pairing.)
- Per-request budgets in the egress policy file only, or lowerable per Input
  the way `limits` narrows the run's timeout and memory? (Revision 0.4 ships
  them in the policy file only; the run's `limits.timeout` still bounds every
  request.)
- Should the audit log line be a structured `Result` on the response (visible
  in `crossplane render --include-function-results`) as well as a log?
- Data files a module needs (templates, CA bundles): through the request —
  `config`, context, or a resource the Composition requires — not through a
  host mount; the private `/tmp` covers scratch space. Host mounts stay out
  unless a use case appears that the request cannot carry.
- Private `/tmp` size cap: a byte quota is not something WASI pre-opens give
  us; a tmpfs volume with a size limit behind `$TMPDIR` is the answer for
  now (documented in the README's flags table).
