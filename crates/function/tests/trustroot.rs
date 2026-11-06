//! The trusted root over TUF (--cosign-trust-instance, #183) against a
//! local TUF repository (`testing::tuf`) and the in-memory Sigstore
//! instances: fetched online and then read from the cache alone, a
//! rotation taken up by a refresh with the verified-digest memo forgotten,
//! a freeze refused with the last good root kept, and a start with neither
//! network nor cache, which refuses keyless signatures by name until the
//! repository answers.

use std::sync::Arc;
use std::time::Duration;

use function_wasm::cosign::{Signer, SignerPolicy, Verifier};
use function_wasm::location::parse_oci_reference;
use function_wasm::oci::RegistryClient;
use function_wasm::testing::registry::Registry;
use function_wasm::testing::tuf::Repository;
use function_wasm::testing::{COSIGN_SIGN_PREDICATE, Shape, Sigstore, guests};
use function_wasm::trustroot::{Origin, Outcome, Refresher};
use function_wasm_engine::metrics;

const INTERVAL: Duration = Duration::from_secs(3600);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// A registry serving one module signed keyless by instance a and one by
/// instance b, under the same release identity.
struct Stage {
    addr: String,
    a: String,
    b: String,
}

fn stage(a: &Sigstore, b: &Sigstore) -> Stage {
    let wasm = guests::tiny_component();
    let signer = Signer {
        issuer: "https://token.actions.githubusercontent.com".to_string(),
        identity:
            "https://github.com/example/greeter/.github/workflows/release.yml@refs/tags/v1.0.0"
                .to_string(),
    };
    let mut registry = Registry::new();
    let digest_a = registry.module(&wasm, None, "a");
    registry.attach(
        &digest_a,
        &a.sign(&digest_a, COSIGN_SIGN_PREDICATE, &signer),
        Shape::AnnotationsOnly,
    );
    let digest_b = registry.module(&wasm, None, "b");
    registry.attach(
        &digest_b,
        &b.sign(&digest_b, COSIGN_SIGN_PREDICATE, &signer),
        Shape::AnnotationsOnly,
    );
    Stage {
        addr: registry.serve(),
        a: digest_a,
        b: digest_b,
    }
}

fn verify(stage: &Stage, verifier: &Verifier, digest: &str) -> Result<(), String> {
    let reference = parse_oci_reference(&format!("{}/example/greeter@{digest}", stage.addr))
        .expect("reference");
    verifier.verify(&RegistryClient::new(&reference, None), &reference)
}

/// The verifier --cosign-trust-instance builds: no root yet, every signer
/// trusted (the policy is not under test here).
fn awaiting_verifier() -> Arc<Verifier> {
    let policy: Arc<dyn SignerPolicy> = Arc::new(|_: &str, _: &Signer| true);
    Arc::new(
        Verifier::configure(None, None, Some("test"), Some(policy))
            .expect("configure")
            .expect("a verifier"),
    )
}

fn refreshes(outcome: &str) -> f64 {
    metrics::sample(
        "function_wasm_trusted_root_refreshes_total",
        &[("outcome", outcome)],
    )
    .unwrap_or_default()
}

#[test]
fn the_root_is_fetched_online_and_then_read_from_the_cache_alone() {
    let rt = runtime();
    let mut repo = Repository::serve();
    let cache = tempfile::tempdir().expect("tempdir");
    let source = repo.source(cache.path());
    // Nothing published yet: the walk finds no timestamp.
    assert!(rt.block_on(source.fetch()).is_err());
    let root = Sigstore::new().trusted_root_json();
    repo.publish(&root);
    assert_eq!(rt.block_on(source.fetch()).expect("fetched"), root);

    repo.set_online(false);
    let err = rt.block_on(source.fetch()).unwrap_err();
    assert!(
        err.contains("cannot refresh the TUF metadata of test from http://"),
        "{err}"
    );
    assert_eq!(rt.block_on(source.cached()).expect("cached"), root);
    assert_eq!(
        rt.block_on(source.fetch_or_cached()).expect("the cache"),
        (root.clone(), Origin::Cache)
    );

    // An empty cache has nothing that verifies.
    let empty = tempfile::tempdir().expect("tempdir");
    let err = rt.block_on(repo.source(empty.path()).cached()).unwrap_err();
    assert!(
        err.contains("the cached TUF metadata of test does not verify"),
        "{err}"
    );
    let err = rt
        .block_on(repo.source(empty.path()).fetch_or_cached())
        .unwrap_err();
    assert!(
        err.contains("cannot refresh the TUF metadata") && err.contains("; the cache: "),
        "{err}"
    );
}

#[test]
fn a_rotation_is_taken_up_by_the_refresh_and_the_memo_forgotten() {
    let rt = runtime();
    let (a, b) = (Sigstore::new(), Sigstore::other());
    let stage = stage(&a, &b);
    let mut repo = Repository::serve();
    repo.publish(&a.trusted_root_json());
    let cache = tempfile::tempdir().expect("tempdir");
    let verifier = awaiting_verifier();
    let refresher = Refresher::new(repo.source(cache.path()), Arc::clone(&verifier), INTERVAL);
    let (updated, unchanged) = (refreshes("updated"), refreshes("unchanged"));

    assert_eq!(rt.block_on(refresher.refresh()), Outcome::Updated);
    assert!(verifier.has_trusted_root());
    verify(&stage, &verifier, &stage.a).expect("a verifies against a's root");
    let err = verify(&stage, &verifier, &stage.b).unwrap_err();
    assert!(
        err.contains("keyless signature does not verify against the trusted root"),
        "{err}"
    );
    // The same root again changes nothing.
    assert_eq!(rt.block_on(refresher.refresh()), Outcome::Unchanged);

    // The rotation: the repository now serves b's root. Until the refresh
    // the verifier keeps a's - a is remembered verified, b still refused.
    repo.publish(&b.trusted_root_json());
    verify(&stage, &verifier, &stage.a).expect("remembered");
    assert!(verify(&stage, &verifier, &stage.b).is_err());

    assert_eq!(rt.block_on(refresher.refresh()), Outcome::Updated);
    verify(&stage, &verifier, &stage.b).expect("b verifies against b's root");
    let err = verify(&stage, &verifier, &stage.a).unwrap_err();
    assert!(
        err.contains("keyless signature does not verify against the trusted root"),
        "the memo must be forgotten with the root: {err}"
    );
    // The counters are the process's, shared with the other tests running
    // beside this one, so they can only have grown by at least this much.
    assert!(refreshes("updated") >= updated + 2.0);
    assert!(refreshes("unchanged") >= unchanged + 1.0);
}

#[test]
fn an_expired_timestamp_keeps_the_last_good_root() {
    let rt = runtime();
    let a = Sigstore::new();
    let stage = stage(&a, &Sigstore::other());
    let mut repo = Repository::serve();
    repo.publish(&a.trusted_root_json());
    let cache = tempfile::tempdir().expect("tempdir");
    let verifier = awaiting_verifier();
    let refresher = Refresher::new(repo.source(cache.path()), Arc::clone(&verifier), INTERVAL);
    assert_eq!(rt.block_on(refresher.refresh()), Outcome::Updated);
    let errors = refreshes("error");

    repo.expire_timestamp();
    let outcome = rt.block_on(refresher.refresh());
    let Outcome::Failed(err) = &outcome else {
        panic!("an expired timestamp must fail the refresh: {outcome:?}");
    };
    assert!(
        err.contains("cannot refresh the TUF metadata of test from http://"),
        "{err}"
    );
    assert!(refreshes("error") >= errors + 1.0);
    assert!(verifier.has_trusted_root());
    verify(&stage, &verifier, &stage.a).expect("the last good root stays in use");
}

#[test]
fn a_start_with_neither_network_nor_cache_refuses_keyless_signatures_by_name() {
    let rt = runtime();
    let a = Sigstore::new();
    let stage = stage(&a, &Sigstore::other());
    let mut repo = Repository::serve();
    repo.publish(&a.trusted_root_json());
    repo.set_online(false);
    let cache = tempfile::tempdir().expect("tempdir");
    let verifier = awaiting_verifier();
    let refresher = Refresher::new(repo.source(cache.path()), Arc::clone(&verifier), INTERVAL);

    // Before any attempt the refusal names the instance alone.
    let err = verify(&stage, &verifier, &stage.a).unwrap_err();
    assert!(
        err.ends_with(
            "keyless signature cannot be verified: the trusted root of test has not been fetched yet"
        ),
        "{err}"
    );
    let outcome = rt.block_on(refresher.refresh());
    assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
    assert!(!verifier.has_trusted_root());
    let err = verify(&stage, &verifier, &stage.a).unwrap_err();
    assert!(
        err.contains(
            "keyless signature cannot be verified: the trusted root of test has not been fetched yet (the last attempt failed: cannot refresh the TUF metadata of test from http://"
        ),
        "{err}"
    );

    // The repository answers again: the next refresh puts the root in use.
    repo.set_online(true);
    assert_eq!(rt.block_on(refresher.refresh()), Outcome::Updated);
    verify(&stage, &verifier, &stage.a).expect("a verifies once the root arrived");
}
