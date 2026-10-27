//! An in-memory Sigstore instance for tests: a Fulcio certificate authority,
//! a certificate-transparency log and a Rekor log, each a P-256 key from a
//! fixed seed, that issues keyless cosign signatures and publishes the
//! trusted root that verifies them. The keyless path is then covered
//! offline, deterministically, without the public-good instance's rotating
//! material. The signing time is fixed in the past and the certificate is
//! valid for ten minutes around it, as Fulcio issues them, so a signature
//! verifies only because its signing time is authenticated by the Rekor
//! entry, never because the clock happens to fall inside the certificate.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use p256::pkcs8::EncodePrivateKey as _;
use rcgen::{
    BasicConstraints, CertificateParams, CustomExtension, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, PublicKeyData as _, SanType,
    SerialNumber, SigningKey as _,
};
use sha2::Digest as _;
use sigstore_trust_root::TrustedRoot;
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, Month, OffsetDateTime};
use x509_cert::der::asn1::ObjectIdentifier;
use x509_cert::der::{Decode as _, Encode as _, Header, SliceReader};
use x509_cert::ext::Extensions;

use crate::cosign::{BUNDLE_MEDIA_TYPE, IN_TOTO_PAYLOAD_TYPE, IN_TOTO_STATEMENT, Signer};

/// Fulcio's OIDC issuer extension, the current form: a DER UTF8String.
const FULCIO_ISSUER_V2: &[u64] = &[1, 3, 6, 1, 4, 1, 57264, 1, 8];
/// RFC 6962's embedded signed-certificate-timestamp list.
const CT_PRECERT_SCTS: &[u64] = &[1, 3, 6, 1, 4, 1, 11129, 2, 4, 2];
const CT_PRECERT_SCTS_OID: &str = "1.3.6.1.4.1.11129.2.4.2";

/// The instance: three keys and the certificate authority they anchor.
pub struct Instance {
    fulcio: KeyPair,
    fulcio_params: CertificateParams,
    fulcio_der: Vec<u8>,
    ct: KeyPair,
    rekor: KeyPair,
    /// When every authority became valid.
    since: OffsetDateTime,
    name: String,
}

impl Default for Instance {
    fn default() -> Self {
        Self::new()
    }
}

impl Instance {
    /// The instance the tests sign with.
    pub fn new() -> Instance {
        Self::with_seeds(21)
    }

    /// A second instance whose trusted root vouches for nothing the first
    /// signed.
    pub fn other() -> Instance {
        Self::with_seeds(31)
    }

    /// An instance whose keys derive from base, base + 1 and base + 2.
    pub fn with_seeds(base: u8) -> Instance {
        let fulcio = key_from_seed(base);
        let since = utc(2026, Month::January, 1);
        let name = format!("sigstore-{base}.test");
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::OrganizationName, "function-wasm tests");
        dn.push(DnType::CommonName, format!("fulcio.{name}"));
        params.distinguished_name = dn;
        params.not_before = since;
        params.not_after = utc(2036, Month::January, 1);
        params.serial_number = Some(SerialNumber::from(vec![0x7f, base]));
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let fulcio_der = params.self_signed(&fulcio).expect("ca").der().to_vec();
        Instance {
            fulcio,
            fulcio_params: params,
            fulcio_der,
            ct: key_from_seed(base + 1),
            rekor: key_from_seed(base + 2),
            since,
            name,
        }
    }

    /// When the instance signs: fixed, in the past, so every verified time
    /// is deterministic and every certificate it issues has expired.
    pub fn signing_time() -> OffsetDateTime {
        utc(2026, Month::October, 1) + Duration::hours(12)
    }

    /// The trusted root a verifier of this instance's signatures reads: the
    /// Rekor key, the Fulcio chain and the CT key, each valid since the
    /// instance started.
    pub fn trusted_root_json(&self) -> Vec<u8> {
        let since = rfc3339(self.since);
        let log = |key: &KeyPair, url: String| {
            serde_json::json!({
                "baseUrl": url,
                "hashAlgorithm": "SHA2_256",
                "publicKey": {
                    "rawBytes": BASE64.encode(key.subject_public_key_info()),
                    "keyDetails": "PKIX_ECDSA_P256_SHA_256",
                    "validFor": {"start": since},
                },
                "logId": {"keyId": BASE64.encode(key_id(key))},
            })
        };
        serde_json::to_vec(&serde_json::json!({
            "mediaType": "application/vnd.dev.sigstore.trustedroot+json;version=0.1",
            "tlogs": [log(&self.rekor, format!("https://rekor.{}", self.name))],
            "certificateAuthorities": [{
                "subject": {"organization": "function-wasm tests", "commonName": format!("fulcio.{}", self.name)},
                "uri": format!("https://fulcio.{}", self.name),
                "certChain": {"certificates": [{"rawBytes": BASE64.encode(&self.fulcio_der)}]},
                "validFor": {"start": since},
            }],
            "ctlogs": [log(&self.ct, format!("https://ctfe.{}", self.name))],
            "timestampAuthorities": [],
        }))
        .expect("trusted root")
    }

    /// The trusted root, parsed.
    pub fn trusted_root(&self) -> TrustedRoot {
        TrustedRoot::from_json(std::str::from_utf8(&self.trusted_root_json()).expect("utf-8"))
            .expect("trusted root parses")
    }

    /// The keyless cosign bundle for a signature of subject (a sha256
    /// manifest digest) with predicate_type, by signer, made at
    /// [`Instance::signing_time`]: a fresh signing key, a ten-minute Fulcio
    /// certificate naming the signer with an embedded SCT, a Rekor dsse
    /// 0.0.1 entry with its signed entry timestamp, inclusion proof and
    /// checkpoint, no RFC 3161 timestamp.
    pub fn sign(&self, subject: &str, predicate_type: &str, signer: &Signer) -> Vec<u8> {
        let signed_at = Self::signing_time();
        let hex = subject.strip_prefix("sha256:").expect("a sha256 digest");
        let payload = serde_json::to_vec(&serde_json::json!({
            "_type": IN_TOTO_STATEMENT,
            "subject": [{"digest": {"sha256": hex}, "annotations": {}}],
            "predicateType": predicate_type,
            "predicate": {},
        }))
        .expect("statement");
        let leaf_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("leaf key");
        let signature = leaf_key
            .sign(&crate::cosign::pae(IN_TOTO_PAYLOAD_TYPE, &payload))
            .expect("sign");

        // The certificate, twice: the SCT signs the TBS without its own
        // extension, so a first pass with a placeholder yields those bytes,
        // the CT log signs them, and the second pass carries the SCT.
        let issuer = Issuer::from_params(&self.fulcio_params, &self.fulcio);
        let not_before = signed_at - Duration::minutes(5);
        let not_after = signed_at + Duration::minutes(5);
        let precert = leaf_params(signer, not_before, not_after, vec![0, 0])
            .signed_by(&leaf_key, &issuer)
            .expect("precert")
            .der()
            .to_vec();
        let tbs = tbs_without_sct(&precert);
        let sct = self.sct(&tbs, &issuer_spki(&self.fulcio_der), unix_millis(signed_at));
        let leaf = leaf_params(signer, not_before, not_after, sct)
            .signed_by(&leaf_key, &issuer)
            .expect("leaf")
            .der()
            .to_vec();
        assert_eq!(
            tbs_without_sct(&leaf),
            tbs,
            "the certificate's TBS must encode identically with and without the SCT"
        );

        // The Rekor entry: the dsse 0.0.1 body, its signed entry timestamp
        // and a one-leaf tree whose checkpoint the log signs.
        let envelope = serde_json::json!({
            "payload": BASE64.encode(&payload),
            "payloadType": IN_TOTO_PAYLOAD_TYPE,
            "signatures": [{"sig": BASE64.encode(&signature)}],
        });
        let envelope_bytes = serde_json::to_vec(&envelope).expect("envelope");
        let leaf_pem = pem::encode(&pem::Pem::new("CERTIFICATE", leaf.clone()));
        let body = serde_json::to_vec(&serde_json::json!({
            "apiVersion": "0.0.1",
            "kind": "dsse",
            "spec": {
                "envelopeHash": {"algorithm": "sha256", "value": hex::encode(sha256(&envelope_bytes))},
                "payloadHash": {"algorithm": "sha256", "value": hex::encode(sha256(&payload))},
                "signatures": [{"signature": BASE64.encode(&signature), "verifier": BASE64.encode(leaf_pem.as_bytes())}],
            },
        }))
        .expect("body");
        let rekor_id = key_id(&self.rekor);
        let integrated_time = signed_at.unix_timestamp();
        // Keys in the canonical (sorted) order Rekor's SET payload uses.
        let set_payload = serde_json::to_vec(&serde_json::json!({
            "body": BASE64.encode(&body),
            "integratedTime": integrated_time,
            "logID": hex::encode(rekor_id),
            "logIndex": 0,
        }))
        .expect("set payload");
        let set = self.rekor.sign(&set_payload).expect("set");
        let mut leaf_hash_input = vec![0u8];
        leaf_hash_input.extend_from_slice(&body);
        let root = sha256(&leaf_hash_input);
        let origin = format!("rekor.{} - 1", self.name);
        let note_body = format!("{origin}\n1\n{}", BASE64.encode(root));
        let checkpoint_signature = self
            .rekor
            .sign(format!("{note_body}\n").as_bytes())
            .expect("checkpoint");
        let mut hinted = rekor_id[..4].to_vec();
        hinted.extend_from_slice(&checkpoint_signature);
        let envelope_text = format!(
            "{note_body}\n\n\u{2014} rekor.{} {}\n",
            self.name,
            BASE64.encode(hinted)
        );

        serde_json::to_vec(&serde_json::json!({
            "mediaType": BUNDLE_MEDIA_TYPE,
            "verificationMaterial": {
                "certificate": {"rawBytes": BASE64.encode(&leaf)},
                "tlogEntries": [{
                    "logIndex": "0",
                    "logId": {"keyId": BASE64.encode(rekor_id)},
                    "kindVersion": {"kind": "dsse", "version": "0.0.1"},
                    "integratedTime": integrated_time.to_string(),
                    "inclusionPromise": {"signedEntryTimestamp": BASE64.encode(&set)},
                    "inclusionProof": {
                        "logIndex": "0",
                        "rootHash": BASE64.encode(root),
                        "treeSize": "1",
                        "hashes": [],
                        "checkpoint": {"envelope": envelope_text},
                    },
                    "canonicalizedBody": BASE64.encode(&body),
                }],
                "timestampVerificationData": {"rfc3161Timestamps": []},
            },
            "dsseEnvelope": envelope,
        }))
        .expect("bundle")
    }

    /// RFC 6962's SignedCertificateTimestampList with one SCT by the CT log
    /// over the precertificate: the TBS without the SCT extension, bound to
    /// the issuer's key.
    fn sct(&self, tbs: &[u8], issuer_spki: &[u8], timestamp_ms: u64) -> Vec<u8> {
        let mut signed = vec![0u8, 0u8]; // v1, certificate_timestamp
        signed.extend_from_slice(&timestamp_ms.to_be_bytes());
        signed.extend_from_slice(&[0, 1]); // precert_entry
        signed.extend_from_slice(&sha256(issuer_spki));
        let len = u32::try_from(tbs.len()).expect("tbs length");
        signed.extend_from_slice(&len.to_be_bytes()[1..]);
        signed.extend_from_slice(tbs);
        signed.extend_from_slice(&[0, 0]); // no extensions
        let signature = self.ct.sign(&signed).expect("sct");

        let mut sct = vec![0u8]; // v1
        sct.extend_from_slice(&key_id(&self.ct));
        sct.extend_from_slice(&timestamp_ms.to_be_bytes());
        sct.extend_from_slice(&[0, 0]); // no extensions
        sct.extend_from_slice(&[4, 3]); // sha256, ecdsa
        sct.extend_from_slice(&u16::try_from(signature.len()).expect("len").to_be_bytes());
        sct.extend_from_slice(&signature);
        let mut list = Vec::new();
        list.extend_from_slice(&u16::try_from(sct.len() + 2).expect("len").to_be_bytes());
        list.extend_from_slice(&u16::try_from(sct.len()).expect("len").to_be_bytes());
        list.extend_from_slice(&sct);
        list
    }
}

/// A P-256 key from a fixed seed, so an instance is the same every run.
fn key_from_seed(seed: u8) -> KeyPair {
    let signing = p256::ecdsa::SigningKey::from_bytes((&[seed; 32]).into()).expect("key");
    let pem = signing
        .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
        .expect("pkcs8");
    KeyPair::from_pem(&pem).expect("rcgen key")
}

/// The Fulcio-shaped leaf: empty subject, the signer's identity as a
/// critical SAN, code signing, the OIDC issuer extension and the SCT list.
fn leaf_params(
    signer: &Signer,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
    sct_list: Vec<u8>,
) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params.not_before = not_before;
    params.not_after = not_after;
    params.serial_number = Some(SerialNumber::from(vec![0x12, 0x34, 0x56]));
    // rcgen's IA5 string type is private; the SAN variant's field infers it.
    params.subject_alt_names = vec![if signer.identity.contains("://") {
        SanType::URI(signer.identity.as_str().try_into().expect("ia5 identity"))
    } else {
        SanType::Rfc822Name(signer.identity.as_str().try_into().expect("ia5 identity"))
    }];
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::CodeSigning];
    params.use_authority_key_identifier_extension = true;
    let issuer =
        CustomExtension::from_oid_content(FULCIO_ISSUER_V2, der(0x0c, signer.issuer.as_bytes()));
    let sct = CustomExtension::from_oid_content(CT_PRECERT_SCTS, der(0x04, &sct_list));
    params.custom_extensions = vec![issuer, sct];
    params
}

/// The certificate's TBS re-encoded without the SCT extension: what the CT
/// log signed, and what a verifier reconstructs.
fn tbs_without_sct(cert_der: &[u8]) -> Vec<u8> {
    let cert = x509_cert::Certificate::from_der(cert_der).expect("certificate");
    let tbs = cert.tbs_certificate();
    let extensions = tbs
        .extensions()
        .expect("a leaf certificate carries extensions");
    let sct_oid = ObjectIdentifier::new_unwrap(CT_PRECERT_SCTS_OID);
    let kept: Extensions = extensions
        .iter()
        .filter(|e| e.extn_id != sct_oid)
        .cloned()
        .collect();

    // x509-cert reads a TBS but no longer lets one be edited (its fields went
    // private in 0.3), so the extensions, the TBS's last field ([3] EXPLICIT),
    // are re-encoded over the kept list and spliced in under a recomputed
    // SEQUENCE header.
    let full = tbs.to_der().expect("tbs");
    let before = der(0xa3, &extensions.to_der().expect("extensions"));
    assert!(
        full.ends_with(&before),
        "the extensions are the TBS's last field"
    );
    let header_len = Header::peek(&SliceReader::new(&full).expect("tbs reader"))
        .expect("tbs header")
        .to_der()
        .expect("tbs header")
        .len();
    let mut body = full[header_len..full.len() - before.len()].to_vec();
    body.extend_from_slice(&der(0xa3, &kept.to_der().expect("extensions")));
    der(0x30, &body)
}

/// The issuer's SubjectPublicKeyInfo as its certificate encodes it.
fn issuer_spki(ca_der: &[u8]) -> Vec<u8> {
    x509_cert::Certificate::from_der(ca_der)
        .expect("ca certificate")
        .tbs_certificate()
        .subject_public_key_info()
        .to_der()
        .expect("spki")
}

/// A log's key id: SHA-256 of its SubjectPublicKeyInfo.
fn key_id(key: &KeyPair) -> [u8; 32] {
    sha256(&key.subject_public_key_info())
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(bytes).into()
}

/// One DER value of the given tag over content.
fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend_from_slice(&[0x81, len as u8]);
    } else {
        out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
    }
    out.extend_from_slice(content);
    out
}

fn utc(year: i32, month: Month, day: u8) -> OffsetDateTime {
    Date::from_calendar_date(year, month, day)
        .expect("date")
        .midnight()
        .assume_utc()
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).expect("rfc 3339")
}

fn unix_millis(t: OffsetDateTime) -> u64 {
    u64::try_from(t.unix_timestamp_nanos() / 1_000_000).expect("millis")
}
