//! The lifecycle profile: debug logging, the published pdb-addon warmed at
//! startup, the caches on a volume that outlives the pod and bounded below
//! that module's size. The oracle is the function pod itself: its readiness,
//! its metrics after a restart, its log.

use function_wasm_e2e::k8s::{self, CROSSPLANE_NAMESPACE, FUNCTION_POD_SELECTOR};
use function_wasm_e2e::prelude::*;
use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{Api, DeleteParams, ListParams};

const WAIT: Duration = Duration::from_secs(180);

/// Deletes the one function pod and waits for its replacement to be Ready;
/// returns the new pod's name. The profile's readiness probe reads /readyz,
/// so Ready means the warm-up is done.
async fn restart_function(client: &Client) -> Result<String> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), CROSSPLANE_NAMESPACE);
    let old: Vec<String> = k8s::function_pods(client)
        .await?
        .iter()
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

/// The caches on the volume: after a restart the test module's compiled
/// artifact is a disk hit, while the warmed module, larger than the cache
/// bound, was swept at startup and fetched again.
#[tokio::test]
async fn caches_survive_a_restart_and_the_sweep_bounds_them() -> Result<()> {
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
        let hits_before =
            metrics::sample(&scraped, "function_wasm_module_cache_events_total", &[("cache", "compiled-disk"), ("event", "hit")])
                .unwrap_or_default();

        let second = ns.guest("second", &step, json!({})).await?;
        second.wait_ready(WAIT).await?;
        let scraped = metrics::scrape(&ns.client).await?;
        let hits_after =
            metrics::sample(&scraped, "function_wasm_module_cache_events_total", &[("cache", "compiled-disk"), ("event", "hit")])
                .unwrap_or_default();
        ensure!(
            hits_after > hits_before,
            "the compiled artifact on the volume was not reused by {pod}: {hits_before} -> {hits_after}\n{scraped}"
        );
        Ok(())
    })
    .await
}

/// --debug: the guest's debug line reaches the pod log, with the module
/// and its digest attached.
#[tokio::test]
async fn debug_logging_names_the_module() -> Result<()> {
    with_temp_ns("debug-logs", |ns| async move {
        ns.ensure_profile(Profile::Lifecycle).await?;
        let plain = ns.state.variant("plain");
        let step = Step::oci(&plain.reference());
        let guest = ns.guest("probe", &step, json!({"log": ["debug"]})).await?;
        guest.wait_status(WAIT, |s| s.logged.is_some()).await?;
        let mut lines = Vec::new();
        for pod in k8s::function_pods(&ns.client).await? {
            let name = pod.metadata.name.clone().unwrap_or_default();
            let log = k8s::function_log(&ns.client, &name).await?;
            lines.extend(
                log.lines()
                    .filter(|l| l.contains("e2e guest log line"))
                    .map(str::to_string),
            );
        }
        ensure!(!lines.is_empty(), "no guest log line in the pod log");
        ensure!(lines.iter().all(|l| l.contains("DEBUG")), "{lines:?}");
        ensure!(lines.iter().all(|l| l.contains(&plain.digest)), "{lines:?}");
        Ok(())
    })
    .await
}
