//! The partial decode of a request and a response: what the request path
//! reads, and a check that the rest would decode.
//!
//! A typed decode of a whole RunFunctionRequest builds every observed and
//! desired resource as a tree of maps, and the request path reads almost
//! none of it: admission needs the Input, the observed composite resource
//! and the step credentials, and a response that no policy layer fences is
//! only asked whether it carries a meta. `Request`, `Desired` and
//! `ResponseHead` are prost messages over the same field numbers that
//! declare those fields alone, so prost skips the others by their length
//! and merges the declared ones exactly as the full message would.
//!
//! Skipping a field is not decoding it, and the runtime's promise is that
//! a request or a response the full message cannot decode never passes: a
//! request is a gRPC error, a response a fatal result naming the module.
//! `request_decodes` and `response_decodes` keep it: they walk the bytes
//! against the message's schema, building nothing, and say whether prost
//! would decode them. They may say no to bytes prost accepts (a group, a
//! nesting past `MAX_DEPTH`), in which case the caller asks prost itself
//! and takes its answer and its error; they never say yes to bytes prost
//! refuses. The schema is read from the descriptor the SDK embeds, the one
//! its messages were generated from, so a field the SDK gains is checked
//! from the day it arrives.

use std::collections::HashMap;
use std::sync::OnceLock;

use function_sdk_rust::proto::v1::{Credentials, RequestMeta, Resource, ResponseMeta, State};
use prost::Message as _;
use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{DescriptorProto, FileDescriptorSet};

/// What admission reads of a RunFunctionRequest: the tag, the observed
/// composite resource, the Input and the step credentials.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Request {
    #[prost(message, optional, tag = "1")]
    pub meta: Option<RequestMeta>,
    #[prost(message, optional, tag = "2")]
    pub observed: Option<Observed>,
    #[prost(message, optional, tag = "4")]
    pub input: Option<pbjson_types::Struct>,
    #[prost(map = "string, message", tag = "7")]
    pub credentials: HashMap<String, Credentials>,
}

/// The observed State's composite resource, without the composed ones.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Observed {
    #[prost(message, optional, tag = "1")]
    pub composite: Option<Resource>,
}

/// A RunFunctionRequest's desired state, which the output fence diffs the
/// response's against.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Desired {
    #[prost(message, optional, tag = "3")]
    pub desired: Option<State>,
}

/// A RunFunctionResponse's meta: whether the guest wrote one is all the
/// runtime asks of a response no policy layer fences.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ResponseHead {
    #[prost(message, optional, tag = "1")]
    pub meta: Option<ResponseMeta>,
}

impl Request {
    /// The Input as a typed value, with the SDK's own errors: a request
    /// with no input, or one that does not deserialize.
    pub fn input<T: serde::de::DeserializeOwned>(&self) -> Result<T, function_sdk_rust::Error> {
        let input = self
            .input
            .as_ref()
            .ok_or(function_sdk_rust::Error::MissingInput)?;
        Ok(serde_json::from_value(
            function_sdk_rust::resource::struct_to_json(input),
        )?)
    }

    /// The observed composite resource.
    pub fn composite(&self) -> Option<&Resource> {
        self.observed.as_ref()?.composite.as_ref()
    }
}

/// Whether prost would decode raw as a RunFunctionRequest. False is not a
/// refusal: the caller then decodes with prost for the verdict.
pub fn request_decodes(raw: &[u8]) -> bool {
    schema().is_some_and(|s| s.valid(s.request, raw, 0))
}

/// Whether prost would decode raw as a RunFunctionResponse.
pub fn response_decodes(raw: &[u8]) -> bool {
    schema().is_some_and(|s| s.valid(s.response, raw, 0))
}

/// The nesting the check follows before it leaves the verdict to prost,
/// whose own limit is 100 messages deep. Every nested message and map
/// entry counts, as prost counts them, so nothing the check accepts is
/// past prost's limit. A Kubernetes object nests three messages per level
/// of its JSON (Value, Struct, map entry), so this is some twenty levels.
const MAX_DEPTH: u32 = 64;

/// The highest field number a schema may declare. The messages here use
/// single digits; a descriptor past this is not one the check was written
/// for, and it stands aside.
const MAX_FIELD_NUMBER: usize = 1024;

const PACKAGE: &str = ".apiextensions.fn.proto.v1";

#[derive(Clone, Copy, Debug)]
enum Kind {
    Varint,
    Fixed32,
    Fixed64,
    Bytes,
    String,
    /// A nested message or a map entry, by its index in the schema.
    Message(usize),
}

#[derive(Clone, Copy, Debug)]
struct Field {
    kind: Kind,
    repeated: bool,
}

/// Every message reachable from the two served ones: for each, its fields
/// by number.
struct Schema {
    messages: Vec<Vec<Option<Field>>>,
    request: usize,
    response: usize,
}

/// The schema, or None when the embedded descriptor is not one the check
/// can follow - every request and response is then decoded by prost, as
/// if the check did not exist.
fn schema() -> Option<&'static Schema> {
    static SCHEMA: OnceLock<Option<Schema>> = OnceLock::new();
    SCHEMA
        .get_or_init(|| Schema::read(function_sdk_rust::proto::FILE_DESCRIPTOR_SET))
        .as_ref()
}

impl Schema {
    fn read(descriptor_set: &[u8]) -> Option<Schema> {
        let set = FileDescriptorSet::decode(descriptor_set).ok()?;
        // Every message of every file by its full name, nested ones (the
        // map entries among them) included.
        let mut named: Vec<(String, &DescriptorProto)> = Vec::new();
        fn collect<'a>(
            prefix: &str,
            message: &'a DescriptorProto,
            out: &mut Vec<(String, &'a DescriptorProto)>,
        ) {
            let name = format!("{prefix}.{}", message.name());
            for nested in &message.nested_type {
                collect(&name, nested, out);
            }
            out.push((name, message));
        }
        for file in &set.file {
            let prefix = match file.package() {
                "" => String::new(),
                package => format!(".{package}"),
            };
            for message in &file.message_type {
                collect(&prefix, message, &mut named);
            }
        }
        let index: HashMap<&str, usize> = named
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (name.as_str(), i))
            .collect();
        if index.len() != named.len() {
            return None;
        }
        let mut messages = Vec::with_capacity(named.len());
        for (_, message) in &named {
            let mut fields: Vec<Option<Field>> = Vec::new();
            for field in &message.field {
                let number = usize::try_from(field.number()).ok()?;
                if number == 0 || number > MAX_FIELD_NUMBER {
                    return None;
                }
                let kind = match field.r#type() {
                    Type::Int64
                    | Type::Uint64
                    | Type::Int32
                    | Type::Uint32
                    | Type::Sint32
                    | Type::Sint64
                    | Type::Bool
                    | Type::Enum => Kind::Varint,
                    Type::Fixed64 | Type::Sfixed64 | Type::Double => Kind::Fixed64,
                    Type::Fixed32 | Type::Sfixed32 | Type::Float => Kind::Fixed32,
                    Type::String => Kind::String,
                    Type::Bytes => Kind::Bytes,
                    Type::Message => Kind::Message(*index.get(field.type_name())?),
                    Type::Group => return None,
                };
                if fields.len() <= number {
                    fields.resize(number + 1, None);
                }
                if fields[number].is_some() {
                    return None;
                }
                fields[number] = Some(Field {
                    kind,
                    repeated: field.label() == Label::Repeated,
                });
            }
            messages.push(fields);
        }
        Some(Schema {
            messages,
            request: *index.get(format!("{PACKAGE}.RunFunctionRequest").as_str())?,
            response: *index.get(format!("{PACKAGE}.RunFunctionResponse").as_str())?,
        })
    }

    /// Whether buf is one whole message of this type as prost decodes it:
    /// every key well formed, every declared field in its wire type with a
    /// value that decodes (a string UTF-8, a nested message valid in turn),
    /// every other field one prost can skip.
    fn valid(&self, message: usize, buf: &[u8], depth: u32) -> bool {
        let fields = &self.messages[message];
        let mut i = 0;
        while i < buf.len() {
            let Some(key) = varint(buf, &mut i) else {
                return false;
            };
            if key > u64::from(u32::MAX) || key >> 3 == 0 {
                return false;
            }
            let wire = key & 0x7;
            let field = fields.get((key >> 3) as usize).copied().flatten();
            let ok = match (field.map(|f| (f.kind, f.repeated)), wire) {
                (None | Some((Kind::Varint, _)), 0) => varint(buf, &mut i).is_some(),
                (None | Some((Kind::Fixed64, _)), 1) => skip(buf, &mut i, 8).is_some(),
                (None | Some((Kind::Fixed32, _)), 5) => skip(buf, &mut i, 4).is_some(),
                (None | Some((Kind::Bytes, _)), 2) => delimited(buf, &mut i).is_some(),
                (Some((Kind::String, _)), 2) => {
                    delimited(buf, &mut i).is_some_and(|s| std::str::from_utf8(s).is_ok())
                }
                (Some((Kind::Message(nested), _)), 2) => {
                    depth < MAX_DEPTH
                        && delimited(buf, &mut i).is_some_and(|m| self.valid(nested, m, depth + 1))
                }
                // A repeated scalar may arrive packed: its values end to
                // end in one length-delimited field.
                (Some((Kind::Varint, true)), 2) => delimited(buf, &mut i).is_some_and(|packed| {
                    let mut j = 0;
                    while j < packed.len() {
                        if varint(packed, &mut j).is_none() {
                            return false;
                        }
                    }
                    true
                }),
                (Some((Kind::Fixed64, true)), 2) => {
                    delimited(buf, &mut i).is_some_and(|packed| packed.len() % 8 == 0)
                }
                (Some((Kind::Fixed32, true)), 2) => {
                    delimited(buf, &mut i).is_some_and(|packed| packed.len() % 4 == 0)
                }
                // A declared field in another wire type, a group, a wire
                // type that does not exist: prost's to judge.
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

/// Reads a varint at *i as prost does: at most ten bytes, the tenth
/// holding one bit.
fn varint(buf: &[u8], i: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for n in 0..10 {
        let byte = *buf.get(*i + n)?;
        if n == 9 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << (7 * n);
        if byte < 0x80 {
            *i += n + 1;
            return Some(value);
        }
    }
    None
}

fn skip(buf: &[u8], i: &mut usize, n: usize) -> Option<()> {
    let end = i.checked_add(n)?;
    if end > buf.len() {
        return None;
    }
    *i = end;
    Some(())
}

/// Reads a length-delimited value at *i: the bytes its length states,
/// which must all be there.
fn delimited<'a>(buf: &'a [u8], i: &mut usize) -> Option<&'a [u8]> {
    let len = usize::try_from(varint(buf, i)?).ok()?;
    let start = *i;
    skip(buf, i, len)?;
    Some(&buf[start..*i])
}

#[cfg(test)]
mod tests {
    use super::*;

    use function_sdk_rust::proto::v1::{
        Condition, CredentialData, MatchLabels, Requirements, ResourceSelector, Resources,
        RunFunctionRequest, RunFunctionResponse, SchemaSelector, credentials, resource_selector,
    };
    use pbjson_types::value::Kind as ValueKind;
    use pbjson_types::{ListValue, Struct, Value};

    /// A small deterministic generator: the corpus is the same on every
    /// run, so a failure names a case that fails again.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn chance(&mut self, one_in: usize) -> bool {
            self.below(one_in) == 0
        }

        fn string(&mut self) -> String {
            const WORDS: &[&str] = &[
                "",
                "a",
                "spec",
                "forProvider",
                "eu-central-1",
                "zażółć gęślą jaźń",
                "日本語",
                "with \"quotes\" and \\ slashes",
                "crossplane.io/composition-resource-name",
            ];
            let mut s = WORDS[self.below(WORDS.len())].to_string();
            if self.chance(3) {
                s.push_str(&self.next().to_string());
            }
            s
        }

        fn bytes(&mut self) -> Vec<u8> {
            (0..self.below(12)).map(|_| self.next() as u8).collect()
        }

        fn value(&mut self, depth: usize) -> Value {
            let kind = match self.below(if depth == 0 { 4 } else { 6 }) {
                0 => ValueKind::NullValue(0),
                1 => ValueKind::NumberValue(self.next() as f64 / 7.0),
                2 => ValueKind::StringValue(self.string()),
                3 => ValueKind::BoolValue(self.chance(2)),
                4 => ValueKind::StructValue(self.object(depth - 1)),
                _ => ValueKind::ListValue(ListValue {
                    values: (0..self.below(4)).map(|_| self.value(depth - 1)).collect(),
                }),
            };
            Value { kind: Some(kind) }
        }

        fn object(&mut self, depth: usize) -> Struct {
            Struct {
                fields: (0..self.below(5))
                    .map(|_| (self.string(), self.value(depth)))
                    .collect(),
            }
        }

        fn resource(&mut self) -> Resource {
            Resource {
                resource: (!self.chance(8)).then(|| self.object(3)),
                connection_details: (0..self.below(3))
                    .map(|_| (self.string(), self.bytes()))
                    .collect(),
                ready: self.below(4) as i32,
            }
        }

        fn state(&mut self) -> State {
            State {
                composite: (!self.chance(6)).then(|| self.resource()),
                resources: (0..self.below(4))
                    .map(|_| (self.string(), self.resource()))
                    .collect(),
            }
        }

        fn resources(&mut self) -> Resources {
            Resources {
                items: (0..self.below(3)).map(|_| self.resource()).collect(),
            }
        }

        fn request(&mut self) -> RunFunctionRequest {
            RunFunctionRequest {
                meta: (!self.chance(6)).then(|| RequestMeta {
                    tag: self.string(),
                    capabilities: (0..self.below(4)).map(|_| self.below(6) as i32).collect(),
                }),
                observed: (!self.chance(6)).then(|| self.state()),
                desired: (!self.chance(6)).then(|| self.state()),
                input: (!self.chance(6)).then(|| self.object(3)),
                context: (!self.chance(3)).then(|| self.object(2)),
                extra_resources: (0..self.below(2))
                    .map(|_| (self.string(), self.resources()))
                    .collect(),
                credentials: (0..self.below(3))
                    .map(|_| {
                        let source = (!self.chance(5)).then(|| {
                            credentials::Source::CredentialData(CredentialData {
                                data: (0..self.below(3))
                                    .map(|_| (self.string(), self.bytes()))
                                    .collect(),
                            })
                        });
                        (self.string(), Credentials { source })
                    })
                    .collect(),
                required_resources: (0..self.below(2))
                    .map(|_| (self.string(), self.resources()))
                    .collect(),
                required_schemas: (0..self.below(2))
                    .map(|_| {
                        let schema = function_sdk_rust::proto::v1::Schema {
                            openapi_v3: (!self.chance(3)).then(|| self.object(2)),
                        };
                        (self.string(), schema)
                    })
                    .collect(),
            }
        }

        fn selector(&mut self) -> ResourceSelector {
            ResourceSelector {
                api_version: self.string(),
                kind: self.string(),
                namespace: self.chance(2).then(|| self.string()),
                r#match: match self.below(3) {
                    0 => None,
                    1 => Some(resource_selector::Match::MatchName(self.string())),
                    _ => Some(resource_selector::Match::MatchLabels(MatchLabels {
                        labels: (0..self.below(3))
                            .map(|_| (self.string(), self.string()))
                            .collect(),
                    })),
                },
            }
        }

        fn response(&mut self) -> RunFunctionResponse {
            RunFunctionResponse {
                meta: (!self.chance(3)).then(|| ResponseMeta {
                    tag: self.string(),
                    ttl: self.chance(2).then(|| pbjson_types::Duration {
                        seconds: self.next() as i64,
                        nanos: self.next() as i32,
                    }),
                }),
                desired: (!self.chance(6)).then(|| self.state()),
                results: (0..self.below(3))
                    .map(|_| function_sdk_rust::proto::v1::Result {
                        severity: self.below(4) as i32,
                        message: self.string(),
                        reason: self.chance(2).then(|| self.string()),
                        target: self.chance(2).then(|| self.below(4) as i32),
                    })
                    .collect(),
                context: (!self.chance(3)).then(|| self.object(2)),
                requirements: (!self.chance(3)).then(|| Requirements {
                    extra_resources: (0..self.below(2))
                        .map(|_| (self.string(), self.selector()))
                        .collect(),
                    resources: (0..self.below(3))
                        .map(|_| (self.string(), self.selector()))
                        .collect(),
                    schemas: (0..self.below(2))
                        .map(|_| {
                            let selector = SchemaSelector {
                                api_version: self.string(),
                                kind: self.string(),
                            };
                            (self.string(), selector)
                        })
                        .collect(),
                }),
                conditions: (0..self.below(3))
                    .map(|_| Condition {
                        r#type: self.string(),
                        status: self.below(4) as i32,
                        reason: self.string(),
                        message: self.chance(2).then(|| self.string()),
                        target: self.chance(2).then(|| self.below(4) as i32),
                    })
                    .collect(),
                output: self.chance(2).then(|| self.object(2)),
            }
        }
    }

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn field(number: u64, wire: u64, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, number << 3 | wire);
        if wire == 2 {
            put_varint(&mut out, value.len() as u64);
        }
        out.extend_from_slice(value);
        out
    }

    /// One field of a well-formed message: its number, its wire type and
    /// its value (a length-delimited one without its length).
    struct Record<'a> {
        number: u64,
        wire: u64,
        value: &'a [u8],
    }

    impl Record<'_> {
        fn encode(&self) -> Vec<u8> {
            field(self.number, self.wire, self.value)
        }
    }

    fn records(buf: &[u8]) -> Vec<Record<'_>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            let key = varint(buf, &mut i).expect("a key");
            let start = i;
            match key & 7 {
                0 => {
                    varint(buf, &mut i).expect("a varint");
                }
                1 => i += 8,
                5 => i += 4,
                2 => {
                    let value = delimited(buf, &mut i).expect("a value");
                    out.push(Record {
                        number: key >> 3,
                        wire: 2,
                        value,
                    });
                    continue;
                }
                wire => panic!("wire type {wire}"),
            }
            out.push(Record {
                number: key >> 3,
                wire: key & 7,
                value: &buf[start..i],
            });
        }
        out
    }

    /// The same message in a spelling no encoder writes but every decoder
    /// must read: nested messages split into several occurrences of their
    /// field (which a decoder merges), fields in another order, singular
    /// fields repeated, and fields no schema here declares in between.
    /// `nested` lists the message-typed field numbers to split, each with
    /// the ones to split inside it.
    fn respell(rng: &mut Rng, buf: &[u8], nested: &[(u64, &[u64])]) -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = Vec::new();
        for record in records(buf) {
            let inner = nested.iter().find(|(n, _)| *n == record.number);
            let (Some((_, deeper)), 2) = (inner, record.wire) else {
                parts.push(record.encode());
                continue;
            };
            let deeper: Vec<(u64, &[u64])> = deeper.iter().map(|n| (*n, &[][..])).collect();
            let value = respell(rng, record.value, &deeper);
            let inner_records = records(&value);
            let cut = rng.below(inner_records.len() + 1);
            let encode =
                |rs: &[Record<'_>]| rs.iter().flat_map(Record::encode).collect::<Vec<u8>>();
            if rng.chance(2) || inner_records.is_empty() {
                parts.push(field(record.number, 2, &value));
            } else {
                parts.push(field(record.number, 2, &encode(&inner_records[..cut])));
                parts.push(field(record.number, 2, &encode(&inner_records[cut..])));
            }
        }
        // An occurrence repeated in place: a singular message field merges
        // with itself, a map entry or repeated element is written twice.
        if !parts.is_empty() && rng.chance(2) {
            let i = rng.below(parts.len());
            parts.insert(i, parts[i].clone());
        }
        // Fields of numbers no message here declares, in every wire type
        // prost skips.
        for _ in 0..rng.below(4) {
            let number = 100 + rng.below(5000) as u64;
            let unknown = match rng.below(4) {
                0 => {
                    let mut v = Vec::new();
                    put_varint(&mut v, rng.next());
                    field(number, 0, &v)
                }
                1 => field(number, 1, &rng.next().to_le_bytes()),
                2 => field(number, 5, &(rng.next() as u32).to_le_bytes()),
                _ => field(number, 2, &rng.bytes()),
            };
            parts.insert(rng.below(parts.len() + 1), unknown);
        }
        // Another order. Occurrences of one field keep theirs, since the
        // order of a field's own occurrences is what a merge reads.
        let mut order: Vec<usize> = (0..parts.len()).collect();
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i + 1));
        }
        let first_key = |part: &Vec<u8>| varint(part, &mut 0).expect("a key") >> 3;
        let mut by_field: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, part) in parts.iter().enumerate() {
            by_field.entry(first_key(part)).or_default().push(i);
        }
        let mut taken: HashMap<u64, usize> = HashMap::new();
        let mut out = Vec::new();
        for i in order {
            let number = first_key(&parts[i]);
            let nth = taken.entry(number).or_default();
            out.extend_from_slice(&parts[by_field[&number][*nth]]);
            *nth += 1;
        }
        out
    }

    /// Request fields worth splitting: meta, the two states (and the
    /// composite resource inside them), the Input and the context.
    const REQUEST_NESTED: &[(u64, &[u64])] = &[(1, &[]), (2, &[1]), (3, &[1]), (4, &[]), (5, &[])];
    const RESPONSE_NESTED: &[(u64, &[u64])] = &[(1, &[2]), (2, &[1]), (4, &[]), (5, &[])];

    /// Damage of the kinds that make bytes undecodable: a byte changed, a
    /// high bit set (an unterminated varint, a string that is no UTF-8),
    /// bytes cut, removed or added, a tail of another message.
    fn damage(rng: &mut Rng, buf: &[u8]) -> Vec<u8> {
        let mut out = buf.to_vec();
        if out.is_empty() {
            return rng.bytes();
        }
        for _ in 0..1 + rng.below(3) {
            if out.is_empty() {
                break;
            }
            let at = rng.below(out.len());
            match rng.below(8) {
                0 => out[at] = rng.next() as u8,
                1 => out[at] |= 0x80,
                2 => out[at] ^= 1 << rng.below(8),
                3 => out.truncate(at),
                4 => {
                    let end = (at + 1 + rng.below(4)).min(out.len());
                    out.drain(at..end);
                }
                5 => {
                    let extra = rng.bytes();
                    out.splice(at..at, extra);
                }
                // A wire type changed in what is likely a key.
                6 => out[at] = (out[at] & !7) | rng.below(8) as u8,
                _ => out[at] = 0xff,
            }
        }
        out
    }

    fn corpus() -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut requests = Vec::new();
        let mut responses = Vec::new();
        for _ in 0..150 {
            let request = rng.request().encode_to_vec();
            requests.push(respell(&mut rng, &request, REQUEST_NESTED));
            requests.push(request);
            let response = rng.response().encode_to_vec();
            responses.push(respell(&mut rng, &response, RESPONSE_NESTED));
            responses.push(response);
        }
        (requests, responses)
    }

    /// What the request path reads, from the whole message.
    fn read_whole(raw: &[u8]) -> Option<(Request, Desired, Result<serde_json::Value, String>)> {
        let whole = RunFunctionRequest::decode(raw).ok()?;
        let input = function_sdk_rust::request::get_input::<serde_json::Value>(&whole)
            .map_err(|e| e.to_string());
        Some((
            Request {
                meta: whole.meta,
                observed: whole.observed.map(|o| Observed {
                    composite: o.composite,
                }),
                input: whole.input,
                credentials: whole.credentials,
            },
            Desired {
                desired: whole.desired,
            },
            input,
        ))
    }

    /// The same from the partial messages.
    fn read_partial(raw: &[u8]) -> Option<(Request, Desired, Result<serde_json::Value, String>)> {
        let request = Request::decode(raw).ok()?;
        let desired = Desired::decode(raw).ok()?;
        let input = request
            .input::<serde_json::Value>()
            .map_err(|e| e.to_string());
        Some((request, desired, input))
    }

    #[test]
    fn the_schema_is_read_from_the_embedded_descriptor() {
        let s = schema().expect("a schema");
        // RunFunctionRequest declares fields 1 to 9, RunFunctionResponse 1
        // to 7; a field the SDK gains moves these with it.
        let declared = |m: usize| s.messages[m].iter().flatten().count();
        assert_eq!(declared(s.request), 9);
        assert_eq!(declared(s.response), 7);
        // The well-known types the messages hold are in the descriptor
        // too: an Input is a Struct of Values.
        let Some(Field {
            kind: Kind::Message(input),
            repeated: false,
        }) = s.messages[s.request][4]
        else {
            panic!("the Input is a message");
        };
        let Some(Field {
            kind: Kind::Message(entry),
            repeated: true,
        }) = s.messages[input][1]
        else {
            panic!("a Struct is a map");
        };
        assert!(matches!(
            s.messages[entry][1],
            Some(Field {
                kind: Kind::String,
                ..
            })
        ));
    }

    #[test]
    fn what_an_encoder_writes_is_vouched_for() {
        let (requests, responses) = corpus();
        for (i, raw) in requests.iter().enumerate() {
            assert!(RunFunctionRequest::decode(raw.as_slice()).is_ok(), "{i}");
            assert!(request_decodes(raw), "request {i}");
        }
        for (i, raw) in responses.iter().enumerate() {
            assert!(RunFunctionResponse::decode(raw.as_slice()).is_ok(), "{i}");
            assert!(response_decodes(raw), "response {i}");
        }
        // The empty message is one.
        assert!(request_decodes(b""));
        assert!(response_decodes(b""));
    }

    /// The property the request path stands on: the check never vouches
    /// for bytes prost refuses. Held over a corpus of damaged messages in
    /// which each verdict occurs often.
    #[test]
    fn nothing_prost_refuses_is_vouched_for() {
        let (requests, responses) = corpus();
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let (mut vouched, mut refused, mut left_to_prost) = (0, 0, 0);
        let mut tally = |vouches: bool, decodes: bool, what: &str, raw: &[u8]| {
            assert!(
                !vouches || decodes,
                "the check vouched for a {what} prost refuses: {}",
                raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            match (vouches, decodes) {
                (true, _) => vouched += 1,
                (false, false) => refused += 1,
                (false, true) => left_to_prost += 1,
            }
        };
        for raw in &requests {
            for _ in 0..120 {
                let damaged = damage(&mut rng, raw);
                tally(
                    request_decodes(&damaged),
                    RunFunctionRequest::decode(damaged.as_slice()).is_ok(),
                    "request",
                    &damaged,
                );
                // A request read as a response and the reverse: declared
                // fields in the wrong wire types and shapes.
                tally(
                    response_decodes(&damaged),
                    RunFunctionResponse::decode(damaged.as_slice()).is_ok(),
                    "response",
                    &damaged,
                );
            }
        }
        for raw in &responses {
            for _ in 0..120 {
                let damaged = damage(&mut rng, raw);
                tally(
                    response_decodes(&damaged),
                    RunFunctionResponse::decode(damaged.as_slice()).is_ok(),
                    "response",
                    &damaged,
                );
                tally(
                    request_decodes(&damaged),
                    RunFunctionRequest::decode(damaged.as_slice()).is_ok(),
                    "request",
                    &damaged,
                );
            }
        }
        assert!(vouched > 5_000, "vouched for {vouched}");
        assert!(refused > 20_000, "refused {refused}");
        // Bytes prost reads and the check will not vouch for are rare:
        // they cost a whole decode, never a wrong answer.
        assert!(
            left_to_prost < (vouched + refused) / 50,
            "left {left_to_prost} to prost"
        );
    }

    /// The partial messages read what the whole one does, over the corpus,
    /// its respelled half (fields split, reordered, repeated, unknown ones
    /// between them) and every damaged message that still decodes.
    #[test]
    fn a_partial_decode_reads_what_the_whole_one_does() {
        let (requests, responses) = corpus();
        let mut rng = Rng(0xda94_2042_e4dd_58b5);
        let mut compared = 0;
        let mut check = |raw: &[u8]| {
            let Some(whole) = read_whole(raw) else {
                return;
            };
            let partial = read_partial(raw).expect("what the whole message decodes");
            assert!(partial == whole, "case {compared}");
            compared += 1;
        };
        for raw in &requests {
            check(raw);
            for _ in 0..40 {
                check(&damage(&mut rng, raw));
            }
        }
        assert!(compared > 1_000, "compared {compared}");

        let mut compared = 0;
        let mut check = |raw: &[u8]| {
            let Ok(whole) = RunFunctionResponse::decode(raw) else {
                return;
            };
            let head = ResponseHead::decode(raw).expect("what the whole message decodes");
            assert!(head.meta == whole.meta, "case {compared}");
            compared += 1;
        };
        for raw in &responses {
            check(raw);
            for _ in 0..40 {
                check(&damage(&mut rng, raw));
            }
        }
        assert!(compared > 1_000, "compared {compared}");
    }

    #[test]
    fn a_respelled_request_merges_as_the_whole_message_does() {
        // The observed state in three occurrences: the composite resource's
        // metadata, then a composed resource, then the composite's spec,
        // which a decoder merges into one composite. The Input twice, its
        // module in the second; a credential written twice, the last one
        // standing; an unknown field between them.
        let object =
            |v: serde_json::Value| -> Struct { serde_json::from_value(v).expect("a Struct") };
        let state = |composite: serde_json::Value| {
            State {
                composite: Some(Resource {
                    resource: Some(object(composite)),
                    ..Default::default()
                }),
                ..Default::default()
            }
            .encode_to_vec()
        };
        let composed = State {
            resources: HashMap::from([("bucket".to_string(), Resource::default())]),
            ..Default::default()
        }
        .encode_to_vec();
        let credential = |token: &[u8]| {
            RunFunctionRequest {
                credentials: HashMap::from([(
                    "cmdb".to_string(),
                    Credentials {
                        source: Some(credentials::Source::CredentialData(CredentialData {
                            data: HashMap::from([("token".to_string(), token.to_vec())]),
                        })),
                    },
                )]),
                ..Default::default()
            }
            .encode_to_vec()
        };
        let mut raw = Vec::new();
        raw.extend(credential(b"first"));
        raw.extend(field(
            4,
            2,
            &object(serde_json::json!({"kind": "Input"})).encode_to_vec(),
        ));
        raw.extend(field(
            2,
            2,
            &state(serde_json::json!({"metadata": {"namespace": "team-a"}})),
        ));
        raw.extend(field(999, 2, b"unknown"));
        raw.extend(field(2, 2, &composed));
        raw.extend(field(
            4,
            2,
            &object(serde_json::json!({"module": {"type": "Path", "path": "fn.wasm"}}))
                .encode_to_vec(),
        ));
        raw.extend(field(2, 2, &state(serde_json::json!({"kind": "Bucket"}))));
        raw.extend(credential(b"second"));
        raw.extend(field(
            1,
            2,
            &RequestMeta {
                tag: "t".to_string(),
                ..Default::default()
            }
            .encode_to_vec(),
        ));

        assert!(request_decodes(&raw));
        let partial = read_partial(&raw).expect("decodes");
        assert_eq!(partial, read_whole(&raw).expect("decodes"));
        let request = partial.0;
        assert_eq!(request.meta.as_ref().expect("meta").tag, "t");
        let composite: serde_json::Value = serde_json::to_value(
            request
                .composite()
                .and_then(|c| c.resource.as_ref())
                .expect("composite"),
        )
        .expect("json");
        assert_eq!(
            composite,
            serde_json::json!({"metadata": {"namespace": "team-a"}, "kind": "Bucket"})
        );
        assert_eq!(
            request.input::<serde_json::Value>().expect("input"),
            serde_json::json!({"kind": "Input", "module": {"type": "Path", "path": "fn.wasm"}})
        );
        let Some(credentials::Source::CredentialData(data)) = &request.credentials["cmdb"].source
        else {
            panic!("a credential");
        };
        assert_eq!(data.data["token"], b"second");
    }

    #[test]
    fn a_fault_deep_in_a_skipped_field_is_found() {
        // A composed resource's field name that is no UTF-8: nothing on
        // the request path reads it, the partial message skips it, and
        // the whole message refuses it.
        let entry = [
            field(1, 2, &[0xff, 0xfe]),
            field(2, 2, &Value::default().encode_to_vec()),
        ]
        .concat();
        let object = field(1, 2, &entry);
        let resource = field(1, 2, &object);
        let composed = [field(1, 2, b"bucket"), field(2, 2, &resource)].concat();
        let state = field(2, 2, &composed);
        for number in [2, 3] {
            let raw = field(number, 2, &state);
            assert!(Request::decode(raw.as_slice()).is_ok());
            assert!(RunFunctionRequest::decode(raw.as_slice()).is_err());
            assert!(!request_decodes(&raw), "field {number}");
        }
        // The same in a response's desired state.
        let raw = field(2, 2, &state);
        assert!(ResponseHead::decode(raw.as_slice()).is_ok());
        assert!(RunFunctionResponse::decode(raw.as_slice()).is_err());
        assert!(!response_decodes(&raw));
    }

    #[test]
    fn a_declared_field_is_held_to_its_wire_type() {
        // The tag as a varint, the observed state as a fixed64, a number
        // Value as a varint: prost refuses each.
        let meta_tag = field(1, 2, &field(1, 0, &[1]));
        let observed = field(2, 1, &[0; 8]);
        let number = field(4, 2, &field(1, 2, &field(2, 2, &field(2, 0, &[1]))));
        for raw in [meta_tag, observed, number] {
            assert!(
                RunFunctionRequest::decode(raw.as_slice()).is_err(),
                "{raw:02x?}"
            );
            assert!(!request_decodes(&raw), "{raw:02x?}");
        }
        // A repeated enum packed and unpacked, as proto3 writes it and as
        // it may: both decode.
        let packed = field(1, 2, &field(2, 2, &[1, 2, 3]));
        let unpacked = field(1, 2, &[field(2, 0, &[1]), field(2, 0, &[2])].concat());
        for raw in [packed, unpacked] {
            let whole = RunFunctionRequest::decode(raw.as_slice()).expect("decodes");
            assert!(!whole.meta.expect("meta").capabilities.is_empty());
            assert!(request_decodes(&raw));
        }
        // A packed value cut inside a varint.
        let cut = field(1, 2, &field(2, 2, &[1, 0x80]));
        assert!(RunFunctionRequest::decode(cut.as_slice()).is_err());
        assert!(!request_decodes(&cut));
    }

    #[test]
    fn what_the_check_will_not_judge_is_left_to_prost() {
        // A group (wire types 3 and 4) under an unknown number: prost
        // skips it, the check declines, and the caller asks prost.
        let group = [field(999, 3, &[]), field(1, 0, &[1]), field(999, 4, &[])].concat();
        assert!(RunFunctionRequest::decode(group.as_slice()).is_ok());
        assert!(!request_decodes(&group));

        // An object nested past the check's depth and within prost's.
        let nested = |levels: usize| {
            let mut value = serde_json::json!("leaf");
            for _ in 0..levels {
                value = serde_json::json!({ "k": value });
            }
            let object: Struct = serde_json::from_value(value).expect("a Struct");
            field(4, 2, &object.encode_to_vec())
        };
        let shallow = nested(15);
        assert!(RunFunctionRequest::decode(shallow.as_slice()).is_ok());
        assert!(request_decodes(&shallow));
        let deep = nested(25);
        assert!(RunFunctionRequest::decode(deep.as_slice()).is_ok());
        assert!(!request_decodes(&deep));
        // Past prost's own limit both refuse.
        let deeper = nested(40);
        assert!(RunFunctionRequest::decode(deeper.as_slice()).is_err());
        assert!(!request_decodes(&deeper));
    }

    #[test]
    fn a_descriptor_the_check_cannot_follow_is_no_schema() {
        assert!(Schema::read(b"not a descriptor set").is_none());
        // A descriptor without the served messages.
        assert!(Schema::read(&FileDescriptorSet::default().encode_to_vec()).is_none());
        // One whose message holds a group.
        let mut set = FileDescriptorSet::decode(function_sdk_rust::proto::FILE_DESCRIPTOR_SET)
            .expect("the embedded descriptor");
        assert!(Schema::read(&set.encode_to_vec()).is_some());
        let message = set
            .file
            .iter_mut()
            .flat_map(|f| f.message_type.iter_mut())
            .find(|m| m.name() == "RunFunctionRequest")
            .expect("the request");
        message.field[0].set_type(Type::Group);
        assert!(Schema::read(&set.encode_to_vec()).is_none());
    }
}
