//! The Input CRD under package/wasm/input/ is generated from input.rs (crd.rs)
//! and checked in for crossplane beta validate and editors; this test
//! holds the file to the generator, and UPDATE_GOLDENS=1 re-records it,
//! like the conformance goldens.

use std::path::PathBuf;

/// Both packages ship the one Input CRD: function-wasm's under
/// package/wasm/input/, function-oci's under package/oci/input/.
#[test]
fn the_input_crd_is_what_input_rs_generates() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let want = function_wasm::crd::yaml();
    for file in [function_wasm::crd::FILE, function_wasm::crd::OCI_FILE] {
        let path = root.join(file);
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            std::fs::write(&path, &want).expect("write the CRD");
            continue;
        }
        let got = std::fs::read_to_string(&path).expect("read the CRD");
        assert_eq!(
            got,
            want,
            "{} differs from what crd::document generates; UPDATE_GOLDENS=1 cargo test -p function-wasm --test crd re-records it",
            path.display()
        );
    }
}
