//! The committed test module and its variants: one `fn.wasm` (guest/, built
//! by `make e2e.guest`) pushed under several module manifests, so one binary
//! covers admitted and refused cases alike. A variant's manifest content
//! differs from every other's (its version carries the variant's name): the
//! runtime verifies a signature once per manifest digest per process, so two
//! variants sharing a digest would share a verdict.

use anyhow::{Context as _, Result};
use function_wasm::cosign::Signer;
use function_wasm::cosign::testutil::{COSIGN_SIGN_PREDICATE, TestKey};
use function_wasm::manifest::Manifest;
use function_wasm::testsigstore::Instance;

pub const WASM: &[u8] = include_bytes!("../guest/fn.wasm");

/// The repository every variant lives under in the suite's registry; the
/// compositionPolicy fences `module.from` to it.
pub const REPOSITORY: &str = "e2e";
/// A repository outside that fence, holding one copy of the plain variant.
pub const OUTSIDE_REPOSITORY: &str = "elsewhere";
pub const TAG: &str = "v0.1.0";

/// The seed of the key the default profile's --cosign-key verifies.
pub const KEY_SEED: u8 = 7;
/// The identities the keyless variants are signed under; the default
/// profile's policy trusts the first for the suite's repository.
pub const TRUSTED_ISSUER: &str = "https://e2e.function-wasm.io";
pub const TRUSTED_IDENTITY: &str = "https://e2e.function-wasm.io/release";
pub const UNTRUSTED_IDENTITY: &str = "https://e2e.function-wasm.io/intruder";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
    /// A cosign 3 key-based bundle by the suite's key.
    Key,
    /// No referrer at all.
    None,
    /// A keyless bundle from the in-memory Sigstore instance by the trusted
    /// identity.
    KeylessTrusted,
    /// The same by an identity no trustSigner rule names.
    KeylessUntrusted,
}

pub struct Variant {
    pub name: &'static str,
    /// The manifest file under guest/manifests the variant is built from.
    manifest: &'static str,
    /// A version suffix distinguishing variants that share a manifest file.
    version_suffix: Option<&'static str>,
    pub signature: Signature,
}

pub const VARIANTS: &[Variant] = &[
    Variant {
        name: "plain",
        manifest: include_str!("../guest/manifests/plain.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "egress",
        manifest: include_str!("../guest/manifests/egress.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "env",
        manifest: include_str!("../guest/manifests/env.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "credentials",
        manifest: include_str!("../guest/manifests/credentials.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "tmp",
        manifest: include_str!("../guest/manifests/tmp.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "everything",
        manifest: include_str!("../guest/manifests/everything.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "unsigned",
        manifest: include_str!("../guest/manifests/plain.yaml"),
        version_suffix: Some("unsigned"),
        signature: Signature::None,
    },
    Variant {
        name: "keyless-trusted",
        manifest: include_str!("../guest/manifests/plain.yaml"),
        version_suffix: Some("keyless-trusted"),
        signature: Signature::KeylessTrusted,
    },
    Variant {
        name: "keyless-untrusted",
        manifest: include_str!("../guest/manifests/plain.yaml"),
        version_suffix: Some("keyless-untrusted"),
        signature: Signature::KeylessUntrusted,
    },
];

impl Variant {
    /// The manifest layer: the YAML validated the way guestfn build does,
    /// encoded the way guestfn push publishes it.
    pub fn manifest_json(&self) -> Result<Vec<u8>> {
        let mut value: serde_json::Value = serde_yaml::from_str(self.manifest)
            .with_context(|| format!("{}: manifest yaml", self.name))?;
        if let Some(suffix) = self.version_suffix {
            let version = value["version"].as_str().unwrap_or("0.1.0").to_string();
            value["version"] = serde_json::Value::String(format!("{version}-{suffix}"));
        }
        let manifest = Manifest::parse(&serde_json::to_vec(&value)?)
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.name))?;
        manifest
            .json()
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.name))
    }

    pub fn repository(&self) -> String {
        format!("{REPOSITORY}/{}", self.name)
    }
}

pub fn key() -> TestKey {
    TestKey::from_seed(KEY_SEED)
}

pub fn sigstore() -> Instance {
    Instance::new()
}

pub fn trusted_signer() -> Signer {
    Signer {
        issuer: TRUSTED_ISSUER.to_string(),
        identity: TRUSTED_IDENTITY.to_string(),
    }
}

pub fn untrusted_signer() -> Signer {
    Signer {
        issuer: TRUSTED_ISSUER.to_string(),
        identity: UNTRUSTED_IDENTITY.to_string(),
    }
}

/// The bundle a variant's signature is, for the manifest digest given.
pub fn bundle(variant: &Variant, digest: &str) -> Option<Vec<u8>> {
    match variant.signature {
        Signature::Key => Some(key().bundle(digest, COSIGN_SIGN_PREDICATE)),
        Signature::None => None,
        Signature::KeylessTrusted => {
            Some(sigstore().sign(digest, COSIGN_SIGN_PREDICATE, &trusted_signer()))
        }
        Signature::KeylessUntrusted => {
            Some(sigstore().sign(digest, COSIGN_SIGN_PREDICATE, &untrusted_signer()))
        }
    }
}

/// The default profile's trusted root: the in-memory instance's authorities
/// beside the public-good instance's, one file (a trusted root is a list
/// per authority kind).
pub fn merged_trusted_root() -> Result<Vec<u8>> {
    let mut merged: serde_json::Value = serde_json::from_str(
        sigstore_trust_root::SigstoreInstance::PublicGood.embedded_trusted_root_json(),
    )
    .context("public-good trusted root")?;
    let test: serde_json::Value =
        serde_json::from_slice(&sigstore().trusted_root_json()).context("test trusted root")?;
    for list in [
        "tlogs",
        "certificateAuthorities",
        "ctlogs",
        "timestampAuthorities",
    ] {
        if let Some(extra) = test[list].as_array() {
            let target = merged[list].as_array_mut();
            match target {
                Some(target) => target.extend(extra.iter().cloned()),
                None => merged[list] = serde_json::Value::Array(extra.clone()),
            }
        }
    }
    Ok(serde_json::to_vec_pretty(&merged)?)
}
