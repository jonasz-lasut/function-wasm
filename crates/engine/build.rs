// Embeds the wasmtime crate version the engine links, so the compiled-cache
// namespace changes with every wasmtime bump without anyone remembering to -
// the Rust counterpart of the Go engine's debug.ReadBuildInfo version.
fn main() {
    let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
        .unwrap_or_default();
    let mut version = String::from("unknown");
    let mut in_wasmtime = false;
    for line in lock.lines() {
        if line.trim() == "[[package]]" {
            in_wasmtime = false;
        }
        if line.trim() == "name = \"wasmtime\"" {
            in_wasmtime = true;
        } else if in_wasmtime && let Some(v) = line.trim().strip_prefix("version = \"") {
            version = v.trim_end_matches('"').to_string();
            break;
        }
    }
    println!("cargo:rustc-env=FUNCTION_WASM_WASMTIME_VERSION={version}");
    println!("cargo:rerun-if-changed=../../Cargo.lock");
}
