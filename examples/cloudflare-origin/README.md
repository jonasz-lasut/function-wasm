# cloudflare-origin

Only Cloudflare may reach this origin: a [Crossplane](https://crossplane.io)
composition function, in async Rust compiled to a WebAssembly **component**
(ABI v2), run by [function-wasm](https://github.com/jonasz-lasut/function-wasm).

An `Origin` composite resource names a VPC and a port. The module composes
the origin's AWS security group and, when `spec.exposure` is `cloudflare`,
one ingress rule per range Cloudflare publishes, read from
`https://api.cloudflare.com/client/v4/ips` at every reconcile. The list's
etag and the number of ranges admitted land in `status.cloudflare`; when
Cloudflare adds or retires a range, the rules follow on the next reconcile.

```yaml
apiVersion: network.example.org/v1alpha1
kind: Origin
metadata:
  name: shop
  namespace: default
spec:
  region: eu-central-1
  vpcId: vpc-0a1b2c3d4e5f60718
  port: 443              # the default
  exposure: cloudflare   # or private (the default): no ingress at all
```

## Why a module

Locking an origin down to Cloudflare is a common Terraform pattern
(`data "http"` over Cloudflare's list feeding security group rules), but a
Composition has no step that reads a URL at reconcile time and turns the
answer into resources. A function could, and here that function needs no
package of its own, and no network beyond the one request it declares:

- **The network access is requested, granted and audited.** The module
  cannot open a socket. Its manifest (`wasmfn.yaml`) requests `GET` to
  `api.cloudflare.com` under `/client/v4/ips`; the runtime makes the
  request on its behalf only if the Composition's `compositionPolicy` and
  the operator's Cedar policy both permit it, after checking the resolved
  address against its block list, and writes one audit line per request.
- **A failed fetch keeps the rules in place.** An unreachable or malformed
  list is a fatal result, never an empty rule set: Crossplane applies
  nothing from a fatal pipeline, so the rules composed last time stay
  until the list can be read again. Every range is checked to be a CIDR of
  its family before it becomes a rule.
- **Rules are named after their ranges**, so a list change replaces only
  the rules whose ranges changed.

## Run it

The toolchain is **Rust 1.100 or newer** with the `wasm32-wasip3` target,
plus `protoc` for prost-build. Until Rust 1.100.0 is released,
`rust-toolchain.toml` pins its beta, which rustup installs with the target
the first time cargo runs here; delete the file once 1.100.0 is out.

```shell
cargo test            # the function's unit tests, natively
make build            # fn.wasm, a component
make render           # serve it with function-wasm and crossplane render example/xr.yaml
make render-check     # the render test, example/xprin.yaml (needs xprin)
```

A local render needs no internet and no AWS account. `example/` holds:

- `definition.yaml`: the `Origin` XRD (Crossplane v2, namespaced).
- `xr.yaml` and `xr-private.yaml`: one origin of each exposure.
- `composition.yaml`: one pipeline step, the module served from this
  directory under `example/wasmfn.local.yaml`, a manifest that asks for the
  fixture server instead of `api.cloudflare.com`.
- `fixtures/client/v4/ips`: a snapshot of Cloudflare's answer, which
  `../render.sh` serves on `127.0.0.1:9480`.
- `policy.cedar`: the operator policy the runtime runs under, granting the
  fixture host and permitting loopback, which the egress block list refuses
  by default.
- `crds/`: provider-upjet-aws's `SecurityGroup` and
  `SecurityGroupIngressRule` CRDs, vendored, so the render test validates
  what the module composes against the provider's own schemas.
- `xprin.yaml`: the render test, one case per XR.

## In a cluster

Each release publishes this example, signed keyless and attested, as
`ghcr.io/jonasz-lasut/wasmfn/examples/cloudflare-origin:v<version>` (the version in
`wasmfn.yaml`); the release's `function-wasm-examples-<release>.yaml` lists
its reference pinned by digest. To run your own build instead, publish it
with its manifest, then reference the digest `guestfn push` prints:

```shell
guestfn push ghcr.io/example/cloudflare-origin:v0.1.0
```

```yaml
  - step: cloudflare-origin
    functionRef:
      name: function-wasm
    input:
      apiVersion: wasm.fn.crossplane.io/v1
      kind: Input
      module:
        type: OCI
        oci:
          ref: ghcr.io/example/cloudflare-origin:v0.1.0@sha256:…
```

The operator grants the egress in the runtime's `--sandbox-policy-file`:

```cedar
permit (principal, action == Action::"grantEgress", resource == HostPattern::"api.cloudflare.com")
when { context.method == "GET" };
```

The composed rules select the security group through
`securityGroupIdSelector.matchControllerRef`, and need a
[provider-upjet-aws](https://github.com/crossplane-contrib/provider-upjet-aws)
v2 with its namespaced `ec2.aws.m.upbound.io` resources.

## Layout

- `src/lib.rs`: `run_function` over the prost messages, ordinary async
  Rust, natively testable with a fetch double.
- `src/bindings.rs` (wasm only): the wit-bindgen world, the async `run`
  export and the `wasi:http` fetch. The writer half of a body's trailers
  future must be **dropped** once the request is built: holding it keeps
  the body incomplete and hangs the send.
- `proto/run_function.proto`, `build.rs`, `wit/` and `rust-toolchain.toml`
  are the rust scaffold's plumbing (`guestfn init --lang rust`), kept
  identical to it: crossplane's `RunFunction` contract, its prost build, the
  guest's `local:guest` world over the vendored
  [ABI v2 world](https://github.com/jonasz-lasut/function-wasm/blob/main/docs/abi-v2.md)
  and the WASI 0.3 WIT the client needs.
