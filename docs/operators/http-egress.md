# HTTP egress

A module can be granted HTTP(S) requests **through the host**: it never
opens a socket (the sandbox links none), it asks the runtime, and the runtime
resolves the name, refuses addresses on its block list, terminates TLS with
its own roots, checks the host, method and path against the rules its
manifest declared and the policy layers granted, follows redirects within
them, enforces the operator's budgets, counts and logs every request, and
hands the response back. Three parties, in the order they decide:

1. **The operator** turns the capability on. Egress is enabled, and its host
   allowlist and SSRF CIDR rules are authored, in the Cedar `--sandbox-policy-file`
   (see [operator grant policy](grant-policy.md)); the per-run budgets are
   fixed defaults, and the one tunable budget - the rate limit - is a pair of
   flags:

   ```shell
   function --sandbox-policy-file /etc/function-wasm/policy.cedar \
     --egress-rate-limit-per-minute 60 --egress-rate-limit-burst 10
   ```

   With no `--sandbox-policy-file`, egress is not grantable at all: a
   module that requires it is a fatal result before it runs. A
   `grantEgress` permit that matches any host opens egress to any public host
   within the fixed budgets (timeout 10s, maxRequests 16, maxResponseBytes 4 MiB,
   maxRedirects 5; response headers are capped separately at 64 KiB) and the
   default block list. The **host allowlist** is the Cedar `grantEgress` action -
   which callers (`principal.namespace`, `principal.xrKind`) may grant which
   hosts, methods and paths, default-deny - and the **CIDR block/allow list** is
   the `Action::"dialAddress"` action over the `context.ip` extension:

   ```cedar
   // Only team-a may grant egress, and only to *.googleapis.com over GET/HEAD.
   permit (principal, action == Action::"grantEgress", resource)
   when { principal.namespace == "team-a" &&
          resource in HostPattern::"googleapis.com" &&
          ["GET", "HEAD"].contains(context.method) };

   // Block an internal range, then open one service inside it.
   forbid (principal, action == Action::"dialAddress", resource)
   when { context.ip.isInRange(ip("10.0.0.0/8")) };
   permit (principal, action == Action::"dialAddress", resource)
   when { context.ip.isInRange(ip("10.96.0.0/12")) };
   ```

   Each `dialAddress` condition is one ip test - `context.ip.isInRange(ip("CIDR"))`,
   `context.ip.isLoopback()`, or a `||` of those - and compiles **at load** into
   an ordered prefix list, so the dial path stays a few `Prefix.Contains` and
   **Cedar never runs per resolved IP**. A malformed rule is refused at startup,
   so `function validate` reports it too. What a rule may contain, and the
   error a malformed one gets, is in the
   [Cedar policy reference](cedar-policy.md).

   The default block list - loopback, link-local (the cloud metadata endpoint),
   RFC 1918, carrier-grade NAT (`100.64.0.0/10`, a common pod range), IPv6
   unique-local, the NAT64 and IPv4-compatible prefixes, and the unspecified,
   multicast and reserved ranges - applies to **every address a name resolves
   to** (a zoned IPv6 literal such as `[::1%25lo]` is never dialled), and the
   host dials the address it checked, so a name cannot rebind between the check
   and the connection. A `dialAddress` `forbid` adds to it, a `permit` punches a
   hole in it, and a `forbid` wins; to reach a loopback service in a local test
   both `127.0.0.0/8` and `::1/128` need a permit, since every resolved address
   is judged. `HTTP_PROXY` is not honoured: the host must see the destination
   address to judge it. What the guest is told about a refusal is only that the
   policy refused; the resolved address and the block-list entry stay in the
   runtime's audit line.

2. **The module** declares what it needs in its manifest, and the
   Composition may narrow it. The manifest's `requires.egress.http` rules -
   exactly one of `host` (exact name) and `hostPattern` (`*.example.com`:
   every name under it, not the apex), at least one of
   `GET HEAD POST PUT PATCH DELETE OPTIONS`, an optional `pathPrefix` the
   (normalized) path must start with - are the module's ask:

   ```yaml
   # wasmfn.yaml
   requires:
     egress:
       http:
       - host: api.example.com
         methods: [GET]
         pathPrefix: /v1/prices/
       - hostPattern: "*.googleapis.com"
         methods: [GET, POST]
   ```

   The Input's `compositionPolicy` narrows only when it scopes
   `grantEgress`: then a rule no permit matches is refused. A rule the
   operator's Cedar policy does not permit is a fatal result before the
   module runs (`module oci … requires egress GET to host
   "evil.example.com" (requires.egress.http[0]), which the operator policy
   (--sandbox-policy-file) does not permit`), and so is any egress
   requirement on a runtime with no `--sandbox-policy-file` at all (`…
   requires egress (requires.egress.http), but the runtime has no
   --sandbox-policy-file, which is required to grant egress (grantEgress)`).
   An XR author who picks a module through `module.from` picks its manifest
   with it, but both Cedar layers still gate every rule it declares.

3. **The module** makes requests. In Go, `wasmfn.HTTPClient()` is an
   `*http.Client` whose transport is the host, so anything that takes a
   client — cloud SDKs, generated API clients — works unchanged:

   ```go
   func init() { wasmfn.Register(&Function{log: wasmfn.NewLogger(), http: wasmfn.HTTPClient()}) }

   func (f *Function) RunFunction(ctx context.Context, req *fnv1.RunFunctionRequest) (*fnv1.RunFunctionResponse, error) {
       r, err := f.http.Get("https://api.example.com/v1/prices/eu")   // refused → *wasmfn.HTTPError with the host's reason
       …
   }
   ```

   Inject the client into your function so native tests can substitute an
   `httptest` server; outside a wasip1 build the transport fails with
   `wasmfn.ErrNoHostHTTP`, and under function-wasm it sends over
   `wasi:http@0.2` through wit-bindgen's Go bindings. The Zig and C
   scaffolds ship the same (`src/wasmfn.zig`, `src/wasmfn.c`) over the same
   `wasi:http@0.2` through wit-bindgen's C bindings, each with a swappable
   host so native tests can fake it.
   A request the host does not perform — no grant, host or method or path
   outside it, a blocked address, a budget, a transport failure — is a
   transport error naming the reason, never a trap; a status from the
   server, whatever it is, is a response. Calling `HTTPClient()` adds about
   3 MB to a raw-proto guest; a function-sdk-go guest already links what it
   needs.

Every request costs one line in the runtime's log with the module reference
and digest — method, host, path (never the query, headers or body), status
or the reason it was refused (plus, for a blocked address, the resolved
address and the block-list entry the guest is not told), response bytes,
duration, outcome — one more line per redirect hop, and one increment of
`function_wasm_module_http_requests_total{outcome}` (`ok`, `refused`,
`budget`, `error`; no host label). A guest that keeps calling without a
grant or past `maxRequests` gets one info line, then debug lines. A request
never outlives its run: it is cut short at the run's deadline
(`limits.timeout` or `--module-timeout`) if that comes before the policy's
`timeout` — and the run then ends as a timeout, so the guest does not get to
handle that error. With
egress on, a module can send whatever its request carries - the step
credentials it was granted included - to any host it is granted: grant narrowly, prefer
`pathPrefix`, and pair the capability with `--cosign-key` so only modules
your organisation signed run.
