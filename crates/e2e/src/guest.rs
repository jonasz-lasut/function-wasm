//! The committed test module and its variants: one `fn.wasm` (guest/, built
//! by `make e2e.guest`) pushed under several module manifests, so one binary
//! covers admitted and refused cases alike. A variant's manifest content
//! differs from every other's (its version carries the variant's name): the
//! runtime verifies a signature once per manifest digest per process, so two
//! variants sharing a digest would share a verdict.

use anyhow::{Context as _, Result};
use guestfn_runtime::cosign::Signer;
use guestfn_runtime::cosign::testutil::{COSIGN_SIGN_PREDICATE, TestKey};
use guestfn_runtime::manifest::Manifest;
use guestfn_runtime::testsigstore::Instance;

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
pub const TRUSTED_ISSUER: &str = "https://e2e.guestfn.io";
pub const TRUSTED_IDENTITY: &str = "https://e2e.guestfn.io/release";
pub const UNTRUSTED_IDENTITY: &str = "https://e2e.guestfn.io/intruder";

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
    /// The manifest file under guest/manifests/<runtime> the variant is built from.
    manifest: &'static str,
    /// A version suffix distinguishing variants that share a manifest file.
    version_suffix: Option<&'static str>,
    pub signature: Signature,
}

pub const VARIANTS: &[Variant] = &[
    Variant {
        name: "plain",
        manifest: include_str!("../guest/manifests/wasm/plain.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "egress",
        manifest: include_str!("../guest/manifests/wasm/egress.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "env",
        manifest: include_str!("../guest/manifests/wasm/env.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "credentials",
        manifest: include_str!("../guest/manifests/wasm/credentials.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "tmp",
        manifest: include_str!("../guest/manifests/wasm/tmp.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "everything",
        manifest: include_str!("../guest/manifests/wasm/everything.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "unsigned",
        manifest: include_str!("../guest/manifests/wasm/plain.yaml"),
        version_suffix: Some("unsigned"),
        signature: Signature::None,
    },
    Variant {
        name: "keyless-trusted",
        manifest: include_str!("../guest/manifests/wasm/plain.yaml"),
        version_suffix: Some("keyless-trusted"),
        signature: Signature::KeylessTrusted,
    },
    Variant {
        name: "keyless-untrusted",
        manifest: include_str!("../guest/manifests/wasm/plain.yaml"),
        version_suffix: Some("keyless-untrusted"),
        signature: Signature::KeylessUntrusted,
    },
];

/// The oci profile's variants (docs/one-pager-oci-runtime.md): the same
/// declarations around the test module as a process, each the one image
/// setup pushed with that manifest appended as a layer, so each has a
/// digest of its own. Their repositories live under REPOSITORY too, so the
/// default policy's rules cover them.
pub const OCI_VARIANTS: &[Variant] = &[
    Variant {
        name: "oci-plain",
        manifest: include_str!("../guest/manifests/oci/plain.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "oci-env",
        manifest: include_str!("../guest/manifests/oci/env.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "oci-credentials",
        manifest: include_str!("../guest/manifests/oci/credentials.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "oci-tmp",
        manifest: include_str!("../guest/manifests/oci/tmp.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    // Refused by name: function-oci serves no egress yet.
    Variant {
        name: "oci-egress",
        manifest: include_str!("../guest/manifests/oci/egress.yaml"),
        version_suffix: None,
        signature: Signature::Key,
    },
    Variant {
        name: "oci-unsigned",
        manifest: include_str!("../guest/manifests/oci/plain.yaml"),
        version_suffix: Some("unsigned"),
        signature: Signature::None,
    },
    // Refused at load: built on the image that sets no user, signed so
    // that the user is what the runtime refuses it for.
    Variant {
        name: OCI_ROOT_VARIANT,
        manifest: include_str!("../guest/manifests/oci/plain.yaml"),
        version_suffix: Some("root"),
        signature: Signature::Key,
    },
];

/// The user the test image runs as, and the variant built on the same
/// image with no user at all, which is root.
pub const OCI_USER: &str = "65532:65532";
pub const OCI_ROOT_VARIANT: &str = "oci-root";
pub const OCI_ROOT_IMAGE_REPOSITORY: &str = "e2e/oci-image-root";

/// The test image's repository and the process's path inside it.
pub const OCI_IMAGE_REPOSITORY: &str = "e2e/oci-image";
pub const OCI_PROCESS_PATH: &str = "e2e-guest";

impl Variant {
    /// The manifest layer: the YAML validated the way guestfn build does,
    /// encoded the way guestfn push publishes it; an oci variant's `oci.image`
    /// is the image it was appended to, as guestfn push --image writes it.
    pub fn manifest_json(&self) -> Result<Vec<u8>> {
        self.manifest_json_for(None)
    }

    pub fn manifest_json_for(&self, image: Option<&str>) -> Result<Vec<u8>> {
        let mut value: serde_json::Value = serde_saphyr::from_str(self.manifest)
            .with_context(|| format!("{}: manifest yaml", self.name))?;
        if let Some(suffix) = self.version_suffix {
            let version = value["version"].as_str().unwrap_or("0.1.0").to_string();
            value["version"] = serde_json::Value::String(format!("{version}-{suffix}"));
        }
        if let Some(image) = image {
            value["oci"]["image"] = serde_json::Value::String(image.to_string());
        }
        let manifest = Manifest::parse(&serde_json::to_vec(&value)?)
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.name))?;
        manifest
            .json()
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.name))
    }

    /// The user of the image an oci variant is built on.
    pub fn image_user(&self) -> &'static str {
        if self.name == OCI_ROOT_VARIANT {
            ""
        } else {
            OCI_USER
        }
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
