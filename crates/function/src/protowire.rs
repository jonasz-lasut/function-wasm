//! Minimal protobuf wire surgery over raw request/response bytes - what
//! keeps the transparent proxy honest: the guest receives the caller's
//! bytes (unknown fields included, which prost cannot retain through a
//! decode), with exactly two edits the runtime is entitled to make - the
//! pull credential removed from the forwarded request, and a meta field
//! appended to a response that lacks one (valid protobuf: last value wins
//! for a singular field).

/// RunFunctionRequest.credentials (map<string, Credentials>).
const CREDENTIALS_FIELD: u64 = 7;
/// The key field of a protobuf map entry.
const MAP_KEY_FIELD: u64 = 1;
/// RunFunctionResponse.meta.
const META_FIELD: u64 = 1;

/// Removes the credentials entries whose key is name from raw - the wire
/// form of the Go runtime's withheld pull credential - leaving every other
/// byte, unknown fields included, exactly as the caller sent them. Bytes
/// that do not parse as protobuf come back unchanged: the typed decode has
/// already succeeded by the time this runs, so this is defensive only.
pub fn strip_credential(raw: &[u8], name: &str) -> Vec<u8> {
    match try_strip(raw, name) {
        Some(out) => out,
        None => raw.to_vec(),
    }
}

fn try_strip(raw: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let start = i;
        let (tag, n) = varint(raw, i)?;
        i += n;
        let field = tag >> 3;
        let wire = tag & 0x7;
        let value_end = skip_value(raw, i, wire)?;
        if field == CREDENTIALS_FIELD && wire == 2 {
            // A length-delimited credentials map entry: drop it when its
            // key matches the withheld name.
            let (len, n) = varint(raw, i)?;
            let entry = &raw[i + n..i + n + len as usize];
            if map_entry_key(entry) == Some(name.to_string()) {
                i = value_end;
                continue;
            }
        }
        out.extend_from_slice(&raw[start..value_end]);
        i = value_end;
    }
    Some(out)
}

/// The string key (field 1) of a protobuf map entry.
fn map_entry_key(entry: &[u8]) -> Option<String> {
    let mut i = 0;
    while i < entry.len() {
        let (tag, n) = varint(entry, i)?;
        i += n;
        let field = tag >> 3;
        let wire = tag & 0x7;
        if field == MAP_KEY_FIELD && wire == 2 {
            let (len, n) = varint(entry, i)?;
            let key = entry.get(i + n..i + n + len as usize)?;
            return Some(String::from_utf8_lossy(key).into_owned());
        }
        i = skip_value(entry, i, wire)?;
    }
    None
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

    #[test]
    fn strips_only_the_named_credential_and_keeps_unknown_fields() {
        let req = RunFunctionRequest {
            meta: Some(RequestMeta {
                tag: "t".to_string(),
                ..Default::default()
            }),
            credentials: HashMap::from([
                ("pull".to_string(), credential(b"registry secret")),
                ("api".to_string(), credential(b"guest secret")),
            ]),
            ..Default::default()
        };
        let mut raw = req.encode_to_vec();
        // A field this proto does not know: field 999, length-delimited.
        let unknown = [0xba, 0x3e, 0x03, b'x', b'y', b'z'];
        raw.extend_from_slice(&unknown);

        let stripped = strip_credential(&raw, "pull");
        let decoded = RunFunctionRequest::decode(stripped.as_slice()).expect("decode");
        assert!(!decoded.credentials.contains_key("pull"));
        assert!(decoded.credentials.contains_key("api"));
        assert_eq!(decoded.meta.expect("meta").tag, "t");
        // The unknown field survived byte-for-byte.
        assert!(stripped.windows(unknown.len()).any(|w| w == unknown));
        // And the secret's bytes did not.
        assert!(!stripped.windows(15).any(|w| w == b"registry secret"));
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
