//! The guestfn runtime as a library, generic over the engine: everything
//! the function-wasm and function-oci binaries and the `guestfn` CLI share -
//! the command line, the Input admission, the module resolver and caches,
//! the manifest, the policy layers, the raw gRPC service and the offline
//! validator. A binary supplies its engine through `cli::run`.

pub mod admission;
pub mod authz;
pub mod cache;
pub mod cli;
pub mod compose;
pub mod cosign;
pub mod crd;
pub mod egress;
pub mod egress_rules;
pub mod from;
pub mod grpc;
pub mod grpcmetrics;
pub mod input;
pub mod location;
pub mod logging;
pub mod manifest;
pub mod oci;
pub mod ops;
pub mod protowire;
pub mod quantity;
pub mod require;
pub mod resolver;
pub mod runner;
pub mod sandboxenv;
pub mod store;
#[cfg(any(test, feature = "testutil"))]
pub mod testing;
#[cfg(any(test, feature = "testutil"))]
pub mod testsigstore;
pub mod trustroot;
pub mod validate;
