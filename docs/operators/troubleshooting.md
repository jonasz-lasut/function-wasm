# Troubleshooting

This page is for the operator of a function-wasm pod and for the module author whose module the runtime refuses. Find the message in the pod's logs, in a step's fatal result or in the output of `function validate` or `guestfn`, then follow the entry. Entries are grouped by what failed and match the current release. Each quoted message is a prefix or a fragment: match on the part that doesn't vary.

## Module load

- **`cannot instantiate module: trap: …`**: the guest trapped while the runtime instantiated it, such as by panicking in its start-up code. The stack is in the function pod's stderr.
- **`component does not implement the wasmfn:function@2.0.0 world: …`**: the component doesn't typecheck against `wit/wasmfn-function.wit`: it has no `run` export (`no export `run` found`) or imports something the runtime doesn't provide. Rebuild against the current `wit/`, and use `guestfn inspect fn.wasm` to list what the module exports and imports.
- **`cannot load module …: cannot compile module: failed to parse WebAssembly module: threads support is not enabled`**, or the same for another proposal: the module uses a WebAssembly proposal the engine refuses: threads, GC, function references, exceptions, tail calls, multi-memory or memory64 ([ABI v2](../abi-v2.md) lists what it accepts). Build the guest without that feature, which no scaffold's toolchain emits by default.
- **`cannot load module …: module is a core module, which function-wasm 1.0.0 no longer runs (ABI v1 was removed); build it as an ABI v2 component (docs/abi-v2.md)`**: rebuild the guest as a component. For a Go guest, `guestfn build` turns the core module that `go build` emits into one; a guest written for ABI v1 is rebuilt on a current scaffold (`guestfn init`, then move the function over). `guestfn inspect` and `function validate --resolve` print the same sentence.
- **`built fn.wasm, but the runtime would refuse it: …`** from `guestfn build`, or a refusal from `guestfn push`: the message is the runtime's own load-time refusal. The entries before this one cover the common ones.
- **`module.oci.ref … tags are not supported`**: pin the reference to the manifest digest, `repo@sha256:…` or `repo:tag@sha256:…`, as `guestfn push` prints it.
- **`module layer is a tar archive without /fn.wasm`**: a `FROM scratch` image must `COPY` the module to `/fn.wasm`. Prefer `guestfn push` or `oras push`, which write a raw `application/wasm` layer.

## Input and admission

`function validate` runs the same checks as the runtime. Run it with the flags you start the runtime with: exit 1 means a step is refused and the line states the reason, and exit 2 means the tool itself failed. The fields are in the [Input reference](input-reference.md).

- **`module.path is refused`**: the runtime was started without `--module-dir`.
- **`module.from: … names a OCI source, but the Input has no compositionPolicy`**: a `module.from` source that isn't static needs a `compositionPolicy` whose `pullModule` permits its repository. Add the policy or make the source static.
- **`module.from: cannot read status.module from the composite resource: module: no such field`**: the composite resource hasn't set the field the step reads. Set it, or add `module.allowEmpty: true` so the step runs nothing until it's set. The pod logs the skipped step. See [Metrics](metrics.md).
- **`module.allowEmpty is set but module.from is not`**: `allowEmpty` applies only to a field the composite resource may leave unset. Remove it, or switch the step to `module.from`.
- **`apiVersion wasm.fn.crossplane.io/v1beta1 is deprecated and is removed in function-wasm 2.0.0; write apiVersion: wasm.fn.crossplane.io/v1 (the same fields)`**: a warning from `function validate` and `guestfn build`, and a log line per request. Change the `apiVersion` line.
- **`limits.memory 1Gi exceeds the runtime's --module-memory-limit of 512Mi`**, or the same for `limits.timeout` and `--module-timeout`: lower the limit or raise the flag.
- **`module oci … has an invalid manifest: config.schema: Resource '…' is not present in a registry and retrieving it failed: $ref … is external; config.schema must be self-contained`**: the module's `config.schema` references a URL or a file. The runtime never fetches a reference. The author inlines the referenced schema. A `#/$defs/…` reference inside the schema is fine.
- **`module oci … config does not match the module's schema: /greeting: got number, want string`**: the Input's `config` fails the module's `config.schema`.

## Policy grants

The layers are explained in [Cedar policy](cedar-policy.md), and the operator's side in [Grant policy](grant-policy.md).

- **`module oci … requires egress GET to host "x" (requires.egress.http[0]), which the operator policy (--sandbox-policy-file) does not permit`**: the manifest requests a capability that the layer named in the message doesn't permit. Variants:
  - `… which the compositionPolicy does not permit`: the composition layer refuses it.
  - `module … requires a private /tmp (requires.filesystem.privateTmp), but the runtime has no --sandbox-policy-file, which is required to grant sandbox capabilities`, and the same for env and egress: the runtime has no policy file at all.
  - `module oci … requires credential "cmdb" (requires.credentials[0]), which the operator policy (--sandbox-policy-file) does not permit`, or `… but the runtime has no --sandbox-policy-file, which is required to grant step credentials (spendCredential)`: the layer has no `spendCredential` permit for `Credential::"cmdb"`.
  - `requires runtime vX or newer, this is vY`: the module needs a newer runtime.

  Add a `permit` to the layer named, or mount a policy file. A module that requests less avoids the refusal.
- **A module stops seeing a step credential it used to read**: the runtime forwards only the credentials a module was granted. Declare it in the manifest, with `requires.credentials: [name]` for the whole credential or a `requires.env` binding for one key. Then permit `spendCredential` for it in the operator policy, and in a `compositionPolicy` that scopes the action. `function validate --resolve` lists what each step's module receives, and the runtime logs the withheld names at debug level. See [Module manifests](../guests/module-manifests.md).
- **`module … requires.credentials[0]: the request carries no credential "cmdb"; declare it on the pipeline step`**: add the credential to the step's `credentials`.
- **`the operator policy grants a private /tmp (usePrivateTmp), but the runtime cannot create one under …`** at startup: point `TMPDIR` at a writable directory. An `emptyDir` with a `sizeLimit` also bounds what a module may write.
- **A guest gets `EPERM` under `/tmp`**: the path is outside the private `/tmp`, the only directory the guest has.
- **`operator policy …: dialAddress rule "…" must scope the action as == Action::"dialAddress"`**, or another Cedar load error: the `--sandbox-policy-file` is malformed and refused at load. A `dialAddress` condition accepts only `context.ip.isInRange(ip("CIDR"))`, `context.ip.isLoopback()` or a `||` of them.
- **`module oci …: composed apps/Deployment "deployment" (modify), which the compositionPolicy does not permit (composeResource)`**, or `… which the operator policy (--sandbox-policy-file) does not permit …`, or `… composed platform.example.org/WebApp (composite, modify), which …`: the module changed a resource that no `permit` of the scoped layer matches, and the runtime applied none of its output. Widen the layer named or use a module that composes less. The rules are in the [Cedar policy reference](cedar-policy.md#the-output-fence).
- **`module oci …: composed resource "thing" states no apiVersion or kind, so the policy layers cannot judge it (composeResource)`**: a resource the module returned in `desired.resources` has no `apiVersion` or `kind` string. Fix the module.

## Signatures

Signing, keyless verification and the trusted root are explained on the [Signatures](signatures.md) page.

- **`cannot verify module oci …: the operator policy requires a cosign signature, but the runtime has neither --cosign-key nor --cosign-trusted-root to verify it`**: add `--cosign-key`. For keyless signatures add `--cosign-trusted-root` and a `trustSigner` rule instead. Or narrow the `requireSignature` rule.
- **`cannot verify module oci …: <ref> carries no cosign signature (no Sigstore bundle among its referrers)`**: none of the manifest's OCI 1.1 referrers is a Sigstore bundle whose subject is this digest. Sign the module with cosign 3. A module signed only with cosign 2 or with `--new-bundle-format=false` (a `sha256-<hex>.sig` tag) lands here too: the legacy format isn't read, so re-sign it.
- **`cannot verify module oci …: no valid cosign signature for <ref>: signature does not verify with the configured keys`**: a bundle exists but no `--cosign-key` key signed it. The same list also reports `signed statement's predicate type is "https://slsa.dev/provenance/v1", not https://sigstore.dev/cosign/sign/v1` (an attestation), `signed statement is for another digest` and bundle fetch, size or digest failures.
- **`… keyless signature cannot be verified: the runtime has neither --cosign-trusted-root nor --cosign-trust-instance`**: the signature is keyless. Add `--cosign-trusted-root` or `--cosign-trust-instance public-good`, and a `trustSigner` rule for the signer. The reverse, `key-based signature cannot be verified: the runtime has no --cosign-key`, means a key-based bundle met a runtime with only a trusted root.
- **`… keyless signature cannot be verified: the trusted root of <instance> has not been fetched yet (the last attempt failed: …)`**: `--cosign-trust-instance` is set and every fetch has failed. The message quotes the last failure. The runtime retries every minute and admits the module once a refresh succeeds, which the log reports as `Trusted root updated`. For a pod without network, mount the root with `--cosign-trusted-root`.
- **`--cosign-trusted-root and --cosign-trust-instance are mutually exclusive: the file is the trusted root, or the instance's root is fetched over TUF`** at startup, or `--cosign-tuf-refresh must be at least 1m0s`: choose the file for an air-gapped pod and the instance for one that can fetch.
- **`… keyless signature does not verify against the trusted root: <reason>`**: the runtime refused the bundle, and the reason is the verification library's own text. Typical causes are a signature made under a different Sigstore instance, an invalid signature or log promise, or a bundle with neither a Rekor entry nor an RFC 3161 timestamp. Run `cosign verify` with the certificate identity and issuer against the same root to tell which.
- **`… keyless signature by <identity> (issuer <issuer>) is not trusted for <repository>: no trustSigner rule in the operator policy permits it`**: the signature verifies but no `trustSigner` rule covers that signer for the repository. Add or widen a rule for the identity and issuer.

## Egress

Egress is explained on the [HTTP egress](http-egress.md) page.

- **`sandbox.egress: <host> resolves to an address the egress policy blocks`**: the host refuses private, loopback, link-local and cluster ranges by default. Add the range to the policy's `allowedCIDRs` to permit an in-cluster service.
- **`sandbox.egress: HTTP egress is not granted to this module: its manifest requires no egress (requires.egress.http)`**: the module made an HTTP request but its manifest doesn't require egress. Add `requires.egress.http` to the manifest. A Go guest sees the message prefixed with `wasmfn: `.
- **`sandbox.egress: the module's request rate exceeds the egress policy's rateLimit`**: raise `--egress-rate-limit-per-minute` and `--egress-rate-limit-burst`, or reduce the module's request frequency.

## Runtime and startup

- **`cannot create cache directory /tmp/function-wasm-cache: …`** at startup: the pod's filesystem is read-only there. Mount an `emptyDir` at that path through a `DeploymentRuntimeConfig`.
- **`module … failed: waiting for a run slot: deadline exceeded`**, or the step-slot and run-memory forms: the request's deadline passed while it waited for a bound, so nothing ran. Raise the bound, shorten runs, or watch `function_wasm_module_runs_in_flight` (see [Metrics](metrics.md)).
- **`cannot load module … : compile exceeded --module-compile-timeout 5m0s`**: a cold load (fetch and compile) ran past the bound. The compile finishes on its own and stores its artifact, so the next request for that digest is served; a module that hits the bound every time is too large to compile under it, so raise the bound or lower `--max-module-size`.
- **The first request is slow, later ones are fast**: expected, because the compile happens once per module digest per process. A volume under `/tmp/function-wasm-cache` spares the next process the fetch, not the compile, which `--warm-modules` moves ahead of readiness. See the cache section of [Runtime flags](runtime-flags.md).
- **`Cannot warm module`** at startup: a `--warm-modules` entry didn't load, and the log line gives the entry and the reason. The pod serves anyway and loads that module on its first request.
- **The readiness probe fails for a while after start**: the pod is warming `--warm-modules`. See [Readiness](readiness.md).
