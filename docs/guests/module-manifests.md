# Module manifests

This page is for module authors deciding what to put in `wasmfn.yaml`. It
covers its contents, how the runtime checks it, the refusals you can expect
and the `composes` declaration. It assumes a scaffolded project
([Write a module](write-a-module.md)); what each policy layer permits is in
the [Cedar policy reference](../operators/cedar-policy.md).

## Manifest contents

`guestfn init` writes a `wasmfn.yaml` like this one (the Go scaffold's):

```yaml
# The module manifest: what this module declares about itself. guestfn build
# validates it, guestfn push publishes it beside the module as the artifact's
# manifest layer, and the runtime holds it against what the policy layers grant
# before every run - a manifest can refuse a run earlier and say why, never
# make one possible.
abi: 2
name: my-fn
version: 0.1.0
# requires: the sandbox capabilities the module cannot run without
# (egress.http rules, filesystem.privateTmp, env credential bindings),
# granted where the compositionPolicy and the operator's Cedar policy permit. This guest fetches a
# greeting only when config.greetingUrl is set, so it requires nothing and
# renders without any grant.
config:
  # The JSON Schema (draft 2020-12) of the Input's config; the runtime refuses
  # a request whose config does not match, naming the field.
  schema:
    type: object
    properties:
      greeting: {type: string}
      greetingUrl: {type: string, format: uri}
    additionalProperties: false
```

The fields:

- `abi`: the module's ABI version. Required, and `2` is the only
  value the runtime accepts.
- `name`, `version`, `description` and `source`: what the module says about
  itself.
- `requires`: the sandbox capabilities the module can't run without. These
  are egress rules (`egress.http`), `filesystem.privateTmp`, `env` credential
  bindings and the step `credentials` the module reads whole from its
  request.
- `config.schema`: a JSON Schema (draft 2020-12) for the Input's `config`.
  It must be self-contained: a `$ref` to a URL or a file is refused at load,
  and `pattern` takes the `regex` crate's syntax, which has no look-around
  and no back-references. Non-secret configuration belongs in `config`, not in env.
- `minRuntime`: the oldest runtime that serves the module.
- `composes`: what the module writes, advisory
  ([below](#the-composes-declaration)).

`guestfn push` publishes the file as a second layer of the OCI artifact, with
the media type `application/vnd.wasmfn.manifest.v1+json`. The layer is covered
by the manifest digest the Composition pins, and so by any cosign signature on
it.

A `path` or `http` source has no OCI manifest layer. It can reference its
`wasmfn.yaml` instead, with `module.manifestPath` or `module.http.manifestURL`
and `manifestDigest` ([Input reference](../operators/input-reference.md)). A
module with no manifest runs in the default sandbox: nothing but the request,
and no step credential.

## Policy check and refusals

A manifest is a request. After the module loads, the runtime checks each
`requires` entry against the Input's `compositionPolicy` and the operator's
`--sandbox-policy-file`, and a capability is granted only when both permit
it. The manifest can make a run fail earlier, with a message saying why. It
can't make a run possible or widen a grant.

A required capability that a layer doesn't permit is a fatal result before
the module runs:

```text
module oci ghcr.io/example/greeter@sha256:… requires egress GET to host "api.example.com" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit
… requires a private /tmp (requires.filesystem.privateTmp), which the compositionPolicy does not permit for this request
… requires credential "cmdb" (requires.credentials[0]), which the operator policy (--sandbox-policy-file) does not permit
```

A runtime older than the manifest's `minRuntime` and a `config` that doesn't
match the schema fail the same way, and a schema that reaches outside itself
is refused when the manifest is read:

```text
… requires runtime v0.3.0 or newer, this is v0.2.1
… config does not match the module's schema: /greeting: got number, want string
… has an invalid manifest: config.schema: Resource 'http://example.com/schema.json' is not present in a registry and retrieving it failed: $ref http://example.com/schema.json is external; config.schema must be self-contained
```

`guestfn push` prints the `requires:` block under the `module:` block. A
Composition author reads it to see what the layers must permit.
`guestfn inspect <ref>` shows what a published module requires, and
`function validate --resolve` applies the same checks offline.

## The `composes` declaration

A manifest can declare what the module writes, beside what it needs:

```yaml
composes:
  - kind: policy/PodDisruptionBudget   # <group>/<Kind>, the core group spelled core
    changes: [add]                     # add, modify, remove; all three when omitted
```

The declaration is for tooling. The policy layers fence a module's output
with `composeResource` rules, judged over the actual response after the run
([the output fence](../operators/cedar-policy.md#the-output-fence)), and the
runtime doesn't read `composes`.

The tooling reads it in three places:

- `function validate --resolve` warns where the step's `compositionPolicy` or
  the operator policy would refuse a declared kind and change, such as
  `the module declares it composes policy/PodDisruptionBudget (add), which the compositionPolicy does not permit (composeResource)`.
- `guestfn inspect` shows it in the manifest summary.
- `guestfn scaffold composition` writes the matching `composeResource`
  permits into the `compositionPolicy` skeleton it derives from `requires`.

`guestfn build` refuses an unknown field inside `composes`, which catches a
typo in the file you're editing. Parsing a published manifest doesn't check
`composes` for unknown fields. The runtime ignores them.
