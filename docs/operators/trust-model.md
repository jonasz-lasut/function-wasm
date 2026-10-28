# Trust model

This page is for operators deciding what a function-wasm installation
trusts. It names who can do what, and which policy or flag limits each
party. It assumes Composition authors are trusted and that composite
resource (XR) authors and module code aren't. The reasoning and the threats
considered are in the
[design record for the trust model](../one-pager-trust-model.md).

## Module privileges

A module is the Composition author's code. It gets only what the
Composition's step and the operator's policy grant it. It sees the request's
observed state, desired state and context. Of the step credentials, it sees only those
granted to it: those its
[manifest](../guests/module-manifests.md) names in a `requires.env`
binding or in `requires.credentials`, and that both Cedar layers permit
through `spendCredential`. The runtime removes every other credential from
the request before the module runs.

The credential that pulled the module (`module.oci.credentials`) belongs to
the host. The runtime never forwards it, whatever the manifest asks.

The sandbox protects the runtime, and every other Composition sharing the
Function, from a module that crashes, loops or exhausts memory. Beyond
that, a module gets only what its manifest requests and both policy layers
permit ([Cedar policy](cedar-policy.md)):

- A private `/tmp` that exists for one request. Host directories can't be
  mounted.
- The environment variables its manifest binds to step credentials.
  Non-secret configuration goes in `config`.
- HTTP requests through the host ([HTTP egress](http-egress.md)).

## Modules chosen by an XR author

With `module.from`, the XR author picks the module. Use it only where you
trust XR authors to choose code, and limit the choice in two ways:

- Permit sources with `pullModule` rules in the Input's
  `compositionPolicy`. `OCI` and `HTTP` sources need one. Without it, the XR
  author could point the runtime at any host.
- Require signatures with `--cosign-key` or the keyless options in
  [Signatures](signatures.md).

The runtime reads `compositionPolicy` and `limits` from the Input only,
never from the XR. An XR author can choose code but can't widen its
permissions, grants or budget. The runtime enforces every rule on every request, because Crossplane never
installs the Input's CRD. [`function validate`](../guests/validate.md) runs
the same checks over a Composition offline.

## Credentials and the registry pull

A source read from the XR can specify a step credential only where a
`spendCredential` permit matches it and a `pullModule` permit admits the
repository. Otherwise the XR author would choose the registry host that
receives the secret. The runtime sends the pull credential to a registry
as HTTP Basic when the registry challenges for it, or exchanges it for a bearer
token.

Without such a permit, the runtime pulls an XR-chosen module anonymously or
with its own Docker config. Mount a Docker config through a
`DeploymentRuntimeConfig` and set `DOCKER_CONFIG`. Its credentials apply
only to their registry host.

## Egress

A module granted egress can forward its whole request to the hosts its
rules allow, step credentials included. Only the
policy layers grant egress: a manifest can request it, and an XR author
can't widen it. Every request leaves an audit line with the module digest.

Require signatures wherever egress is granted, so only code you've
approved can send data out ([Signatures](signatures.md)).

## Pinning

A remote module that a Composition names is pinned by a digest the
Composition states: the OCI reference's manifest digest or `http.digest`.
The runtime verifies the manifest and layer digests when it fetches, so the
code can't change without the Composition changing. This doesn't hold for
modules chosen with `module.from`, which the XR author picks, or for `Path`
sources, which have no digest.
