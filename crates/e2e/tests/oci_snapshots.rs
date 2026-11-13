//! The oci snapshots profile (docs/one-pager-vm-snapshots.md): function-oci
//! with --enable-vm-snapshots and the published function-oci-go example
//! warmed. The oracle is the function pod's own snapshot counters and what
//! the restored processes compose and report.
//!
//! The counters are the pod's, summed over every module and never reset,
//! so a count says something about one test only when no other test's
//! Guests run beside it. Every test therefore begins with `enter`: the
//! tests take turns, the first one reads the counters before any Guest of
//! this binary exists (what the warm-up alone did), and each of the others
//! asserts on the difference its own Guests made.

use guestfn_e2e::k8s;
use guestfn_e2e::prelude::*;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, ListParams};
use tokio::sync::{Mutex, MutexGuard, OnceCell};

const WAIT: Duration = Duration::from_secs(240);
const PROFILE: Profile = Profile::OciSnapshots;
/// How many Guests restore side by side in the load test.
const GUESTS: usize = 8;
/// What the runtime logs when a restore fails and its run boots instead.
const RESTORE_FAILED: &str = "cannot restore the VM's snapshot";

/// One test's Guests at a time: see the module's comment.
static TURN: Mutex<()> = Mutex::const_new(());
/// The pod's metrics before this binary created a Guest.
static WARMED: OnceCell<String> = OnceCell::const_new();

/// A test's turn, held until it ends, and the pod's metrics as the warm-up
/// left them. The scrape is taken under the lock by whichever test comes
/// first, so no Guest of this binary has run before it.
async fn enter(ns: &TempNs) -> Result<(MutexGuard<'static, ()>, &'static str)> {
    ns.ensure_profile(PROFILE).await?;
    let turn = TURN.lock().await;
    let warmed = WARMED.get_or_try_init(|| scrape(ns)).await?;
    Ok((turn, warmed))
}

async fn scrape(ns: &TempNs) -> Result<String> {
    metrics::scrape_of(&ns.client, &PROFILE.pod_selector()).await
}

/// The count of one snapshot cache event: a miss is a snapshot made, a hit
/// a run restored from one.
fn snapshots(scraped: &str, event: &str) -> f64 {
    metrics::sample(
        scraped,
        "guestfn_module_cache_events_total",
        &[("cache", "snapshot"), ("event", event)],
    )
    .unwrap_or_default()
}

/// The snapshots made and the runs restored since `before` was scraped:
/// this test's own, since it holds the turn.
async fn since(ns: &TempNs, before: &str) -> Result<(f64, f64, String)> {
    let scraped = scrape(ns).await?;
    let made = snapshots(&scraped, "miss") - snapshots(before, "miss");
    let restored = snapshots(&scraped, "hit") - snapshots(before, "hit");
    Ok((made, restored, scraped))
}

/// A hit is counted before the restore is tried, so the counters cannot
/// tell a restored run from one that fell back to a boot: only the log
/// can. The log is the pod's whole life, so a failed restore fails every
/// test that ends after it.
async fn no_restore_failed(ns: &TempNs) -> Result<()> {
    for pod in k8s::function_pods_of(&ns.client, &PROFILE.pod_selector()).await? {
        let name = pod.metadata.name.context("a pod with no name")?;
        let log = k8s::function_log(&ns.client, &name).await?;
        ensure!(
            !log.contains(RESTORE_FAILED),
            "a restore failed in {name}:\n{log}"
        );
    }
    Ok(())
}

/// Waits for a ConfigMap of the namespace to hold each of the greetings.
async fn wait_greetings(ns: &TempNs, wants: &[String]) -> Result<()> {
    let configmaps: Api<ConfigMap> = Api::namespaced(ns.client.clone(), &ns.name);
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let greetings: Vec<String> = configmaps
            .list(&ListParams::default())
            .await
            .context("listing ConfigMaps")?
            .items
            .into_iter()
            .filter_map(|c| c.data?.remove("greeting"))
            .collect();
        if wants.iter().all(|want| greetings.contains(want)) {
            return Ok(());
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "no ConfigMaps hold all of {wants:?}: {greetings:?}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// The profile's readiness probe reads /readyz, so a ready pod has warmed
/// the published example: its snapshot was made before any request, and
/// the making was timed. Read from the metrics as they were before this
/// binary's first Guest, so no other test's snapshot stands in for it.
#[tokio::test]
async fn the_warm_up_makes_the_warmed_image_s_snapshot() -> Result<()> {
    with_temp_ns("snap-warm", |ns| async move {
        let (_turn, warmed) = enter(&ns).await?;
        ensure!(
            snapshots(warmed, "miss") >= 1.0,
            "no snapshot was made at warm-up\n{warmed}"
        );
        let timed = metrics::sample(
            warmed,
            "guestfn_module_snapshot_duration_seconds_count",
            &[],
        )
        .unwrap_or_default();
        ensure!(
            timed >= 1.0,
            "the snapshot's making was not timed: {timed}\n{warmed}"
        );
        Ok(())
    })
    .await
}

/// The published example runs from the snapshot the warm-up made: its
/// process composes the ConfigMap greeting the Guest, the pod counts a
/// restored run while this Guest's are the only runs, and no restore
/// failed.
#[tokio::test]
async fn the_warmed_example_runs_from_its_snapshot() -> Result<()> {
    with_temp_ns("snap-published", |ns| async move {
        let (_turn, _) = enter(&ns).await?;
        let before = scrape(&ns).await?;
        let step = Step::oci_public(&ns.state.published_oci);
        let guest = ns.guest("probe", &step, json!({})).await?;
        guest.wait_synced(WAIT).await?;
        wait_greetings(&ns, &[format!("hello {}", guest.name)]).await?;
        let (_, restored, scraped) = since(&ns, &before).await?;
        ensure!(
            restored >= 1.0,
            "no run of this Guest was restored from a snapshot\n{scraped}"
        );
        no_restore_failed(&ns).await
    })
    .await
}

/// Guests created at once reconcile together, so their runs restore the
/// warm-up's snapshot side by side. Each Guest gets its own greeting, the
/// pod counts at least one restored run per Guest, and no restore failed
/// and fell back to a boot.
#[tokio::test]
async fn guests_created_at_once_restore_the_warmed_snapshot() -> Result<()> {
    with_temp_ns("snap-load", |ns| async move {
        let (_turn, _) = enter(&ns).await?;
        let before = scrape(&ns).await?;
        let step = Step::oci_public(&ns.state.published_oci);
        let names = ns.guests_at_once("load", GUESTS, &step, json!({})).await?;
        let greetings: Vec<String> = names.iter().map(|n| format!("hello {n}")).collect();
        wait_greetings(&ns, &greetings).await?;
        let (_, restored, scraped) = since(&ns, &before).await?;
        ensure!(
            restored >= GUESTS as f64,
            "{restored} restored runs for {GUESTS} Guests\n{scraped}"
        );
        no_restore_failed(&ns).await
    })
    .await
}

/// A module the warm-up did not see: its first run makes the snapshot (or
/// boots beside the run that does), and the runs after it restore. The
/// test module composes as it does on a booted VM.
#[tokio::test]
async fn an_image_module_composes_from_restored_runs() -> Result<()> {
    with_temp_ns("snap-plain", |ns| async move {
        let (_turn, warmed) = enter(&ns).await?;
        let before = scrape(&ns).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-plain").reference());
        let guest = ns
            .guest(
                "probe",
                &step,
                json!({"nopResources": 2, "readyAfterSeconds": 1}),
            )
            .await?;
        guest.wait_ready(WAIT).await?;
        let nops = guest.wait_composed(2, WAIT).await?;
        let mut indexes: Vec<i64> = nops
            .iter()
            .filter_map(|n| n.data["spec"]["forProvider"]["fields"]["index"].as_i64())
            .collect();
        indexes.sort();
        assert_eq!(indexes, vec![0, 1]);
        let status = guest.wait_status(WAIT, |_| true).await?;
        assert_eq!(status.module, "e2e-guest");
        // Ready took several reconciles of this Guest, each a run. A pod
        // that had restored nothing when this binary began has never run
        // this image, the one test that uses it being this one: the
        // snapshot is then this Guest's to make. A pod kept from an
        // earlier run of the suite may hold it already, and restores.
        let (made, restored, scraped) = since(&ns, &before).await?;
        if snapshots(warmed, "hit") == 0.0 {
            ensure!(
                made >= 1.0,
                "the test module's snapshot was not made beside the warmed one\n{scraped}"
            );
        } else {
            ensure!(
                made + restored >= 1.0,
                "the test module's runs neither made nor restored a snapshot\n{scraped}"
            );
        }
        no_restore_failed(&ns).await
    })
    .await
}

/// What a run is granted reaches a restored process as it reaches a
/// booted one: the env binding arrives with the run's description, after
/// the restore, and nothing of it is in the snapshot.
#[tokio::test]
async fn an_env_binding_reaches_a_restored_process() -> Result<()> {
    with_temp_ns("snap-env", |ns| async move {
        let (_turn, _) = enter(&ns).await?;
        ns.secret("e2e-secret", &[("token", "t-123")]).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-env").reference())
            .with_credential("e2e", "e2e-secret");
        let guest = ns
            .guest("probe", &step, json!({"env": ["E2E_TOKEN", "E2E_MISSING"]}))
            .await?;
        let status = guest.wait_status(WAIT, |s| s.env.is_some()).await?;
        let env = status.env.expect("env");
        assert_eq!(env["E2E_TOKEN"], Some("t-123".to_string()));
        assert_eq!(env["E2E_MISSING"], None);
        no_restore_failed(&ns).await
    })
    .await
}
