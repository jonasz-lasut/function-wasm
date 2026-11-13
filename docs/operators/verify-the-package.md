# Verifying a function package

This page is for operators who want proof of where a function package came from before a cluster runs it. It covers the packages themselves, `function-wasm` and `function-oci`. The signatures of the modules a function runs are a separate matter, on the [Signatures](signatures.md) page.

## The signature

A workflow in this repository signs every release of both packages with cosign, keyless, in both registries: `ghcr.io/jonasz-lasut/<function>` and its mirror `xpkg.upbound.io/jonasz-lasut/<function>`. The signature binds the package's digest to that signing workflow. A package that verifies is one this repository's release process signed.

Verify a package with the signer's identity and its issuer:

```shell
cosign verify ghcr.io/jonasz-lasut/function-wasm:v1.0.2 \
  --certificate-identity https://github.com/jonasz-lasut/guestfn/.github/workflows/supplychain.yml@refs/heads/main \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

The identity is the same for every release of both functions since function-wasm v1.0.0 and function-oci v0.1.0, so one rule covers them all. function-wasm v0.6.0 and earlier date from before the repository took its present name, and their identity has `jonasz-lasut/function-wasm` where this one has `jonasz-lasut/guestfn`. cosign prints the digest it verified. Put that digest in the `Function`'s `spec.package` (`ghcr.io/jonasz-lasut/function-wasm@sha256:...`) when the cluster must run that package and no other.

## In the cluster

Crossplane can make the same check before it installs a package. An `ImageConfig` names the packages and the signer they must have:

```yaml
apiVersion: pkg.crossplane.io/v1beta1
kind: ImageConfig
metadata:
  name: guestfn-functions
spec:
  matchImages:
    - type: Prefix
      prefix: ghcr.io/jonasz-lasut/function-
  verification:
    provider: Cosign
    cosign:
      authorities:
        - name: guestfn-signing-workflow
          keyless:
            identities:
              - issuer: https://token.actions.githubusercontent.com
                subject: https://github.com/jonasz-lasut/guestfn/.github/workflows/supplychain.yml@refs/heads/main
```

Crossplane's own documentation describes `ImageConfig` and the versions that support signature verification.

## Provenance

Each package also has an SBOM attestation and a SLSA provenance attestation, which `gh attestation verify` reads:

```shell
gh attestation verify oci://ghcr.io/jonasz-lasut/function-wasm:v1.0.2 --repo jonasz-lasut/guestfn
```

From function-wasm v1.0.2 and function-oci v0.3.0 the provenance records the run that built the package: the `publish-function.yml` workflow at the release's tag, `refs/tags/function-wasm-vX.Y.Z` or `refs/tags/function-oci-vX.Y.Z`, with that tag's commit. The certificate's identity is the signing workflow on `main` all the same, as for the signature.

Through function-wasm v1.0.1 and function-oci v0.2.0 the provenance records the signing run on `main` instead, with the signing workflow and the commit `main` had when that run started. That commit isn't the one the package's code comes from. For those releases the source is the git tag, `function-wasm-vX.Y.Z` or `function-oci-vX.Y.Z`.
