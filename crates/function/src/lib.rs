//! The function-wasm runtime as a library: everything the `function`
//! binary and the `guestfn` CLI share - the Input admission, the module
//! resolver and caches, the manifest, the policy layers, the raw gRPC
//! service and the offline validator.

pub mod admission;
pub mod authz;
pub mod cache;
pub mod cosign;
pub mod egress;
pub mod egress_rules;
pub mod from;
pub mod grpc;
pub mod input;
pub mod location;
pub mod manifest;
pub mod oci;
pub mod ops;
pub mod protowire;
pub mod quantity;
pub mod resolver;
pub mod runner;
pub mod sandboxenv;
pub mod store;
pub mod validate;
