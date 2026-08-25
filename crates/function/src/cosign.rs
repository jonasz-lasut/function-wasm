//! Cosign signature verification - the Rust port of `internal/module`'s
//! signature.go, with the crypto from sigstore-rs (key-based only: the
//! "cosign sign --key" workflow; keyless is deliberately unsupported, as in
//! the Go runtime). The signature artifact is fetched through the runtime's
//! own registry client, so the .sig manifest travels the same authenticated
//! path as the module pull; sigstore-rs contributes the verification keys
//! (algorithm auto-detected from the PEM's SPKI) and the signature check.

use std::collections::HashSet;
use std::sync::Mutex;

use sigstore::crypto::{CosignVerificationKey, Signature};

use crate::location::OciReference;
use crate::oci::RegistryClient;

/// cosign stores an image's signatures as an artifact tagged after the
/// manifest digest; each layer is a simple-signing payload with the base64
/// signature in an annotation.
const SIGNATURE_ANNOTATION: &str = "dev.cosignproject.cosign/signature";
const PAYLOAD_TYPE: &str = "cosign container image signature";

/// Bounds a signature payload read from a registry.
const MAX_SIGNATURE_PAYLOAD: u64 = 1 << 20;

/// Checks cosign signatures made with a fixed set of public keys. Results
/// are remembered per manifest digest for the life of the process;
/// manifests are immutable by digest.
pub struct Verifier {
    keys: Vec<CosignVerificationKey>,
    verified: Mutex<HashSet<String>>,
}

impl Verifier {
    /// Reads one or more PEM public keys (as cosign.pub is written).
    pub fn load(path: &std::path::Path) -> Result<Verifier, String> {
        let raw = std::fs::read(path).map_err(|e| {
            format!(
                "cannot read cosign public key: {}",
                crate::resolver::go_io_error("open", path, &e)
            )
        })?;
        Self::new(&raw)
    }

    /// Parses PEM public keys; the algorithm (ECDSA, RSA, ed25519) is read
    /// from each key's SPKI.
    pub fn new(pem_keys: &[u8]) -> Result<Verifier, String> {
        let blocks = pem::parse_many(pem_keys)
            .map_err(|e| format!("cannot parse cosign public key: {e}"))?;
        let mut keys = Vec::new();
        for block in blocks {
            let key = CosignVerificationKey::try_from_der(block.contents())
                .map_err(|e| format!("cannot parse cosign public key: {e}"))?;
            keys.push(key);
        }
        if keys.is_empty() {
            return Err("no PEM public key found for cosign verification".to_string());
        }
        Ok(Verifier {
            keys,
            verified: Mutex::new(HashSet::new()),
        })
    }

    /// Checks that the pinned manifest carries a signature by one of the
    /// keys, fetching the signature artifact through client - the same
    /// authenticated path the module pull uses. Blocking: run it off the
    /// async executor.
    pub fn verify(&self, client: &RegistryClient, reference: &OciReference) -> Result<(), String> {
        if self
            .verified
            .lock()
            .expect("poisoned")
            .contains(&reference.digest)
        {
            return Ok(());
        }
        let tag = signature_tag(&reference.digest);
        let Some(manifest) = client.manifest_by_tag(&tag)? else {
            return Err(format!(
                "{}/{}@{} carries no cosign signature ({tag} not found)",
                reference.registry, reference.repository, reference.digest
            ));
        };

        let mut reasons = Vec::new();
        for layer in &manifest.layers {
            let Some(encoded) = layer.annotations.get(SIGNATURE_ANNOTATION) else {
                continue;
            };
            let payload = client.blob(&layer.digest, MAX_SIGNATURE_PAYLOAD, "cosign payload")?;
            if let Err(reason) = check_payload(&payload, &reference.digest) {
                reasons.push(reason);
                continue;
            }
            let verifies = self.keys.iter().any(|key| {
                key.verify_signature(Signature::Base64Encoded(encoded.as_bytes()), &payload)
                    .is_ok()
            });
            if verifies {
                self.verified
                    .lock()
                    .expect("poisoned")
                    .insert(reference.digest.clone());
                return Ok(());
            }
            reasons.push("signature does not verify with the configured keys".to_string());
        }
        if reasons.is_empty() {
            return Err(format!("{tag} has no cosign signature layers"));
        }
        Err(format!(
            "no valid cosign signature for {}/{}@{}: {}",
            reference.registry,
            reference.repository,
            reference.digest,
            reasons.join("; ")
        ))
    }
}

/// The tag cosign uses for the signatures of a manifest.
pub fn signature_tag(manifest_digest: &str) -> String {
    format!("{}.sig", manifest_digest.replacen(':', "-", 1))
}

/// The part of cosign's simple-signing payload that binds a signature to a
/// manifest digest.
fn check_payload(payload: &[u8], digest: &str) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct SimpleSigning {
        critical: Critical,
    }
    #[derive(serde::Deserialize)]
    struct Critical {
        image: Image,
        #[serde(default, rename = "type")]
        payload_type: String,
    }
    #[derive(serde::Deserialize)]
    struct Image {
        #[serde(default, rename = "docker-manifest-digest")]
        docker_manifest_digest: String,
    }
    let ss: SimpleSigning = serde_json::from_slice(payload)
        .map_err(|e| format!("payload is not simple-signing JSON: {e}"))?;
    if ss.critical.payload_type != PAYLOAD_TYPE {
        return Err(format!("payload type is {:?}", ss.critical.payload_type));
    }
    if ss.critical.image.docker_manifest_digest != digest {
        return Err(format!(
            "payload signs {}, not {digest}",
            ss.critical.image.docker_manifest_digest
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod testsig {
    //! Signing helpers for tests: a P-256 key pair and a cosign-shaped
    //! signature artifact, compatible with the real cosign CLI's key-based
    //! output (ASN.1 DER ECDSA over SHA-256 of the simple-signing payload).

    use p256::ecdsa::signature::Signer as _;
    use p256::pkcs8::EncodePublicKey as _;

    pub struct TestKey {
        signing: p256::ecdsa::SigningKey,
        pub public_pem: String,
    }

    pub fn generate() -> TestKey {
        // A fixed seed keeps the test deterministic; the key never leaves
        // the test process.
        let signing = p256::ecdsa::SigningKey::from_bytes((&[7u8; 32]).into()).expect("key");
        let public_pem = signing
            .verifying_key()
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .expect("pem");
        TestKey {
            signing,
            public_pem,
        }
    }

    impl TestKey {
        /// The simple-signing payload and its base64 signature for a digest.
        pub fn sign_digest(&self, digest: &str) -> (Vec<u8>, String) {
            let payload = serde_json::to_vec(&serde_json::json!({
                "critical": {
                    "identity": {"docker-reference": ""},
                    "image": {"docker-manifest-digest": digest},
                    "type": super::PAYLOAD_TYPE,
                },
                "optional": null,
            }))
            .expect("payload");
            let signature: p256::ecdsa::DerSignature = self.signing.sign(&payload);
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
            (payload, encoded)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::location::parse_oci_reference;
    use crate::oci::testregistry::{TestRegistry, digest_of, serve};
    use std::collections::HashMap;

    /// A registry holding a module artifact and a cosign signature for it.
    fn signed_registry(sign_for: Option<&str>, annotate: bool) -> (String, String, String) {
        let key = testsig::generate();
        let wasm = b"fake wasm";
        let config = b"{}";
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(config), "size": config.len()},
            "layers": [{"mediaType": "application/wasm", "digest": digest_of(wasm), "size": wasm.len()}],
        }))
        .expect("manifest");
        let artifact_digest = digest_of(&manifest);

        // The signature payload binds either the real digest or (for the
        // tamper case) a different one.
        let signed_digest = sign_for.unwrap_or(&artifact_digest).to_string();
        let (payload, signature) = key.sign_digest(&signed_digest);
        let payload_digest = digest_of(&payload);
        let mut layer = serde_json::json!({
            "mediaType": "application/vnd.dev.cosign.simplesigning.v1+json",
            "digest": payload_digest,
            "size": payload.len(),
        });
        if annotate {
            layer["annotations"] = serde_json::json!({ SIGNATURE_ANNOTATION: signature });
        }
        let sig_manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(config), "size": config.len()},
            "layers": [layer],
        }))
        .expect("sig manifest");

        let mut manifests = HashMap::new();
        manifests.insert(artifact_digest.clone(), manifest);
        manifests.insert(signature_tag(&artifact_digest), sig_manifest);
        let mut blobs = HashMap::new();
        blobs.insert(digest_of(wasm), wasm.to_vec());
        blobs.insert(payload_digest, payload);
        let addr = serve(TestRegistry {
            manifests,
            blobs,
            bearer: false,
        });
        (addr, artifact_digest, key.public_pem)
    }

    fn verify(addr: &str, digest: &str, pem: &str) -> Result<(), String> {
        let reference =
            parse_oci_reference(&format!("{addr}/example/greeter@{digest}")).expect("reference");
        let client = RegistryClient::new(&reference, None);
        Verifier::new(pem.as_bytes())?.verify(&client, &reference)
    }

    #[test]
    fn a_valid_signature_verifies() {
        let (addr, digest, pem) = signed_registry(None, true);
        verify(&addr, &digest, &pem).expect("verified");
    }

    #[test]
    fn a_payload_for_another_digest_is_refused() {
        let other = format!("sha256:{}", "9".repeat(64));
        let (addr, digest, pem) = signed_registry(Some(&other), true);
        let err = verify(&addr, &digest, &pem).expect_err("refused");
        assert!(err.contains("payload signs"), "{err}");
    }

    #[test]
    fn a_wrong_key_is_refused() {
        let (addr, digest, _) = signed_registry(None, true);
        // A different key than the one that signed.
        use p256::pkcs8::EncodePublicKey as _;
        let other = p256::ecdsa::SigningKey::from_bytes((&[9u8; 32]).into()).expect("key");
        let pem = other
            .verifying_key()
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .expect("pem");
        let err = verify(&addr, &digest, &pem).expect_err("refused");
        assert!(
            err.contains("does not verify with the configured keys"),
            "{err}"
        );
    }

    #[test]
    fn an_unsigned_module_is_refused() {
        let key = testsig::generate();
        let wasm = b"fake wasm";
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "layers": [{"mediaType": "application/wasm", "digest": digest_of(wasm), "size": wasm.len()}],
        }))
        .expect("manifest");
        let digest = digest_of(&manifest);
        let mut manifests = HashMap::new();
        manifests.insert(digest.clone(), manifest);
        let addr = serve(TestRegistry {
            manifests,
            blobs: HashMap::new(),
            bearer: false,
        });
        let err = verify(&addr, &digest, &key.public_pem).expect_err("refused");
        assert!(err.contains("carries no cosign signature"), "{err}");
    }

    #[test]
    fn a_signature_layer_without_the_annotation_is_refused() {
        let (addr, digest, pem) = signed_registry(None, false);
        let err = verify(&addr, &digest, &pem).expect_err("refused");
        assert!(err.contains("has no cosign signature layers"), "{err}");
    }
}
