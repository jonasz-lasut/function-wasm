# Signatures

To restrict a Function to modules your organisation signed, sign them with
cosign 3, `cosign sign --key cosign.key <ref>`, and start the runtime with
`--cosign-key cosign.pub` (a `DeploymentRuntimeConfig` mounts the key and
sets the flag or `COSIGN_KEY`). Every OCI module is then verified once per
manifest digest per process before it's run - before it consults any
cache, so an artifact left on a persisted volume by a runtime without
the key isn't served by one with it - and unsigned sources are refused.

cosign 3 stores a signature as a Sigstore bundle: a DSSE envelope around an
in-toto statement whose subject is the module's manifest digest, pushed as
an OCI 1.1 referrer of that manifest. The runtime lists the referrers
through the registry's referrers API or, where the registry has none (GHCR,
`registry:3`), through the `sha256-<hex>` tag index cosign maintains
instead, with the same credentials as the pull. A module is signed when
one bundle's signature verifies with a configured key and its statement is
a cosign signature (predicate type `https://sigstore.dev/cosign/sign/v1`)
of that digest. Other attestations among the referrers, such as SLSA
provenance from `actions/attest`, aren't signatures and never admit a
module, even when signed with a configured key. For a key-based bundle
the keys are the trust root: its key hint and transparency-log entries
aren't consulted.

cosign 2's legacy signatures (the `sha256-<hex>.sig` tag, which cosign 3
still writes with `--new-bundle-format=false`) aren't read: a module
signed only that way counts as unsigned and is refused, so re-sign it with cosign 3.

## Keyless signatures

A keyless signature (`cosign sign <ref>` under an OIDC identity, which is
how this repository's release workflow signs the examples it publishes)
carries a Fulcio certificate valid for ten minutes, a Rekor
transparency-log entry and an RFC 3161 timestamp instead of a key hint.
The runtime verifies it with `--cosign-trusted-root trusted_root.json`
(`COSIGN_TRUSTED_ROOT`), a Sigstore trusted root - the Fulcio authorities,
the Rekor and certificate-transparency keys and the timestamp authorities,
each with its validity window. `cosign initialize` fetches the public-good
instance's over TUF:

```shell
cosign initialize
cp ~/.sigstore/root/tuf-repo-cdn.sigstore.dev/targets/trusted_root.json .
```

Verification is offline and rests on the signing time, not the clock.
Every RFC 3161 timestamp in the bundle must verify with a timestamp
authority of the root, and the Rekor entry's signed entry timestamp with a
Rekor key. The certificate must chain to a Fulcio authority and be valid
at each of those times. The runtime then verifies the entry's inclusion proof and signed
checkpoint, and then the signature itself. A bundle with no verifiable
signing time is refused, so a signing key taken from a CI run after its
certificate's ten minutes signs nothing the runtime accepts.

A trusted root alone admits nothing. The signer the certificate names (its
OIDC issuer and its subject alternative name: a workflow URI, or an email)
must be trusted for the module's repository by a `trustSigner` rule in the
operator policy, default-deny, and the repository still needs its
`requireSignature` rule:

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

With that policy and the trusted root, the examples this repository
publishes run on the identity of its release workflow, with no
countersignature. A keyless signature by any other signer is refused with
`keyless signature by <identity> (issuer <issuer>) is not trusted for
<repository>: no trustSigner rule in the operator policy permits it`, and
a keyless bundle met by a runtime without the flag with `keyless signature
cannot be verified: the runtime has neither --cosign-trusted-root nor
--cosign-trust-instance`.
`--cosign-key` and a trusted root combine. The runtime checks a bundle
with a key hint against the keys and one with a certificate against the
root, and a module is signed when one of its bundles verifies.

### Fetching the trusted root over TUF

Sigstore rotates the material in the trusted root (the public-good root
already contains two generations of Rekor keys, Fulcio chains and CT keys),
and a runtime holding a stale file keeps admitting old signatures while
refusing every module signed after the next rotation. `--cosign-trusted-root`
is the air-gapped mode. The runtime reads that file and nothing else,
without network access, and the operator refreshes it (a mounted ConfigMap
that `cosign initialize` rewrites). `--cosign-trust-instance public-good`
(`COSIGN_TRUST_INSTANCE`; also `staging` and `github`) fetches the root
instead and refreshes it while serving. The flags exclude each other.
The TUF root embedded in the runtime's sigstore-trust-root crate
bootstraps the walk: root, timestamp, snapshot and targets, each signed
and expiring, and the runtime reads `trusted_root.json` by the length and hashes the
verified metadata pins. It caches the verified metadata and the root
under `/tmp/function-wasm-cache/tuf/<instance>` (outside the
`--max-cache-size` sweep). A volume under the cache root preserves it
across restarts.

At startup the runtime fetches the root before it warms modules and
reports ready, bounded to one minute; if the repository doesn't answer,
the cache is re-verified offline from the embedded TUF root (as
trustworthy as a fetch, only as old as the cache, and refused once the
metadata has expired). With neither the runtime still starts and serves:
every keyless signature is refused with `keyless signature cannot be
verified: the trusted root of public-good has not been fetched yet (the
last attempt failed: …)` until a refresh succeeds, retried once a minute
until then. Key-signed and unsigned modules are unaffected, and so are
readiness and liveness. While serving, the runtime refreshes the root every
`--cosign-tuf-refresh` (`COSIGN_TUF_REFRESH`, default `1h`, at least
`1m`, the duration syntax of `--module-timeout`). A changed root replaces
the one in use and forgets the verified-digest memo, so a module whose
signer the new root no longer vouches for is verified again on its next
request. When a refresh fails, the runtime keeps the last good root.
Verification itself never touches the network: a refresh failure can only
make the root older and never slows a request. The metrics
`function_wasm_trusted_root_refreshes_total{outcome}` and
`function_wasm_trusted_root_last_refresh_timestamp_seconds` show the
refreshes and the root's age (see [metrics](metrics.md)).

`function validate --resolve --cosign-trust-instance public-good …` runs
the same fetch (or reads the same cache) before it verifies, so an
operator can rehearse the flags offline; without `--resolve` nothing is
fetched. A private TUF mirror isn't offered yet.

## Requiring signatures per repository

`--cosign-key` on its own is all-or-nothing: with it, every module must be
signed. An operator grant policy (`--sandbox-policy-file`) can instead require a
signature **per repository** through a `requireSignature` rule over the same
boundary-correct `Repository` hierarchy the module fence uses, so only the
repositories you name must be signed while others run unsigned:

```cedar
permit (principal, action == Action::"requireSignature", resource)
when { resource in Repository::"ghcr.io/acme/prod" };
```

The crypto is unchanged: `--cosign-key` still provides the keys and performs
the check, and Cedar only decides *whether* a repository must be signed. The
refusal (a required module that's unsigned, or one the runtime has neither
flag to verify with) happens before any cache, exactly as the
all-or-nothing check does. Precedence: **without** `--sandbox-policy-file`, `--cosign-key`
keeps its all-or-nothing meaning unchanged; **with** a `--sandbox-policy-file`, the
per-repository `requireSignature` decision selects which modules must be signed
(a repository without a rule needn't be signed), and `--cosign-key` supplies the
keys. A signature no key can check is refused, so the rule is
fail-closed.
