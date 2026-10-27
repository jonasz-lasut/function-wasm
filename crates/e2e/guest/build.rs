// Generates the fnv1 module from the rust scaffold's vendored crossplane
// proto: this guest is a test fixture of the repository, so it reads the
// template's copy instead of carrying a nineteenth one. prost-build shells
// out to protoc; point PROTOC at a binary if it is not on PATH.
const PROTO_DIR: &str = "../../guestfn/templates/rust/proto";

fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={PROTO_DIR}/run_function.proto");
    prost_build::compile_protos(&[format!("{PROTO_DIR}/run_function.proto")], &[PROTO_DIR])
}
