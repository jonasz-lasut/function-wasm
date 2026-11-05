//! The Input CRD under package/input/ is generated from input.rs (crd.rs)
//! and checked in for crossplane beta validate and editors; this test
//! holds the file to the generator, and UPDATE_GOLDENS=1 re-records it,
//! like the conformance goldens.

use std::path::PathBuf;

#[test]
fn the_input_crd_is_what_input_rs_generates() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(function_wasm::crd::FILE);
    let want = function_wasm::crd::yaml();
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, &want).expect("write the CRD");
        return;
    }
    let got = std::fs::read_to_string(&path).expect("read the CRD");
    assert_eq!(
        got,
        want,
        "{} differs from what crd::document generates; UPDATE_GOLDENS=1 cargo test -p function-wasm --test crd re-records it",
        path.display()
    );
}
