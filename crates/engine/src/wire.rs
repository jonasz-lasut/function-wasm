//! The egress seam: one HTTP request a guest makes through the host and
//! its answer, as the engine's wasi:http bridge (wasihttp.rs) hands them to
//! the HttpRequester that performs them - the runtime's egress policy
//! client - and nothing else, so the engine that serves the import and the
//! policy that answers it share the types without the engine depending on
//! the policy. Field names and omission match the Go runtime's
//! `internal/egress/wire`, whose refusal strings are contract.

use std::collections::BTreeMap;
use std::time::Instant;

/// One request a guest makes. The base64 body travels as the string it
/// arrives as; whoever performs the request decodes it.
#[derive(Debug, Default)]
pub struct Request {
    /// Method of the request; empty means GET.
    pub method: String,
    /// URL, http or https, absolute.
    pub url: String,
    /// Headers to send. Host, Content-Length and hop-by-hop headers are the
    /// host's to set and are dropped.
    pub headers: BTreeMap<String, Vec<String>>,
    /// Body bytes, base64.
    pub body: String,
}

/// The answer the guest gets. A request that was not performed - refused by
/// the grant or the policy, over a budget, or failed - has status 0 and an
/// error; a response from the server has its status, whatever it is, and
/// no error.
#[derive(Debug, Default)]
pub struct Response {
    pub status: i32,
    pub headers: BTreeMap<String, Vec<String>>,
    /// Body bytes, base64.
    pub body: String,
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

/// Answers the guest's wasi:http sends for one run: the host side of the
/// module's egress grant. It never fails - whatever stops a request is a
/// Response with status 0 and an error - so a guest always gets a
/// well-formed answer and never a trap. The deadline is the run's: a
/// request never outlives it.
pub trait HttpRequester: Send + Sync {
    fn do_request(&self, req: &Request, deadline: Instant) -> Response;
}
