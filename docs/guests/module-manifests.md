# Module manifests

This page is for module authors deciding what to put in `guestfn.yaml`. It
covers its contents, how the runtime checks it, the refusals you can expect
and the `composes` declaration. It assumes a scaffolded project
([Write a module](write-a-module.md)); what each policy layer permits is in
the [Cedar policy reference](../operators/cedar-policy.md).

## Manifest contents

`guestfn init` writes a `guestfn.yaml` like this one (the Go scaffold's):

```yaml
# The module manifest: what this module declares about itself. guestfn build
# validates it, guestfn push publishes it beside the module as the artifact's
# manifest layer, and the runtime holds it against what the policy layers grant
# before every run - a manifest can refuse a run earlier and say why, never
# make one possible.
contract: 2
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

- `type`: `wasm` or `oci`. Leave it out for a WebAssembly component, the
  default. `oci` marks an OCI image for the OCI runtime
  ([#239](https://github.com/jonasz-lasut/function-wasm/issues/239)), which
  function-wasm refuses by name until that package exists.
- `contract`: the version of the type's contract. Required. `2` is the only
  value for `wasm`, the component world, and `1` the only value for
  `oci`, the process contract. The field was `abi` through 0.6, and the
  old name is refused as unknown: an OCI image has no ABI.
- `image`: the digest-pinned reference of the image a `type: oci` module
  runs (`ghcr.io/acme/tagger@sha256:…`). `guestfn push --image` writes it
  into the published manifest, so the file may leave it out. A `type: wasm`
  manifest has no `image`, and the runtime refuses one that sets it.
- `name`, `version`, `description` and `source`: what the module says about
  itself.
- `requires`: the sandbox capabilities the module can't run without. These
  are egress rules (`egress.http`), `filesystem.privateTmp`, `env` credential
  bindings and the step `credentials` the module reads whole from its
  request.
The runtime reads the manifest with a YAML 1.2 parser written in Rust.
Unquoted `yes`, `no`, `on` and `off` are booleans, an unquoted `0755` or
`1_000` parses as `755` and `1000`, a merge key (`<<: *anchor`) merges, and
`.inf` and `.nan` are refused. Quote a value
that must stay a string. `function validate` reads a Composition file the
same way.

- `config.schema`: a JSON Schema (draft 2020-12) for the Input's `config`.
  It must be self-contained: a `$ref` to a URL or a file is refused at load,
  and `pattern` takes the `regex` crate's syntax, which has no look-around
  and no back-references. Non-secret configuration belongs in `config`, not in env.
- `minRuntime`: the oldest runtime that serves the module.
- `composes`: what the module writes, advisory
  ([below](#the-composes-declaration)).
- `requires.resources`: what the module asks Crossplane for, advisory
  ([below](#the-requiresresources-declaration)).

`guestfn push` publishes the file as a second layer of the OCI artifact, with
the media type `application/vnd.guestfn.manifest.v1+json`. The layer is covered
by the manifest digest the Composition pins, and so by any cosign signature on
it.

For a `type: oci` manifest, `guestfn push <ref> --image <image@sha256:…>`
publishes the wrapping artifact instead. Its one layer is the manifest with
`image` set, under the artifact type `application/vnd.guestfn.module.v1`
and OCI 1.1's empty config. The image itself is built and pushed by its own
toolchain. The Composition pins the artifact, a signature covers it, and the
manifest pins the image. The OCI runtime then pulls an image the signed
manifest names. `guestfn inspect` names that image in place of a module
layer. function-wasm refuses the artifact by name, since the OCI runtime's
package serves it.

A `path` or `http` source has no OCI manifest layer. It can reference its
`guestfn.yaml` instead, with `module.manifestPath` or `module.http.manifestURL`
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

## The `requires.resources` declaration

A manifest can declare what the module asks Crossplane for through its
response's `requirements`, under `requires` beside the capabilities:

```yaml
requires:
  resources:
    - kind: apiextensions.crossplane.io/EnvironmentConfig   # <group>/<Kind>, the core group spelled core
      by: [name]                                            # name, labels; both when omitted
```

The declaration is for tooling, and unlike the other `requires` entries it's
never granted or refused before the run. The policy layers fence a module's
reads with `requireResource` rules, judged over the actual selectors after the
run ([the read fence](../operators/cedar-policy.md#the-read-fence)), and the
runtime doesn't read `requires.resources`. The tooling reads it in the same
three places:

- `function validate --resolve` warns where a layer would refuse a declared
  kind, such as
  `the module declares it requires core/Secret (by name), which the compositionPolicy does not permit (requireResource)`.
  It judges a read by name with an empty name and a read by labels with no
  labels, and never with a namespace, so a rule conditioned on a value may
  warn for a selector the run would permit.
- `guestfn inspect` shows it in the manifest summary.
- `guestfn scaffold composition` writes the matching `requireResource`
  permits, with `context has name` or `context has labels` from `by`.

`guestfn build` refuses a `kind` that isn't `<group>/<Kind>`, a `by` entry
other than `name` and `labels`, and an unknown field inside an entry, as it
does for the rest of `requires`.
