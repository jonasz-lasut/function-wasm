//! Cosign signature verification over cosign 3's Sigstore bundles, with the
//! crypto from sigstore-rs - key-based only: the "cosign sign --key"
//! workflow. Keyless verification is not implemented yet (#116); a keyless
//! bundle simply matches no configured key.
//!
//! cosign 3 stores a signature as an OCI 1.1 referrer of the signed
//! manifest: a manifest whose subject is that manifest and whose layer is a
//! Sigstore bundle (v0.3) holding a DSSE envelope around an in-toto
//! statement that names the manifest digest. The referrers are listed and
//! fetched through the runtime's own registry client, so they travel the
//! same authenticated path as the module pull; sigstore-rs contributes the
//! verification keys (algorithm auto-detected from the PEM's SPKI) and the
//! signature check. The configured keys are the trust root: the bundle's
//! verification material (a key hint, a certificate, transparency-log
//! entries) is never consulted, as the Rekor entry of a legacy signature
//! never was. cosign 2's legacy `sha256-<hex>.sig` signatures are not read.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Mutex;

use serde::Deserialize;
use sigstore::crypto::{CosignVerificationKey, Signature};

use crate::location::OciReference;
use crate::oci::{Descriptor, OCI_MANIFEST_TYPE, RegistryClient};

/// The media type of a Sigstore bundle v0.3: the referrer's layer and the
/// bundle's own mediaType.
pub const BUNDLE_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";
/// The DSSE payload type of an in-toto statement.
pub const IN_TOTO_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
/// The in-toto statement version cosign 3 writes.
pub const IN_TOTO_STATEMENT: &str = "https://in-toto.io/Statement/v1";
/// The predicate type of a cosign image signature. Anything else - SLSA
/// provenance from the same signer, say - is an attestation, not a
/// signature, and never admits a module.
pub const COSIGN_SIGN_PREDICATE: &str = "https://sigstore.dev/cosign/sign/v1";

/// Bounds a Sigstore bundle read from a registry (one with a certificate
/// chain is about 11 KB).
const MAX_BUNDLE_SIZE: u64 = 1 << 20;
/// Bounds the referrers listing and each referrer manifest: the size
/// registries commonly cap a manifest at.
const MAX_MANIFEST_SIZE: usize = 4 << 20;
/// Bounds the referrer manifests examined per module, so a crowded listing
/// costs bounded requests.
const MAX_REFERRERS: usize = 32;

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

    /// Checks that the pinned manifest carries a cosign signature by one of
    /// the keys: some Sigstore bundle among its referrers verifies. The
    /// referrers are read through client - the same authenticated path the
    /// module pull uses. Blocking: run it off the async executor.
    pub fn verify(&self, client: &RegistryClient, reference: &OciReference) -> Result<(), String> {
        if self
            .verified
            .lock()
            .expect("poisoned")
            .contains(&reference.digest)
        {
            return Ok(());
        }
        let referrers = client.referrers(&reference.digest, MAX_MANIFEST_SIZE)?;
        // Only an image manifest can carry a bundle layer. The listing's
        // artifactType does not say which do: cosign 3.0 sets none, so its
        // signatures are listed under the empty config's type.
        let manifests: Vec<&Descriptor> = referrers
            .iter()
            .filter(|d| d.media_type == OCI_MANIFEST_TYPE)
            .collect();

        let mut bundles = 0;
        // A set: the reasons come out sorted and once each, whatever order
        // the registry lists the referrers in.
        let mut reasons = BTreeSet::new();
        for descriptor in manifests.iter().take(MAX_REFERRERS) {
            let referrer = match client.manifest_by_digest(&descriptor.digest, MAX_MANIFEST_SIZE) {
                Ok(referrer) => referrer,
                Err(e) => {
                    reasons.insert(e);
                    continue;
                }
            };
            // The listing is not pinned - under the tag schema it is an
            // index anyone with push access writes - so a referrer counts
            // only if it names this manifest as its subject.
            if referrer
                .subject
                .as_ref()
                .is_none_or(|s| s.digest != reference.digest)
            {
                continue;
            }
            for layer in referrer
                .layers
                .iter()
                .filter(|l| l.media_type == BUNDLE_MEDIA_TYPE)
            {
                bundles += 1;
                match self.check_bundle(client, layer, &reference.digest) {
                    Ok(()) => {
                        self.verified
                            .lock()
                            .expect("poisoned")
                            .insert(reference.digest.clone());
                        return Ok(());
                    }
                    Err(reason) => {
                        reasons.insert(reason);
                    }
                }
            }
        }
        if manifests.len() > MAX_REFERRERS {
            reasons.insert(format!(
                "only the first {MAX_REFERRERS} of its {} referrers were examined",
                manifests.len()
            ));
        }
        let name = format!(
            "{}/{}@{}",
            reference.registry, reference.repository, reference.digest
        );
        if bundles == 0 && reasons.is_empty() {
            return Err(format!(
                "{name} carries no cosign signature (no Sigstore bundle among its referrers)"
            ));
        }
        Err(format!(
            "no valid cosign signature for {name}: {}",
            reasons.into_iter().collect::<Vec<_>>().join("; ")
        ))
    }

    /// Fetches one bundle layer, bounded and verified against its digest,
    /// and checks it signs digest.
    fn check_bundle(
        &self,
        client: &RegistryClient,
        layer: &Descriptor,
        digest: &str,
    ) -> Result<(), String> {
        if !(0..=MAX_BUNDLE_SIZE as i64).contains(&layer.size) {
            return Err(format!(
                "Sigstore bundle {} of {} bytes exceeds the size limit of {MAX_BUNDLE_SIZE} bytes",
                layer.digest, layer.size
            ));
        }
        let raw = client.verified_blob(&layer.digest, MAX_BUNDLE_SIZE, "Sigstore bundle")?;
        self.check_bundle_content(&raw, digest)
    }

    /// Checks a bundle: a DSSE envelope whose signature verifies with one
    /// of the keys, around an in-toto statement that is a cosign signature
    /// of digest.
    fn check_bundle_content(&self, raw: &[u8], digest: &str) -> Result<(), String> {
        let bundle: Bundle = serde_json::from_slice(raw)
            .map_err(|e| format!("Sigstore bundle is not valid JSON: {e}"))?;
        if bundle.media_type != BUNDLE_MEDIA_TYPE {
            return Err(format!(
                "Sigstore bundle media type is {:?}, want {BUNDLE_MEDIA_TYPE}",
                bundle.media_type
            ));
        }
        // A messageSignature bundle signs a blob's digest; cosign signs an
        // image with a DSSE envelope.
        let Some(envelope) = bundle.dsse_envelope else {
            return Err(
                "Sigstore bundle carries no DSSE envelope (a message signature signs a blob, not an image)"
                    .to_string(),
            );
        };
        if envelope.payload_type != IN_TOTO_PAYLOAD_TYPE {
            return Err(format!(
                "DSSE payload type is {:?}, want {IN_TOTO_PAYLOAD_TYPE}",
                envelope.payload_type
            ));
        }
        let payload = decode_base64(&envelope.payload).ok_or("DSSE payload is not base64")?;
        let signed = pae(&envelope.payload_type, &payload);
        let verifies = envelope
            .signatures
            .iter()
            .filter_map(|s| decode_base64(&s.sig))
            .any(|sig| {
                self.keys
                    .iter()
                    .any(|key| key.verify_signature(Signature::Raw(&sig), &signed).is_ok())
            });
        if !verifies {
            return Err("signature does not verify with the configured keys".to_string());
        }
        // Only a payload a key vouched for is read any further.
        check_statement(&payload, digest)
    }
}

/// A Sigstore bundle, as far as a key-based check reads it: the
/// verification material is deliberately not modelled.
#[derive(Deserialize)]
struct Bundle {
    #[serde(default, rename = "mediaType")]
    media_type: String,
    #[serde(default, rename = "dsseEnvelope")]
    dsse_envelope: Option<Envelope>,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    payload: String,
    #[serde(default, rename = "payloadType")]
    payload_type: String,
    #[serde(default)]
    signatures: Vec<EnvelopeSignature>,
}

#[derive(Deserialize)]
struct EnvelopeSignature {
    #[serde(default)]
    sig: String,
}

/// DSSE's pre-authentication encoding - what the envelope's signatures
/// sign: "DSSEv1 <len(type)> <type> <len(payload)> <payload>", lengths in
/// ASCII decimal bytes.
fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "DSSEv1 {} {payload_type} {} ",
        payload_type.len(),
        payload.len()
    )
    .into_bytes();
    out.extend_from_slice(payload);
    out
}

/// Bundles are protobuf's JSON mapping, whose bytes fields read in either
/// base64 alphabet, padded or not.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD_PAD_INDIFFERENT, URL_SAFE_PAD_INDIFFERENT};
    STANDARD_PAD_INDIFFERENT
        .decode(s)
        .or_else(|_| URL_SAFE_PAD_INDIFFERENT.decode(s))
        .ok()
}

/// The part of the signed in-toto statement that makes it a cosign
/// signature of digest.
fn check_statement(payload: &[u8], digest: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Statement {
        #[serde(default, rename = "_type")]
        statement_type: String,
        #[serde(default)]
        subject: Vec<Subject>,
        #[serde(default, rename = "predicateType")]
        predicate_type: String,
    }
    #[derive(Deserialize)]
    struct Subject {
        #[serde(default)]
        digest: BTreeMap<String, String>,
    }
    let statement: Statement = serde_json::from_slice(payload)
        .map_err(|e| format!("signed payload is not an in-toto statement: {e}"))?;
    if statement.statement_type != IN_TOTO_STATEMENT {
        return Err(format!(
            "signed statement type is {:?}, want {IN_TOTO_STATEMENT}",
            statement.statement_type
        ));
    }
    if statement.predicate_type != COSIGN_SIGN_PREDICATE {
        return Err(format!(
            "signed statement's predicate type is {:?}, not {COSIGN_SIGN_PREDICATE}",
            statement.predicate_type
        ));
    }
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    if !statement
        .subject
        .iter()
        .any(|s| s.digest.get("sha256").is_some_and(|d| d == hex))
    {
        return Err("signed statement is for another digest".to_string());
    }
    Ok(())
}

#[cfg(any(test, feature = "testutil"))]
pub mod testutil {
    //! Signing helpers for tests: a P-256 key and the Sigstore bundle
    //! `cosign sign --key` (cosign 3) attaches to an image - a DSSE envelope
    //! (ASN.1 DER ECDSA over SHA-256 of the PAE) around an in-toto
    //! statement - pushed into a test registry as a referrer of the signed
    //! manifest.

    use base64::Engine as _;
    use p256::ecdsa::signature::Signer as _;
    use p256::pkcs8::EncodePublicKey as _;
    use sha2::Digest as _;

    use super::{BUNDLE_MEDIA_TYPE, IN_TOTO_PAYLOAD_TYPE, IN_TOTO_STATEMENT};
    use crate::oci::OCI_MANIFEST_TYPE;
    use crate::oci::testregistry::{TestRegistry, digest_of};

    pub use super::COSIGN_SIGN_PREDICATE;

    pub struct TestKey {
        signing: p256::ecdsa::SigningKey,
        pub public_pem: String,
    }

    impl TestKey {
        /// A key from a fixed seed: deterministic, and it never leaves the
        /// test process.
        pub fn from_seed(seed: u8) -> TestKey {
            let signing = p256::ecdsa::SigningKey::from_bytes((&[seed; 32]).into()).expect("key");
            let public_pem = signing
                .verifying_key()
                .to_public_key_pem(p256::pkcs8::LineEnding::LF)
                .expect("pem");
            TestKey {
                signing,
                public_pem,
            }
        }

        /// An ASN.1 DER ECDSA signature over SHA-256 of msg, as cosign
        /// signs with a P-256 key.
        pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
            let signature: p256::ecdsa::DerSignature = self.signing.sign(msg);
            signature.to_bytes().to_vec()
        }

        /// The bundle cosign 3 writes for a key-based signature: an in-toto
        /// statement naming subject (a sha256 manifest digest) with
        /// predicate_type, signed in a DSSE envelope.
        pub fn bundle(&self, subject: &str, predicate_type: &str) -> Vec<u8> {
            let hex = subject.strip_prefix("sha256:").expect("a sha256 digest");
            let statement = serde_json::to_vec(&serde_json::json!({
                "_type": IN_TOTO_STATEMENT,
                "subject": [{"digest": {"sha256": hex}, "annotations": {}}],
                "predicateType": predicate_type,
                "predicate": {},
            }))
            .expect("statement");
            let signature = self.sign(&super::pae(IN_TOTO_PAYLOAD_TYPE, &statement));
            let public_der = self
                .signing
                .verifying_key()
                .to_public_key_der()
                .expect("der");
            let b64 = &base64::engine::general_purpose::STANDARD;
            serde_json::to_vec(&serde_json::json!({
                "mediaType": BUNDLE_MEDIA_TYPE,
                "verificationMaterial": {
                    "publicKey": {"hint": b64.encode(sha2::Sha256::digest(public_der.as_bytes()))},
                },
                "dsseEnvelope": {
                    "payload": b64.encode(&statement),
                    "payloadType": IN_TOTO_PAYLOAD_TYPE,
                    "signatures": [{"sig": b64.encode(signature)}],
                },
            }))
            .expect("bundle")
        }
    }

    /// How a referrer announces its bundle in a referrers listing.
    #[derive(Clone, Copy)]
    pub enum Shape {
        /// cosign 3.1: the manifest's artifactType is the bundle media type.
        ArtifactType,
        /// cosign 3.0, as found on GHCR: no artifactType, only the
        /// dev.sigstore.bundle.* annotations - a listing shows the empty
        /// config's media type.
        AnnotationsOnly,
    }

    /// Attaches bundle to the manifest subject as cosign 3 does: the bundle
    /// and the empty config as blobs, the referrer manifest pushed - with
    /// the referrers tag index where the registry lacks the referrers API.
    /// Returns the referrer manifest's digest.
    pub fn attach(
        registry: &mut TestRegistry,
        subject: &str,
        bundle: &[u8],
        shape: Shape,
    ) -> String {
        let config = b"{}";
        registry.blobs.insert(digest_of(config), config.to_vec());
        registry.blobs.insert(digest_of(bundle), bundle.to_vec());
        let mut manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_TYPE,
            "config": {
                "mediaType": "application/vnd.oci.empty.v1+json",
                "digest": digest_of(config),
                "size": config.len(),
            },
            "layers": [{
                "mediaType": BUNDLE_MEDIA_TYPE,
                "digest": digest_of(bundle),
                "size": bundle.len(),
            }],
            "annotations": {
                "dev.sigstore.bundle.content": "dsse-envelope",
                "dev.sigstore.bundle.predicateType": predicate_type(bundle),
            },
            "subject": {"mediaType": OCI_MANIFEST_TYPE, "digest": subject, "size": 0},
        });
        if let Shape::ArtifactType = shape {
            manifest["artifactType"] = serde_json::json!(BUNDLE_MEDIA_TYPE);
        }
        let manifest = serde_json::to_vec(&manifest).expect("referrer manifest");
        let digest = digest_of(&manifest);
        registry.push_referrer(manifest);
        digest
    }

    /// The statement's predicate type, as cosign annotates the referrer
    /// with it; empty for a bundle that carries none.
    fn predicate_type(bundle: &[u8]) -> String {
        let bundle: serde_json::Value = serde_json::from_slice(bundle).unwrap_or_default();
        bundle["dsseEnvelope"]["payload"]
            .as_str()
            .and_then(super::decode_base64)
            .and_then(|p| serde_json::from_slice::<serde_json::Value>(&p).ok())
            .and_then(|s| s["predicateType"].as_str().map(str::to_string))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{Shape, TestKey, attach};
    use super::*;
    use crate::location::parse_oci_reference;
    use crate::oci::testregistry::{TestRegistry, digest_of, serve};
    use std::collections::HashMap;

    const SLSA_PROVENANCE: &str = "https://slsa.dev/provenance/v1";

    /// A registry holding one module artifact, and its manifest digest.
    fn module_registry(referrers_api: bool) -> (TestRegistry, String) {
        let wasm = b"fake wasm";
        let config = b"{}";
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_TYPE,
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(config), "size": config.len()},
            "layers": [{"mediaType": "application/wasm", "digest": digest_of(wasm), "size": wasm.len()}],
        }))
        .expect("manifest");
        let digest = digest_of(&manifest);
        let registry = TestRegistry {
            manifests: HashMap::from([(digest.clone(), manifest)]),
            blobs: HashMap::from([
                (digest_of(wasm), wasm.to_vec()),
                (digest_of(config), config.to_vec()),
            ]),
            bearer: false,
            referrers_api,
        };
        (registry, digest)
    }

    /// Serves registry and verifies the module digest with keys; the
    /// registry's address reads <REGISTRY> in a refusal.
    fn verify(registry: TestRegistry, digest: &str, keys: &[&TestKey]) -> Result<(), String> {
        let addr = serve(registry);
        let reference =
            parse_oci_reference(&format!("{addr}/example/greeter@{digest}")).expect("reference");
        let client = RegistryClient::new(&reference, None);
        let pem: String = keys.iter().map(|k| k.public_pem.as_str()).collect();
        Verifier::new(pem.as_bytes())?
            .verify(&client, &reference)
            .map_err(|e| e.replace(&addr, "<REGISTRY>"))
    }

    fn unsigned(digest: &str) -> String {
        format!(
            "<REGISTRY>/example/greeter@{digest} carries no cosign signature (no Sigstore bundle among its referrers)"
        )
    }

    fn invalid(digest: &str, reasons: &str) -> String {
        format!("no valid cosign signature for <REGISTRY>/example/greeter@{digest}: {reasons}")
    }

    #[test]
    fn a_bundle_listed_by_the_referrers_api_verifies() {
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(true);
        attach(
            &mut registry,
            &digest,
            &key.bundle(&digest, COSIGN_SIGN_PREDICATE),
            Shape::ArtifactType,
        );
        assert!(
            !registry
                .manifests
                .contains_key(&crate::oci::referrers_tag(&digest))
        );
        assert_eq!(verify(registry, &digest, &[&key]), Ok(()));
    }

    #[test]
    fn a_bundle_listed_by_the_referrers_tag_verifies() {
        let key = TestKey::from_seed(7);
        let other = TestKey::from_seed(9);
        let (mut registry, digest) = module_registry(false);
        attach(
            &mut registry,
            &digest,
            &key.bundle(&digest, COSIGN_SIGN_PREDICATE),
            Shape::ArtifactType,
        );
        // Any configured key may be the one that signed.
        assert_eq!(verify(registry, &digest, &[&other, &key]), Ok(()));
    }

    #[test]
    fn a_bundle_announced_by_annotations_only_verifies() {
        // cosign 3.0's shape on GHCR: no artifactType, so the listing
        // shows the empty config's type.
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(false);
        attach(
            &mut registry,
            &digest,
            &key.bundle(&digest, COSIGN_SIGN_PREDICATE),
            Shape::AnnotationsOnly,
        );
        assert_eq!(verify(registry, &digest, &[&key]), Ok(()));
    }

    #[test]
    fn a_statement_for_another_digest_is_refused() {
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(true);
        let other = format!("sha256:{}", "9".repeat(64));
        attach(
            &mut registry,
            &digest,
            &key.bundle(&other, COSIGN_SIGN_PREDICATE),
            Shape::ArtifactType,
        );
        assert_eq!(
            verify(registry, &digest, &[&key]),
            Err(invalid(&digest, "signed statement is for another digest"))
        );
    }

    #[test]
    fn a_referrer_of_another_manifest_is_ignored() {
        // A tag index lists a referrer whose subject is another manifest,
        // carrying a bundle that would otherwise verify.
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(false);
        let other = format!("sha256:{}", "9".repeat(64));
        attach(
            &mut registry,
            &other,
            &key.bundle(&digest, COSIGN_SIGN_PREDICATE),
            Shape::ArtifactType,
        );
        let index = registry
            .manifests
            .remove(&crate::oci::referrers_tag(&other))
            .expect("tag index");
        registry
            .manifests
            .insert(crate::oci::referrers_tag(&digest), index);
        assert_eq!(verify(registry, &digest, &[&key]), Err(unsigned(&digest)));
    }

    #[test]
    fn a_wrong_key_is_refused() {
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(true);
        attach(
            &mut registry,
            &digest,
            &key.bundle(&digest, COSIGN_SIGN_PREDICATE),
            Shape::ArtifactType,
        );
        assert_eq!(
            verify(registry, &digest, &[&TestKey::from_seed(9)]),
            Err(invalid(
                &digest,
                "signature does not verify with the configured keys"
            ))
        );
    }

    #[test]
    fn an_unsigned_module_is_refused() {
        let key = TestKey::from_seed(7);
        for referrers_api in [true, false] {
            let (registry, digest) = module_registry(referrers_api);
            assert_eq!(
                verify(registry, &digest, &[&key]),
                Err(unsigned(&digest)),
                "referrers api {referrers_api}"
            );
        }
    }

    #[test]
    fn provenance_signed_by_the_key_is_not_a_signature() {
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(true);
        attach(
            &mut registry,
            &digest,
            &key.bundle(&digest, SLSA_PROVENANCE),
            Shape::AnnotationsOnly,
        );
        assert_eq!(
            verify(registry, &digest, &[&key]),
            Err(invalid(
                &digest,
                "signed statement's predicate type is \"https://slsa.dev/provenance/v1\", not https://sigstore.dev/cosign/sign/v1"
            ))
        );
    }

    /// A keyless bundle's shape: a certificate and transparency-log
    /// material instead of a key hint, signed by a key nobody configured.
    fn keyless_bundle(digest: &str) -> Vec<u8> {
        let mut bundle: serde_json::Value =
            serde_json::from_slice(&TestKey::from_seed(11).bundle(digest, COSIGN_SIGN_PREDICATE))
                .expect("bundle");
        bundle["verificationMaterial"] = serde_json::json!({
            "certificate": {"rawBytes": "MIIC1jCCAlygAwIBAgIU"},
            "tlogEntries": [{"logIndex": "42", "kindVersion": {"kind": "dsse", "version": "0.0.1"}}],
            "timestampVerificationData": {},
        });
        serde_json::to_vec(&bundle).expect("bundle")
    }

    #[test]
    fn a_keyless_bundle_beside_a_key_bundle_does_not_get_in_the_way() {
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(false);
        // Listed first, so it is examined first.
        attach(
            &mut registry,
            &digest,
            &keyless_bundle(&digest),
            Shape::AnnotationsOnly,
        );
        attach(
            &mut registry,
            &digest,
            &key.bundle(&digest, COSIGN_SIGN_PREDICATE),
            Shape::ArtifactType,
        );
        assert_eq!(verify(registry, &digest, &[&key]), Ok(()));

        let (mut registry, digest) = module_registry(false);
        attach(
            &mut registry,
            &digest,
            &keyless_bundle(&digest),
            Shape::AnnotationsOnly,
        );
        assert_eq!(
            verify(registry, &digest, &[&key]),
            Err(invalid(
                &digest,
                "signature does not verify with the configured keys"
            ))
        );
    }

    #[test]
    fn a_legacy_signature_alone_is_unsigned() {
        // cosign 2's default: a simple-signing payload under the
        // sha256-<hex>.sig tag, signed with the configured key.
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(false);
        let payload = serde_json::to_vec(&serde_json::json!({
            "critical": {
                "identity": {"docker-reference": ""},
                "image": {"docker-manifest-digest": digest},
                "type": "cosign container image signature",
            },
            "optional": null,
        }))
        .expect("payload");
        use base64::Engine as _;
        let signature = base64::engine::general_purpose::STANDARD.encode(key.sign(&payload));
        let sig_manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST_TYPE,
            "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(b"{}"), "size": 2},
            "layers": [{
                "mediaType": "application/vnd.dev.cosign.simplesigning.v1+json",
                "digest": digest_of(&payload),
                "size": payload.len(),
                "annotations": {"dev.cosignproject.cosign/signature": signature},
            }],
        }))
        .expect("sig manifest");
        registry.blobs.insert(digest_of(&payload), payload);
        registry.manifests.insert(
            format!("{}.sig", crate::oci::referrers_tag(&digest)),
            sig_manifest,
        );
        assert_eq!(verify(registry, &digest, &[&key]), Err(unsigned(&digest)));
    }

    #[test]
    fn an_oversize_bundle_is_refused() {
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(true);
        let mut bundle: serde_json::Value =
            serde_json::from_slice(&key.bundle(&digest, COSIGN_SIGN_PREDICATE)).expect("bundle");
        bundle["padding"] = serde_json::json!("x".repeat(MAX_BUNDLE_SIZE as usize));
        let bundle = serde_json::to_vec(&bundle).expect("bundle");
        attach(&mut registry, &digest, &bundle, Shape::ArtifactType);
        assert_eq!(
            verify(registry, &digest, &[&key]),
            Err(invalid(
                &digest,
                &format!(
                    "Sigstore bundle {} of {} bytes exceeds the size limit of 1048576 bytes",
                    digest_of(&bundle),
                    bundle.len()
                )
            ))
        );
    }

    #[test]
    fn a_tampered_bundle_blob_is_refused() {
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(true);
        let bundle = key.bundle(&digest, COSIGN_SIGN_PREDICATE);
        attach(&mut registry, &digest, &bundle, Shape::ArtifactType);
        let tampered = key.bundle(&format!("sha256:{}", "9".repeat(64)), COSIGN_SIGN_PREDICATE);
        registry.blobs.insert(digest_of(&bundle), tampered.clone());
        assert_eq!(
            verify(registry, &digest, &[&key]),
            Err(invalid(
                &digest,
                &format!(
                    "Sigstore bundle content is {}, want {}",
                    digest_of(&tampered),
                    digest_of(&bundle)
                )
            ))
        );
    }

    #[test]
    fn a_message_signature_bundle_is_not_an_image_signature() {
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(true);
        let bundle = serde_json::to_vec(&serde_json::json!({
            "mediaType": BUNDLE_MEDIA_TYPE,
            "verificationMaterial": {"publicKey": {"hint": ""}},
            "messageSignature": {
                "messageDigest": {"algorithm": "SHA2_256", "digest": ""},
                "signature": "",
            },
        }))
        .expect("bundle");
        attach(&mut registry, &digest, &bundle, Shape::ArtifactType);
        assert_eq!(
            verify(registry, &digest, &[&key]),
            Err(invalid(
                &digest,
                "Sigstore bundle carries no DSSE envelope (a message signature signs a blob, not an image)"
            ))
        );
    }

    #[test]
    fn the_referrers_examined_are_bounded() {
        // MAX_REFERRERS referrers that carry no bundle, then a valid one
        // the bound leaves unexamined.
        let key = TestKey::from_seed(7);
        let (mut registry, digest) = module_registry(false);
        for i in 0..MAX_REFERRERS {
            let manifest = serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 2,
                "mediaType": OCI_MANIFEST_TYPE,
                "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": digest_of(b"{}"), "size": 2},
                "layers": [],
                "annotations": {"n": i.to_string()},
                "subject": {"mediaType": OCI_MANIFEST_TYPE, "digest": digest, "size": 0},
            }))
            .expect("referrer");
            registry.push_referrer(manifest);
        }
        attach(
            &mut registry,
            &digest,
            &key.bundle(&digest, COSIGN_SIGN_PREDICATE),
            Shape::ArtifactType,
        );
        assert_eq!(
            verify(registry, &digest, &[&key]),
            Err(invalid(
                &digest,
                "only the first 32 of its 33 referrers were examined"
            ))
        );
    }

    #[test]
    fn pae_matches_the_dsse_spec() {
        assert_eq!(
            pae("http://example.com/HelloWorld", b"hello world"),
            b"DSSEv1 29 http://example.com/HelloWorld 11 hello world"
        );
    }
}
