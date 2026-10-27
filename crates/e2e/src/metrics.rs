//! Reads of the function pod's own endpoints through a port-forward: the
//! metrics it serves on :8080 and the health answers on :8081.

use anyhow::{Context as _, Result};
use kube::Client;

use crate::forward::Bridge;
use crate::k8s::{CROSSPLANE_NAMESPACE, FUNCTION_POD_SELECTOR};

pub const METRICS_PORT: u16 = 8080;
pub const HEALTH_PORT: u16 = 8081;

/// The /metrics text of the running function pod.
pub async fn scrape(client: &Client) -> Result<String> {
    let bridge = Bridge::to_labelled(
        client.clone(),
        CROSSPLANE_NAMESPACE,
        FUNCTION_POD_SELECTOR,
        METRICS_PORT,
    )
    .await?;
    let url = bridge.url("http", "/metrics");
    reqwest::get(&url)
        .await
        .with_context(|| format!("GET {url}"))?
        .text()
        .await
        .context("reading /metrics")
}

/// The status code of a health path (/readyz, /livez) of one pod.
pub async fn health(client: &Client, pod: &str, path: &str) -> Result<u16> {
    let bridge = Bridge::to_pod(client.clone(), CROSSPLANE_NAMESPACE, pod, HEALTH_PORT).await?;
    let url = bridge.url("http", path);
    Ok(reqwest::get(&url)
        .await
        .with_context(|| format!("GET {url}"))?
        .status()
        .as_u16())
}

/// One sample of a metrics text: the value of the series name whose labels
/// include every pair given (the OpenMetrics and classic formats agree on
/// this line shape). None when no such series is exposed.
pub fn sample(text: &str, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix(name)?;
        let (label_set, value) = match rest.strip_prefix('{') {
            Some(rest) => {
                let (labels, value) = rest.split_once('}')?;
                (labels, value)
            }
            None if rest.starts_with(' ') => ("", rest),
            None => return None,
        };
        let have: Vec<(&str, &str)> = label_set
            .split(',')
            .filter_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                Some((k, v.trim_matches('"')))
            })
            .collect();
        if !labels.iter().all(|want| have.contains(want)) {
            return None;
        }
        value.split_whitespace().next()?.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::sample;

    #[test]
    fn reads_one_series_by_its_labels() {
        let text = "# TYPE a counter\na_total{cache=\"blob\",event=\"hit\"} 3\na_total{cache=\"blob\",event=\"miss\"} 1\nb 7\n";
        assert_eq!(sample(text, "a_total", &[("event", "miss")]), Some(1.0));
        assert_eq!(
            sample(text, "a_total", &[("cache", "blob"), ("event", "hit")]),
            Some(3.0)
        );
        assert_eq!(sample(text, "b", &[]), Some(7.0));
        assert_eq!(sample(text, "a_total", &[("event", "evict")]), None);
    }
}
