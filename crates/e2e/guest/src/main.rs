//! The test module as an OCI image's process (docs/one-pager-oci-runtime.md,
//! "The process contract"): the same `run_function` the component wraps,
//! around standard input and output. Built static for musl by `make
//! e2e.oci.guest` and put into a FROM scratch image by the suite's setup;
//! a guest error is the exit status 1 with the message as the last stderr
//! line, which the runtime turns into the fatal result.

use std::io::{Read, Write};

use e2e_guest::fnv1::{RunFunctionRequest, RunFunctionResponse};
use prost::Message as _;

fn main() {
    let mut input = Vec::new();
    if let Err(e) = std::io::stdin().read_to_end(&mut input) {
        eprintln!("cannot read the request: {e}");
        std::process::exit(1);
    }
    let request = match RunFunctionRequest::decode(input.as_slice()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot decode the request: {e}");
            std::process::exit(1);
        }
    };
    // Egress goes through the host's proxy, which the OCI runtime does not
    // offer yet; the knob reports the refusal the way the component
    // reports the host's.
    let fetch = |url: String| async move {
        Err(format!("GET {url}: the OCI runtime offers no egress yet"))
    };
    let response: RunFunctionResponse = match pollster::block_on(e2e_guest::run_function(&request, fetch)) {
        Ok(r) => r,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    let out = response.encode_to_vec();
    if let Err(e) = std::io::stdout().write_all(&out).and_then(|()| std::io::stdout().flush()) {
        eprintln!("cannot write the response: {e}");
        std::process::exit(1);
    }
}
