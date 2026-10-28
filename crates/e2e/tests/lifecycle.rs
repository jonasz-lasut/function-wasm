//! The lifecycle profile: the published pdb-addon warmed at startup, the
//! caches on a volume that outlives the pod and bounded below that module's
//! size, the trusted root fetched over TUF from the public-good instance,
//! --debug on. The oracle is the function pod itself: its readiness and its
//! metrics after a restart.

use std::sync::LazyLock;

use function_wasm_e2e::k8s::{self, CROSSPLANE_NAMESPACE, FUNCTION_POD_SELECTOR};
use function_wasm_e2e::prelude::*;
use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{Api, DeleteParams, ListParams};

const WAIT: Duration = Duration::from_secs(180);

/// The harness runs the tests in parallel, and each restarts the one
/// function pod and reads that pod's metrics: one test at a time, or one
/// restarts the pod another is measuring.
static ONE_AT_A_TIME: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

/// Deletes the one function pod and waits for its replacement to be Ready;
/// returns the new pod's name. The profile's readiness probe reads /readyz,
/// so Ready means the warm-up is done. A pod the last restart left
/// terminating is not the one.
async fn restart_function(client: &Client) -> Result<String> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), CROSSPLANE_NAMESPACE);
    let old: Vec<String> = k8s::function_pods(client)
        .await?
        .iter()
        .filter(|p| p.metadata.deletion_timestamp.is_none())
        .filter_map(|p| p.metadata.name.clone())
        .collect();
    ensure!(old.len() == 1, "one function pod expected, found {old:?}");
    pods.delete(&old[0], &DeleteParams::default()).await?;
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    loop {
        let current = pods
            .list(&ListParams::default().labels(FUNCTION_POD_SELECTOR))
            .await?;
        if let Some(pod) = current.items.iter().find(|p| {
            !old.contains(p.metadata.name.as_ref().unwrap_or(&String::new()))
                && k8s::pod_is_ready(p)
        }) {
            return Ok(pod.metadata.name.clone().unwrap_or_default());
        }
        ensure!(
            std::time::Instant::now() < deadline,
            "the function pod did not come back Ready"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Readiness waits for the warm-up: the profile's readiness probe reads
/// /readyz, so a fresh pod is Ready only once the warmed module is loaded,
/// and by then the metrics show the warm-up's work (the sweep removed the
/// module's cache entries at startup, so it was fetched and compiled again)
/// while /livez answered throughout.
#[tokio::test]
async fn readiness_waits_for_the_warm_up() -> Result<()> {
    let _serial = ONE_AT_A_TIME.lock().await;
    with_temp_ns("warm-up", |ns| async move {
        ns.ensure_profile(Profile::Lifecycle).await?;
        let pod = restart_function(&ns.client).await?;
        assert_eq!(metrics::health(&ns.client, &pod, "/livez").await?, 200);
        assert_eq!(metrics::health(&ns.client, &pod, "/readyz").await?, 200);
        let scraped = metrics::scrape(&ns.client).await?;
        for series in [
            "function_wasm_module_fetch_duration_seconds_count",
            "function_wasm_module_compile_duration_seconds_count",
        ] {
            let count = metrics::sample(&scraped, series, &[]).unwrap_or_default();
            ensure!(
                count >= 1.0,
                "{series} after the warm-up: {count}\n{scraped}"
            );
        }
        // A request after the warm-up finds the module in memory.
        let step = Step::oci_public(&ns.state.pdb_addon);
        let guest = ns.guest("warmed", &step, json!({})).await?;
        guest.wait_synced(WAIT).await?;
        Ok(())
    })
    .await
}

/// The caches on the volume: after a restart the test module's bytes are
/// a blob hit (no fetch) while its compiled artifact, written under the
/// previous process's key, is refused as stale and compiled again; the
/// warmed module, larger than the cache bound, was swept at startup and
/// fetched again.
#[tokio::test]
async fn the_volume_keeps_fetched_modules_and_the_sweep_bounds_them() -> Result<()> {
    let _serial = ONE_AT_A_TIME.lock().await;
    with_temp_ns("caches", |ns| async move {
        ns.ensure_profile(Profile::Lifecycle).await?;
        let step = Step::oci(&ns.state.variant("plain").reference());
        let first = ns.guest("first", &step, json!({})).await?;
        first.wait_ready(WAIT).await?;

        let pod = restart_function(&ns.client).await?;
        let scraped = metrics::scrape(&ns.client).await?;
        let fetched = metrics::sample(&scraped, "function_wasm_module_fetch_duration_seconds_count", &[]);
        ensure!(
            fetched.unwrap_or_default() >= 1.0,
            "after the restart the warm-up should have fetched the swept module again: {fetched:?}\n{scraped}"
        );
        let blob_hits_before =
            metrics::sample(&scraped, "function_wasm_module_cache_events_total", &[("cache", "blob"), ("event", "hit")])
                .unwrap_or_default();
        let stale_before =
            metrics::sample(&scraped, "function_wasm_module_cache_events_total", &[("cache", "compiled-disk"), ("event", "stale")])
                .unwrap_or_default();

        let second = ns.guest("second", &step, json!({})).await?;
        second.wait_ready(WAIT).await?;
        let scraped = metrics::scrape(&ns.client).await?;
        let blob_hits_after =
            metrics::sample(&scraped, "function_wasm_module_cache_events_total", &[("cache", "blob"), ("event", "hit")])
                .unwrap_or_default();
        let stale_after =
            metrics::sample(&scraped, "function_wasm_module_cache_events_total", &[("cache", "compiled-disk"), ("event", "stale")])
                .unwrap_or_default();
        ensure!(
            blob_hits_after > blob_hits_before,
            "the fetched module on the volume was not reused by {pod}: {blob_hits_before} -> {blob_hits_after}\n{scraped}"
        );
        ensure!(
            stale_after > stale_before,
            "the previous process's artifact should be refused as stale by {pod}: {stale_before} -> {stale_after}\n{scraped}"
        );
        Ok(())
    })
    .await
}

/// The trusted root over TUF: the profile mounts no root file and names
/// the public-good instance, whose root the pod fetched before it reported
/// ready (a refresh counted as updated, a last-refresh time). The published
/// pdb-addon, whose keyless signature the policy requires and only that
/// root verifies, is admitted - and after a restart the cache on the
/// volume holds the verified metadata, so the fresh pod has its root even
/// before the repository answers.
#[tokio::test]
async fn the_trusted_root_is_fetched_over_tuf() -> Result<()> {
    let _serial = ONE_AT_A_TIME.lock().await;
    with_temp_ns("tuf", |ns| async move {
        ns.ensure_profile(Profile::Lifecycle).await?;
        let refreshed = |scraped: &str| -> Result<()> {
            let updated = metrics::sample(
                scraped,
                "function_wasm_trusted_root_refreshes_total",
                &[("outcome", "updated")],
            )
            .unwrap_or_default();
            ensure!(
                updated >= 1.0,
                "the trusted root was not fetched over TUF: updated={updated}\n{scraped}"
            );
            let errors = metrics::sample(
                scraped,
                "function_wasm_trusted_root_refreshes_total",
                &[("outcome", "error")],
            )
            .unwrap_or_default();
            ensure!(
                errors == 0.0,
                "a TUF refresh failed: error={errors}\n{scraped}"
            );
            let last = metrics::sample(
                scraped,
                "function_wasm_trusted_root_last_refresh_timestamp_seconds",
                &[],
            )
            .unwrap_or_default();
            ensure!(last > 0.0, "no last-refresh time: {last}\n{scraped}");
            Ok(())
        };
        refreshed(&metrics::scrape(&ns.client).await?)?;

        let step = Step::oci_public(&ns.state.pdb_addon);
        let guest = ns.guest("tuf", &step, json!({})).await?;
        guest.wait_synced(WAIT).await?;

        restart_function(&ns.client).await?;
        refreshed(&metrics::scrape(&ns.client).await?)?;
        let again = ns.guest("tuf-again", &step, json!({})).await?;
        again.wait_synced(WAIT).await?;
        Ok(())
    })
    .await
}
