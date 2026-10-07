//! Minimal protobuf wire surgery over raw request/response bytes - what
//! keeps the transparent proxy honest: the guest receives the caller's
//! bytes (unknown fields included, which prost cannot retain through a
//! decode), with exactly two edits the runtime is entitled to make - the
//! step credentials the module was not granted (the pull credential among
//! them, always) removed from the forwarded request, and a meta field
//! appended to a response that lacks one (valid protobuf: last value wins
//! for a singular field) - and one response it produces itself, the no-op
//! of a step with no module to run, built from the caller's own desired
//! state and context bytes.

/// RunFunctionRequest.desired (State).
const REQUEST_DESIRED_FIELD: u64 = 3;
/// RunFunctionRequest.context (google.protobuf.Struct).
const REQUEST_CONTEXT_FIELD: u64 = 5;
/// RunFunctionRequest.credentials (map<string, Credentials>).
const CREDENTIALS_FIELD: u64 = 7;
/// The key field of a protobuf map entry.
const MAP_KEY_FIELD: u64 = 1;
/// RunFunctionResponse.meta.
const META_FIELD: u64 = 1;
/// RunFunctionResponse.desired (State).
const RESPONSE_DESIRED_FIELD: u64 = 2;
/// RunFunctionResponse.context (google.protobuf.Struct).
const RESPONSE_CONTEXT_FIELD: u64 = 4;

/// Keeps the credentials map entries whose key forward admits and removes
/// every other one from raw - the wire form of withholding the step
/// credentials a module was not granted - leaving every other byte, unknown
/// fields included, exactly as the caller sent them. Withholding fails
/// closed: an entry whose key cannot be read is removed, and bytes that do
/// not parse as protobuf yield None rather than a request that might still
/// carry a withheld secret (the typed decode has already succeeded by the
/// time this runs, so that is defensive only).
pub fn retain_credentials(raw: &[u8], forward: impl Fn(&str) -> bool) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let start = i;
        let (tag, n) = varint(raw, i)?;
        i += n;
        let wire = tag & 0x7;
        let value_end = skip_value(raw, i, wire)?;
        if tag >> 3 == CREDENTIALS_FIELD {
            // A map entry is length-delimited; anything else under the
            // field cannot be a credential the module was granted.
            let forwarded = wire == 2 && {
                let (_, n) = varint(raw, i)?;
                map_entry_key(&raw[i + n..value_end]).is_some_and(|key| forward(&key))
            };
            if !forwarded {
                i = value_end;
                continue;
            }
        }
        out.extend_from_slice(&raw[start..value_end]);
        i = value_end;
    }
    Some(out)
}

/// The string key (field 1) of a protobuf map entry: its last occurrence,
/// the one a decoder keeps, so the key judged is the key the guest reads.
/// None for an entry without one, or one that does not parse.
fn map_entry_key(entry: &[u8]) -> Option<String> {
    let mut key = None;
    let mut i = 0;
    while i < entry.len() {
        let (tag, n) = varint(entry, i)?;
        i += n;
        let wire = tag & 0x7;
        let value_end = skip_value(entry, i, wire)?;
        if tag >> 3 == MAP_KEY_FIELD && wire == 2 {
            let (_, n) = varint(entry, i)?;
            key = Some(String::from_utf8(entry[i + n..value_end].to_vec()).ok()?);
        }
        i = value_end;
    }
    key
}

/// Appends meta (an encoded ResponseMeta) to a response that lacks one, as
/// field 1 - concatenation is valid protobuf, and the guest's own bytes
/// stay untouched.
pub fn append_meta(mut raw: Vec<u8>, meta: &[u8]) -> Vec<u8> {
    raw.push((META_FIELD << 3) as u8 | 2);
    push_varint(&mut raw, meta.len() as u64);
    raw.extend_from_slice(meta);
    raw
}

/// The response of a step that ran no module: the request's desired state
/// and context, byte for byte (a field of theirs newer than the vendored
/// proto survives, as it would through a guest), under the given meta - the
/// SDK's response::to, without the typed round trip. Nothing else of the
/// request crosses over: not its input, credentials or observed state.
/// Request bytes that do not parse yield the bare meta; the typed decode has
/// already succeeded by the time this runs, so that is defensive only.
pub fn noop_response(raw_request: &[u8], meta: &[u8]) -> Vec<u8> {
    let mut out = append_meta(Vec::with_capacity(raw_request.len()), meta);
    let mut i = 0;
    while i < raw_request.len() {
        let Some((tag, n)) = varint(raw_request, i) else {
            break;
        };
        i += n;
        let wire = tag & 0x7;
        let Some(value_end) = skip_value(raw_request, i, wire) else {
            break;
        };
        let retagged = match tag >> 3 {
            REQUEST_DESIRED_FIELD if wire == 2 => Some(RESPONSE_DESIRED_FIELD),
            REQUEST_CONTEXT_FIELD if wire == 2 => Some(RESPONSE_CONTEXT_FIELD),
            _ => None,
        };
        if let Some(field) = retagged {
            out.push((field << 3) as u8 | 2);
            out.extend_from_slice(&raw_request[i..value_end]);
        }
        i = value_end;
    }
    out
}

/// The end offset of a field value starting at i with the given wire type.
fn skip_value(raw: &[u8], i: usize, wire: u64) -> Option<usize> {
    match wire {
        0 => varint(raw, i).map(|(_, n)| i + n),
        1 => (i + 8 <= raw.len()).then_some(i + 8),
        2 => {
            let (len, n) = varint(raw, i)?;
            let end = i + n + len as usize;
            (end <= raw.len()).then_some(end)
        }
        5 => (i + 4 <= raw.len()).then_some(i + 4),
        _ => None,
    }
}

fn varint(raw: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0;
    let start = i;
    loop {
        let b = *raw.get(i)?;
        value |= u64::from(b & 0x7f) << shift;
        i += 1;
        if b & 0x80 == 0 {
            return Some((value, i - start));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

fn push_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use function_sdk_rust::proto::v1::{
        CredentialData, Credentials, RequestMeta, ResponseMeta, RunFunctionRequest,
        RunFunctionResponse, credentials,
    };
    use prost::Message as _;
    use std::collections::HashMap;

    fn credential(value: &[u8]) -> Credentials {
        Credentials {
            source: Some(credentials::Source::CredentialData(CredentialData {
                data: HashMap::from([("k".to_string(), value.to_vec())]),
            })),
        }
    }

    /// One credentials map entry on the wire: what a request carrying only
    /// that credential encodes to.
    fn credential_entry(name: &str, value: &[u8]) -> Vec<u8> {
        RunFunctionRequest {
            credentials: HashMap::from([(name.to_string(), credential(value))]),
            ..Default::default()
        }
        .encode_to_vec()
    }

    #[test]
    fn withholds_every_credential_not_forwarded_and_keeps_every_other_byte() {
        let meta = RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
        .encode_to_vec();
        // Fields this proto does not know (999 and 1000, length-delimited),
        // between and after the entries.
        let unknown = [0xba, 0x3e, 0x03, b'x', b'y', b'z'];
        let unknown_too = [0xc2, 0x3e, 0x02, b'u', b'v'];
        // (segment, kept) in wire order: the caller's bytes, with the
        // entries the module was not granted interleaved.
        let segments: Vec<(Vec<u8>, bool)> = vec![
            (meta, true),
            (credential_entry("pull", b"registry secret"), false),
            (unknown.to_vec(), true),
            (credential_entry("cmdb", b"granted secret"), true),
            (credential_entry("other", b"another secret"), false),
            (unknown_too.to_vec(), true),
            (credential_entry("api", b"guest secret"), false),
            (credential_entry("db", b"bound secret"), true),
        ];
        let raw: Vec<u8> = segments.iter().flat_map(|(s, _)| s.clone()).collect();
        let want: Vec<u8> = segments
            .iter()
            .filter(|(_, kept)| *kept)
            .flat_map(|(s, _)| s.clone())
            .collect();

        let forwarded =
            retain_credentials(&raw, |name| name == "cmdb" || name == "db").expect("parses");
        // Exactly the withheld entries are gone; every other byte, unknown
        // fields included, is the caller's, in the caller's order.
        assert_eq!(forwarded, want);
        let decoded = RunFunctionRequest::decode(forwarded.as_slice()).expect("decode");
        let mut names: Vec<&String> = decoded.credentials.keys().collect();
        names.sort();
        assert_eq!(names, ["cmdb", "db"]);
        assert_eq!(decoded.meta.expect("meta").tag, "t");
        for secret in [&b"registry secret"[..], b"another secret", b"guest secret"] {
            assert!(
                !forwarded.windows(secret.len()).any(|w| w == secret),
                "{}",
                String::from_utf8_lossy(secret)
            );
        }

        // Nothing forwarded: every entry goes, nothing else does.
        let none = retain_credentials(&raw, |_| false).expect("parses");
        let decoded = RunFunctionRequest::decode(none.as_slice()).expect("decode");
        assert!(decoded.credentials.is_empty());
        assert!(none.windows(unknown.len()).any(|w| w == unknown));
        assert!(none.windows(unknown_too.len()).any(|w| w == unknown_too));
    }

    #[test]
    fn withholding_judges_the_key_a_decoder_reads_and_fails_closed() {
        // An entry repeating its key: a decoder keeps the last one, so that
        // is the one judged.
        let mut entry = vec![0x0a, 0x04];
        entry.extend_from_slice(b"cmdb");
        entry.extend_from_slice(&[0x0a, 0x05]);
        entry.extend_from_slice(b"other");
        let mut raw = vec![(7 << 3) | 2, entry.len() as u8];
        raw.extend_from_slice(&entry);
        let decoded = RunFunctionRequest::decode(raw.as_slice()).expect("decode");
        assert!(decoded.credentials.contains_key("other"));
        assert_eq!(
            retain_credentials(&raw, |name| name == "cmdb").expect("parses"),
            Vec::<u8>::new()
        );
        assert_eq!(
            retain_credentials(&raw, |name| name == "other").expect("parses"),
            raw
        );
        // An entry without a key, or a credentials field that is not an
        // entry at all, is never forwarded.
        assert_eq!(
            retain_credentials(&[(7 << 3) | 2, 0x00], |_| true).expect("parses"),
            Vec::<u8>::new()
        );
        assert_eq!(
            retain_credentials(&[7 << 3, 0x01], |_| true).expect("parses"),
            Vec::<u8>::new()
        );
        // Bytes that do not parse are never forwarded.
        assert_eq!(retain_credentials(&[0xff], |_| true), None);
        assert_eq!(
            retain_credentials(&[(7 << 3) | 2, 0x05, 0x0a], |_| true),
            None
        );
    }

    #[test]
    fn the_noop_response_carries_the_desired_state_and_context_verbatim() {
        use function_sdk_rust::proto::v1::{Resource, State};
        use function_sdk_rust::resource::json_to_struct;

        let desired = State {
            composite: Some(Resource {
                resource: Some(json_to_struct(
                    serde_json::json!({"kind": "XR", "status": {"ready": true}})
                        .as_object()
                        .expect("object"),
                )),
                ..Default::default()
            }),
            resources: HashMap::from([(
                "bucket".to_string(),
                Resource {
                    resource: Some(json_to_struct(
                        serde_json::json!({"kind": "Bucket"})
                            .as_object()
                            .expect("object"),
                    )),
                    ..Default::default()
                },
            )]),
        };
        let context = json_to_struct(
            serde_json::json!({"apiextensions.crossplane.io/environment": {"region": "eu"}})
                .as_object()
                .expect("object"),
        );
        let req = RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".to_string(),
                ..Default::default()
            }),
            observed: Some(desired.clone()),
            desired: Some(desired.clone()),
            input: Some(json_to_struct(
                serde_json::json!({"module": {"type": "OCI"}})
                    .as_object()
                    .expect("object"),
            )),
            context: Some(context.clone()),
            credentials: HashMap::from([("api".to_string(), credential(b"guest secret"))]),
            ..Default::default()
        };
        // A field of State this proto does not know (field 999, length-
        // delimited) inside desired: the bytes must cross over untouched.
        let unknown = [0xba, 0x3e, 0x03, b'x', b'y', b'z'];
        let mut desired_bytes = desired.encode_to_vec();
        desired_bytes.extend_from_slice(&unknown);
        let mut raw = RunFunctionRequest {
            desired: None,
            ..req.clone()
        }
        .encode_to_vec();
        raw.push((3 << 3) | 2);
        push_varint(&mut raw, desired_bytes.len() as u64);
        raw.extend_from_slice(&desired_bytes);

        let meta = ResponseMeta {
            tag: "t".to_string(),
            ttl: None,
        }
        .encode_to_vec();
        let out = noop_response(&raw, &meta);

        let decoded = RunFunctionResponse::decode(out.as_slice()).expect("decode");
        assert_eq!(decoded.meta.expect("meta").tag, "t");
        assert_eq!(decoded.desired, Some(desired));
        assert_eq!(decoded.context, Some(context));
        assert!(decoded.results.is_empty());
        assert!(out.windows(unknown.len()).any(|w| w == unknown));
        // Nothing else of the request crossed over.
        assert!(!out.windows(12).any(|w| w == b"guest secret"));
        assert!(!out.windows(6).any(|w| w == b"module"));
        // And a request that is not protobuf yields the bare meta.
        assert_eq!(
            noop_response(&[0xff], &meta),
            append_meta(Vec::new(), &meta)
        );
    }

    #[test]
    fn appends_meta_without_touching_the_guest_bytes() {
        let guest = RunFunctionResponse::default().encode_to_vec();
        let meta = ResponseMeta {
            tag: "t".to_string(),
            ttl: None,
        }
        .encode_to_vec();
        let out = append_meta(guest.clone(), &meta);
        assert!(out.starts_with(&guest));
        let decoded = RunFunctionResponse::decode(out.as_slice()).expect("decode");
        assert_eq!(decoded.meta.expect("meta").tag, "t");
    }
}
