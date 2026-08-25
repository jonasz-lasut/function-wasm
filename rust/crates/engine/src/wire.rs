//! The JSON payloads of the wasmfn.http import - the contract between a
//! guest and the host, documented in docs/abi.md - and nothing else, so the
//! engine that serves the import and the policy that answers it share the
//! types without the engine depending on the policy. Field names and
//! omission match the Go runtime's `internal/egress/wire` exactly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The JSON payload a guest hands to wasmfn.http. The base64 body travels
/// as the string it arrives as; whoever performs the request decodes it.
#[derive(Debug, Default, Deserialize)]
pub struct Request {
    /// Method of the request; empty means GET.
    #[serde(default)]
    pub method: String,
    /// URL, http or https, absolute.
    #[serde(default)]
    pub url: String,
    /// Headers to send. Host, Content-Length and hop-by-hop headers are the
    /// host's to set and are dropped.
    #[serde(default)]
    pub headers: BTreeMap<String, Vec<String>>,
    /// Body bytes, base64 on the wire.
    #[serde(default)]
    pub body: String,
}

/// The JSON payload wasmfn.http returns. A request that was not performed -
/// refused by the grant or the policy, over a budget, or failed - has status
/// 0 and an error; a response from the server has its status, whatever it
/// is, and no error.
#[derive(Debug, Default, Serialize)]
pub struct Response {
    pub status: i32,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, Vec<String>>,
    /// Body bytes, base64 on the wire.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

impl Response {
    /// A request the host did not perform: status 0 and the reason.
    pub fn refusal(error: impl Into<String>) -> Self {
        Response {
            error: error.into(),
            ..Default::default()
        }
    }
}
