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
module, even when signed with a configured key. The keys are the trust
root: a bundle's key hint, certificate and transparency-log entries are
not consulted.

cosign 2's legacy signatures (the `sha256-<hex>.sig` tag, which cosign 3
still writes with `--new-bundle-format=false`) are not read: a module
signed only that way is refused as unsigned, so re-sign it with cosign 3.
Keyless (Fulcio/Rekor) signatures are not verified yet
([#116](https://github.com/jonasz-lasut/function-wasm/issues/116)): a
keyless bundle matches no configured key, so it neither admits a module
nor gets in the way of a key-based bundle beside it. Until then, admit a
module signed keyless upstream by verifying it, copying it if you serve
from your own registry, and countersigning it with your key:

```shell
cosign verify --certificate-identity <identity> --certificate-oidc-issuer <issuer> <upstream-ref>
crane copy <upstream-ref> <your-ref>         # optional; keeps the manifest digest
cosign sign --key cosign.key <your-ref>      # pinned by digest, repo@sha256:…
```

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
the refusal (a required module that is unsigned, or that the runtime has no
`--cosign-key` to verify) happens before any cache, exactly as the
all-or-nothing check does. Precedence: **without** `--sandbox-policy-file`, `--cosign-key`
keeps its all-or-nothing meaning unchanged; **with** a `--sandbox-policy-file`, the
per-repository `requireSignature` decision governs which modules must be signed
(a repository no rule names is not required), and `--cosign-key` supplies the
keys. A signature no key can check is refused, so the requirement is
fail-closed.
