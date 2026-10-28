# Signatures

This page is for operators who want the runtime to refuse modules that aren't
signed by a signer they trust. It covers key-based and keyless verification,
the Sigstore trusted root and per-repository requirements. It assumes
cosign 3 and OCI modules; how to sign in CI is out of scope beyond the
cosign commands shown.

Without a `--sandbox-policy-file`, `--cosign-key` makes every module need a
signature. With one, its `requireSignature` rules select the repositories
that must be signed and the rest run unsigned. `--cosign-key` then only
supplies keys (see [per repository](#requiring-signatures-per-repository)).
Only OCI artifacts have a cosign signature, so a source of another type is
refused wherever a signature is required. With `--cosign-key` alone, that's
every `Path` and `HTTP` source. Under a policy it's an `HTTP` source whose URL a
`requireSignature` rule matches. A `Path` source has no repository, so no
rule matches it and it runs unsigned.

## Key-based signatures

Sign a module with `cosign sign --key cosign.key <ref>` and start the runtime
with `--cosign-key cosign.pub`. A `DeploymentRuntimeConfig` mounts the key and
sets the flag or `COSIGN_KEY`.

The runtime verifies a module once per manifest digest per process, before it
consults any cache. An artifact left on a persisted volume by a runtime
without the key isn't served by one with it.

cosign 3 stores a signature as a Sigstore bundle: a DSSE envelope around an
in-toto statement whose subject is the module's manifest digest, pushed as an
OCI 1.1 referrer of that manifest. The runtime lists the referrers through
the registry's referrers API. Where the registry has none (GHCR,
`registry:3`), it reads the `sha256-<hex>` tag index cosign maintains, with
the same credentials as the pull.

A module is signed when one bundle verifies with a configured key and its
statement is a cosign signature of that digest (predicate type
`https://sigstore.dev/cosign/sign/v1`). Other attestations among the
referrers, such as SLSA provenance, never admit a module. For a key-based
bundle the keys are the trust root, so its key hint and transparency-log
entries aren't consulted.

cosign 2's legacy signatures (the `sha256-<hex>.sig` tag, which cosign 3
writes with `--new-bundle-format=false`) aren't read. A module signed only
that way counts as unsigned and is refused: re-sign it with cosign 3.

## Keyless signatures

A keyless signature is made with `cosign sign <ref>` under an OIDC identity.
This repository's release workflow signs the examples it publishes that way.
The bundle has a Fulcio certificate valid for ten minutes, a Rekor
transparency-log entry and an RFC 3161 timestamp instead of a key hint.

### The trusted root

The runtime verifies a keyless bundle against a Sigstore trusted root, which
lists the Fulcio authorities, the Rekor and certificate-transparency keys and
the timestamp authorities, each with a validity window. Pass it as
`--cosign-trusted-root trusted_root.json` (`COSIGN_TRUSTED_ROOT`).
`cosign initialize` fetches the public-good instance's root over TUF:

```shell
cosign initialize
cp ~/.sigstore/root/tuf-repo-cdn.sigstore.dev/targets/trusted_root.json .
```

Verification is offline and rests on the signing time, not the clock. Every
RFC 3161 timestamp in the bundle must verify with a timestamp authority of the
root, and the Rekor entry's signed entry timestamp with a Rekor key. The
certificate must chain to a Fulcio authority and be valid at each of those
times. The runtime then checks the entry's inclusion proof and signed
checkpoint, and last the signature. A bundle with no verifiable signing time
is refused, and so is a bundle whose certificate had expired at signing time.

### Trusting a signer

A trusted root alone admits nothing. The signer the certificate names (its
OIDC issuer and its subject alternative name, a workflow URI or an email)
must be trusted for the module's repository by a `trustSigner` rule in the
operator policy, default-deny. The repository also needs its
`requireSignature` rule (see [per repository](#requiring-signatures-per-repository)):

```cedar
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples" };

permit (principal, action == Action::"trustSigner", resource)
when {
  resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples" &&
  context.issuer == "https://token.actions.githubusercontent.com" &&
  context.identity like "https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-pkg.yml@refs/tags/v*"
};
```

With that policy and the trusted root, the examples this repository publishes
run on the identity of its release workflow, with no countersignature.

A keyless signature by any other signer is refused:

```text
keyless signature by <identity> (issuer <issuer>) is not trusted for <repository>: no trustSigner rule in the operator policy permits it
```

A keyless bundle met by a runtime with neither flag is refused:

```text
keyless signature cannot be verified: the runtime has neither --cosign-trusted-root nor --cosign-trust-instance
```

`--cosign-key` and a trusted root combine. The runtime checks a bundle with a
key hint against the keys and one with a certificate against the root, and a
module is signed when one of its bundles verifies.

### Fetching the trusted root over TUF

The trusted root lists each key's and authority's validity window, and a
bundle verifies against the material valid at its signing time. A file that
predates a rotation can't verify bundles signed with the new material, so the
root has to be refreshed. `--cosign-trusted-root` is the air-gapped mode. The runtime reads
the file and nothing else, so you refresh it yourself, such as through a
mounted ConfigMap that `cosign initialize` rewrites.

`--cosign-trust-instance public-good` (`COSIGN_TRUST_INSTANCE`; also
`staging` and `github`) fetches the root instead and refreshes it while
serving. You can't set both flags. The refresh interval,
`--cosign-tuf-refresh`, is described in [runtime flags](runtime-flags.md).

The runtime verifies the metadata chain starting from the TUF root built into
the runtime. It caches the verified
metadata and the root under `/tmp/function-wasm-cache/tuf/<instance>`, outside
the `--max-cache-size` sweep. A volume under the cache root preserves it
across restarts.

At startup the runtime fetches the root before it warms modules and reports
ready, giving up after one minute. It then falls back to the cache,
re-verified offline from the built-in root, until the cached metadata expires.
With neither available, the runtime still starts and refuses every keyless
signature until a refresh succeeds, retried once a minute:

```text
keyless signature cannot be verified: the trusted root of public-good has not been fetched yet (the last attempt failed: …)
```

Key-signed and unsigned modules, readiness and liveness are unaffected.

While serving, a changed root replaces the one in use and clears the
verified-digest memo, so a module whose signer the new root no longer vouches
for is verified again on its next request. A failed refresh keeps the last
good root. Verification never touches the network: a failed refresh makes the
root older and never slows a request. The metrics
`function_wasm_trusted_root_refreshes_total{outcome}` and
`function_wasm_trusted_root_last_refresh_timestamp_seconds` show the refreshes
and the root's age (see [metrics](metrics.md)).

`function validate --resolve --cosign-trust-instance public-good …` runs the
same fetch, or reads the same cache, before it verifies, so you can rehearse
the flags offline. Without `--resolve` nothing is fetched. A private TUF
mirror isn't supported.

## Requiring signatures per repository

An operator policy (`--sandbox-policy-file`) can require a signature per
repository through a `requireSignature` rule over the same `Repository`
hierarchy the module fence uses. Only the repositories you name must be
signed:

```cedar
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/acme/prod" };
```

A repository without a rule needn't be signed. A signature no configured key
can check is refused: the rule fails closed. A required module that's unsigned, or that the runtime has no flag to
verify, is refused before any cache is consulted.

## Startup warnings

The runtime warns at startup when a signing flag and the operator policy
don't match, and serves anyway. Each line says what it leaves unverified:

```text
--cosign-key is set but the operator policy has no requireSignature rule: no module will be signature-verified
--cosign-trusted-root is set but the operator policy has no trustSigner rule: no keyless signature will be trusted
the operator policy has trustSigner rules but neither --cosign-trusted-root nor --cosign-trust-instance is set: every keyless signature will be refused
```

The second line names `--cosign-trust-instance` when that flag is set
instead. `function validate --resolve` adds a warning of its own for a module
that requires egress when it can't verify signatures (see the
[warnings of `function validate`](../guests/validate.md#warnings-and-exit-codes)).
