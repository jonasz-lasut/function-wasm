# Signatures

To restrict a Function to modules your organisation signed, sign them with
cosign 3, `cosign sign --key cosign.key <ref>`, and start the runtime with
`--cosign-key cosign.pub` (a `DeploymentRuntimeConfig` mounts the key and
sets the flag or `COSIGN_KEY`). Every OCI module is then verified once per
manifest digest per process before it is run - before any cache is
consulted, so an artifact left on a persisted volume by a runtime without
the key is not served by one with it - and unsigned sources are refused.

cosign 3 stores a signature as a Sigstore bundle: a DSSE envelope around an
in-toto statement that names the module's manifest digest, pushed as an OCI
1.1 referrer of that manifest. The runtime lists the referrers through the
registry's referrers API or, where the registry has none (GHCR,
`registry:3`), through the `sha256-<hex>` tag index cosign maintains
instead, with the same credentials as the pull. A module is signed when
one bundle's signature verifies with a configured key and its statement is
a cosign signature (predicate type `https://sigstore.dev/cosign/sign/v1`)
of that digest. Other attestations among the referrers, such as SLSA
provenance from `actions/attest`, are not signatures and never admit a
module, even when signed with a configured key. For a key-based bundle
the keys are the trust root: its key hint and transparency-log entries are
not consulted.

cosign 2's legacy signatures (the `sha256-<hex>.sig` tag, which cosign 3
still writes with `--new-bundle-format=false`) are not read: a module
signed only that way is refused as unsigned, so re-sign it with cosign 3.

## Keyless signatures

A keyless signature (`cosign sign <ref>` under an OIDC identity, which is
how this repository's release workflow signs the examples it publishes)
carries a Fulcio certificate valid for ten minutes, a Rekor
transparency-log entry and an RFC 3161 timestamp instead of a key hint.
The runtime verifies it with `--cosign-trusted-root trusted_root.json`
(`COSIGN_TRUSTED_ROOT`), a Sigstore trusted root: the Fulcio authorities,
the Rekor and certificate-transparency keys and the timestamp authorities,
each with its validity window. `cosign initialize` fetches the public-good
instance's over TUF:

```shell
cosign initialize
cp ~/.sigstore/root/tuf-repo-cdn.sigstore.dev/targets/trusted_root.json .
```

Verification is offline and rests on the signing time, not the clock:
every RFC 3161 timestamp in the bundle must verify with a timestamp
authority of the root and the Rekor entry's signed entry timestamp with a
Rekor key, the certificate must chain to a Fulcio authority and be valid
at each of those times, the entry's inclusion proof and signed checkpoint
are verified, and then the signature itself. A bundle with no verifiable
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
cannot be verified: the runtime has no --cosign-trusted-root`.
`--cosign-key` and `--cosign-trusted-root` combine: a bundle with a key
hint is checked against the keys, one with a certificate against the root,
and a module is signed when any one bundle verifies. The trusted root is
read once at startup; fetching and refreshing it over TUF, for an operator who
would rather not mount the file, is [#183](https://github.com/jonasz-lasut/function-wasm/issues/183).

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
the check; Cedar only decides *whether* a given repository must be signed, and
the refusal (a required module that is unsigned, or that the runtime has
neither flag to verify it with) happens before any cache, exactly as the
all-or-nothing check does. Precedence: **without** `--sandbox-policy-file`, `--cosign-key`
keeps its all-or-nothing meaning unchanged; **with** a `--sandbox-policy-file`, the
per-repository `requireSignature` decision governs which modules must be signed
(a repository no rule names is not required), and `--cosign-key` supplies the
keys. A signature no key can check is refused, so the requirement is
fail-closed.
