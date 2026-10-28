# HTTP egress

This page is for operators who let modules make HTTP and HTTPS requests. It
covers what to enable, the budgets, the block list and CIDR rules, and what
the runtime refuses and logs. It assumes a runtime started with a Cedar
`--sandbox-policy-file` (see [operator grant policy](grant-policy.md)); the
guest-side client is covered in [write a module](../guests/write-a-module.md).

A module never opens a socket. The runtime resolves the name and checks the
address against its block list, then sends the request over TLS with its own
roots. A request the runtime doesn't perform is a transport error that
states the reason, never a trap, and any status from the server is a
response.

A request is allowed only when all three layers agree:

1. The operator's Cedar policy enables egress.
2. The module's manifest lists the hosts, methods and paths it needs.
3. The Composition's `compositionPolicy` may narrow that list.

## Credentials and egress

With egress on, a module can send any host it's granted whatever its request
contains, including the step credentials granted to it. A `grantEgress`
permit that matches any host grants every public host. Grant narrowly, prefer `pathPrefix` in the manifest's rules, and pair the
capability with `--cosign-key` so only modules your organisation signed run
(see [signatures](signatures.md)).

## Enabling egress

With no `--sandbox-policy-file`, egress isn't grantable: a module that
requires it fails with a fatal result before it runs. With one, egress is
default-deny and the policy opens it through two actions:

- `grantEgress` decides which hosts, methods and paths a caller's modules
  (`principal.namespace`, `principal.xrKind`) may reach.
- `dialAddress` decides which resolved IP addresses the runtime may dial,
  over `context.ip`.

```shell
function --sandbox-policy-file /etc/function-wasm/policy.cedar \
  --egress-rate-limit-per-minute 60 --egress-rate-limit-burst 10
```

```cedar
// Only team-a may grant egress, and only to *.googleapis.com over GET/HEAD.
permit (principal, action == Action::"grantEgress", resource)
when { principal.namespace == "team-a" &&
       resource in HostPattern::"googleapis.com" &&
       ["GET", "HEAD"].contains(context.method) };

// Open one in-cluster service range: the default block list already
// refuses 10.0.0.0/8, and a permit punches a hole in it.
permit (principal, action == Action::"dialAddress", resource)
when { context.ip.isInRange(ip("10.96.0.0/12")) };
// Block a public range the default list does not cover. A forbid wins
// over any permit for the same address, so never pair it with a hole
// inside it.
forbid (principal, action == Action::"dialAddress", resource)
when { context.ip.isInRange(ip("203.0.113.0/24")) };
```

The schema and the refusal sentences are in the
[Cedar policy reference](cedar-policy.md).

## Budgets

Each run has fixed budgets: `timeout` 10 seconds, `maxRequests` 16,
`maxResponseBytes` 4 MiB and `maxRedirects` 5. Only the rate limit is
tunable, through `--egress-rate-limit-per-minute` and
`--egress-rate-limit-burst`.

The run's remaining deadline also bounds each request. The deadline is
`limits.timeout` or `--module-timeout`, and the time a guest waits for the
host's HTTP answer is credited back to that compute budget. A request is cut
short at the remaining deadline when that comes before the 10 seconds, and the
run then ends as a timeout, so the guest can't handle that error.

## Block list and CIDR rules

The default block list refuses these ranges:

- loopback
- link-local, which includes a cloud provider's metadata endpoint
- RFC 1918
- carrier-grade NAT (`100.64.0.0/10`)
- IPv6 unique-local
- the NAT64 and IPv4-compatible prefixes
- the unspecified, multicast and reserved ranges

It applies to every address a name resolves to. A zoned IPv6 literal such as
`[::1%25lo]` is never dialled. The runtime dials the address it checked, so a
name can't rebind between the check and the connection.

A `dialAddress` `forbid` adds a range to the list and a `permit` punches a
hole in it, and a `forbid` wins over a `permit`. To reach a loopback service
in a local test, both `127.0.0.0/8` and `::1/128` need a permit.

Each `dialAddress` condition is one IP test: `context.ip.isInRange(ip("CIDR"))`,
`context.ip.isLoopback()`, or a `||` of those. The conditions compile at load
into an ordered prefix list, and Cedar isn't evaluated per resolved IP. A
malformed rule is refused at startup, and `function validate` reports it too.

`HTTP_PROXY` isn't honoured, because the host must see the destination
address to judge it.

## Manifest rules

The manifest's `requires.egress.http` rules (see
[module manifests](../guests/module-manifests.md)) are what the module requests. Each
rule has exactly one of `host` (an exact name) and `hostPattern`
(`*.example.com`: every name under it, not the apex), at least one of
`GET HEAD POST PUT PATCH DELETE OPTIONS`, and an optional `pathPrefix` the
normalized path must start with.

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

The runtime checks every request and every redirect hop against these rules.
The Input's `compositionPolicy` narrows them only when it scopes
`grantEgress`, and a rule is then refused unless one of its permits matches it.
When an XR author picks a module through `module.from`, the module's manifest
comes with it, and both Cedar layers still gate every rule it declares.

A rule the operator's policy doesn't permit is a fatal result before the
module runs:

```text
module oci … requires egress GET to host "evil.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit
```

A runtime with no `--sandbox-policy-file` refuses any egress a manifest
requires:

```text
… requires egress (requires.egress.http), but the runtime has no --sandbox-policy-file, which is required to grant egress (grantEgress)
```

## Audit log and metric

The runtime writes one log line per request, with the module reference
and digest. The line contains:

- method, host and path, never the query, headers or body
- the status, or the reason for the refusal
- response bytes, duration and outcome
- for a blocked address, the resolved address and the block-list entry, which
  the guest isn't told

A redirect adds one line per hop. Repeated calls without a grant or past
`maxRequests` log one info line, then debug lines.

Each request also increments `function_wasm_module_http_requests_total{outcome}`,
where the outcome is `ok`, `refused`, `budget` or `error`. The series has no
host label.
