# cert-rotator

A [Crossplane](https://crossplane.io) composition function in C++ on
[ABI v2](https://github.com/jonasz-lasut/guestfn/blob/main/docs/abi-v2.md):
a WebAssembly component of about 160 KB, compiled by `zig c++` and run by
[function-wasm](https://github.com/jonasz-lasut/guestfn). It keeps a
TLS Secret's self-signed certificate fresh. A `ServerCertificate` composite
resource names the Secret, the common name, the DNS names and the validity;
the module asks Crossplane for that Secret through the response's
`requirements`, reads the certificate it holds, and composes the Secret
again: carried forward while the certificate has more than `renewBeforeDays`
left, with a freshly issued P-256 key and self-signed certificate otherwise
(absent, expiring, expired, or unreadable). What it found goes to
`status.certificate`.

It is the example of the two fences a module's response meets. The
Composition's `compositionPolicy` scopes both: `requireResource` lets the
module ask for Secrets by name in the composite resource's own namespace and
for nothing else, `composeResource` lets it compose Secrets and write its own
status and nothing else. A module that asked for another kind or another
namespace, or composed another kind, would be refused with a fatal result
naming the layer. The manifest declares both asks, `requires.resources` and
`composes`, for the tooling.

- `src/rotator.cpp` and `src/tls.cpp` are the function: the decision and the
  certificate work over [mbedTLS](https://github.com/Mbed-TLS/mbedtls)
  (X.509 parse and write, ECDSA over P-256, PEM, base64), configured by
  `src/cert_rotator_mbedtls_config.h` to what the sandbox has: no socket, no
  file, randomness from `getentropy` (`src/entropy.c`) and dates the guest
  converts itself. `build.zig.zon` fetches mbedTLS and nanopb; `build.zig`
  lists the mbedTLS sources the configuration leaves with code.
- `src/fn.c` is the C adapter between the generated protobuf structs and the
  C++ half (`src/rotator.h`): it reads the composite resource, asks for the
  Secret, hands the facts to `rotator_decide` and composes the Secret, the
  status, a result and a condition. C rather than C++ because nanopb's
  `ResourceSelector` carries a field named `namespace`, which no C++
  translation unit can include. The rest is the `c` flavour's plumbing,
  byte for byte: `src/wasmfn.c` (the world's `run` export, the typed `log`
  import), `src/structpb.c`, the nanopb codec in `src/fnv1` from the vendored
  `proto/`, and wit-bindgen's C bindings in `src/gen` from `wit/`.
- `zig build test` runs `src/rotator_test.cpp` (the crypto and the decision)
  and `src/fn_test.c` (the function end to end over the structs) natively.
  The module needs `wasi_snapshot_preview1` for the clock and the
  randomness, so `guestfn build` links the wasip1 adapter it carries into
  the component.

```shell
# Unit tests run natively.
zig build test

# Compile to a component: zig build, then guestfn wraps zig-out/bin/fn.wasm.
make build                          # guestfn build (from this repository) → fn.wasm

# What the runtime sees.
guestfn inspect fn.wasm             # ABI v2, exports run, imports log; manifest: requires resources core/Secret (name), composes core/Secret

# Publish it as an OCI artifact; it prints the module block for the Composition.
guestfn push ghcr.io/example/cert-rotator:v0.1.0
```

Reference the module from a Composition step of function-wasm, with the
policy that fences it ([`example/composition.yaml`](example/composition.yaml)):

```yaml
- step: cert-rotator
  functionRef:
    name: function-wasm
  input:
    apiVersion: guestfn.crossplane.io/v1
    kind: Input
    module:
      type: OCI
      oci:
        ref: ghcr.io/example/cert-rotator:v0.1.0@sha256:…
    compositionPolicy: |
      permit (principal, action == Action::"requireResource", resource == Kind::"core/Secret")
      when { context has name && context has namespace && context.namespace == principal.namespace };
      permit (principal, action == Action::"composeResource", resource == Kind::"core/Secret");
      permit (principal, action == Action::"composeResource", resource)
      when { context.target == "composite" };
```

`make render` runs it locally: the runtime serves this directory under
`example/policy.cedar`, and `crossplane render` composes
[`example/xr.yaml`](example/xr.yaml) with the Secret of
[`example/required-resources.yaml`](example/required-resources.yaml) as what
Crossplane found, a certificate openssl issued for one day, so the render
shows a rotation. In a cluster, install [`example/definition.yaml`](example/definition.yaml) and the
Composition, create a `ServerCertificate`, and the module composes the Secret
on the first reconcile and reads back what it wrote on every later one.

Rotation is what a reconcile does when the certificate has less than
`renewBeforeDays` left, so set the Composition's reconcile interval, or
Crossplane's poll interval, shorter than that window.
