# cosign fixtures

`pdb-addon-v0.1.0-keyless.bundle.json` is a real keyless cosign signature:
the Sigstore bundle (v0.3) that this repository's release workflow,
`publish-pkg.yml` on tag `v0.6.0`, attached on 2026-10-07 to
`ghcr.io/jonasz-lasut/wasmfn/examples/pdb-addon:v0.1.0` (the manifest digest
in `pdb-addon-v0.1.0.digest`), downloaded from GHCR on 2026-10-26: the
referrer annotated with predicateType `https://sigstore.dev/cosign/sign/v1`,
its one layer. It carries a Fulcio certificate (SAN
`https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-pkg.yml@refs/tags/v0.6.0`,
issuer `https://token.actions.githubusercontent.com`, valid 11:15:35 to
11:25:35 UTC), a Rekor v1 `dsse/0.0.1` entry (log index 3129974832) with its
signed entry timestamp, inclusion proof and checkpoint, and one RFC 3161
timestamp. The certificate has long expired: verifying it proves the
signing time is established from the bundle, not the clock. The tests verify
it offline against the trusted root embedded in sigstore-trust-root.
