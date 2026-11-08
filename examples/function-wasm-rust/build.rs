// Generates the fnv1 module (as apiextensions.r#fn.proto.v1.rs in OUT_DIR) from the
// vendored crossplane proto. prost-build shells out to protoc; point PROTOC at
// a binary if it is not on PATH.
fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=proto/run_function.proto");
    prost_build::compile_protos(&["proto/run_function.proto"], &["proto"])
}
