//! Typed helpers over kube-rs: dynamic APIs by apiVersion and kind,
//! server-side apply, waits with a timeout that name what they waited for,
//! conditions, events and pod logs. Crossplane's kinds are handled as
//! `DynamicObject`s: the suite asserts on fields, never on a schema of its
//! own.

use std::fmt::Debug;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use k8s_openapi::api::core::v1::{Event, Pod};
use kube::Client;
use kube::api::{
    Api, ApiResource, DynamicObject, GroupVersionKind, ListParams, LogParams, Patch, PatchParams,
};
use serde::de::DeserializeOwned;

/// Every object the suite writes carries this manager.
pub const FIELD_MANAGER: &str = "function-wasm-e2e";
/// Where Crossplane and the function run.
pub const CROSSPLANE_NAMESPACE: &str = "crossplane-system";
/// The Function under test and the label its pods carry.
pub const FUNCTION_NAME: &str = "function-wasm";
pub const FUNCTION_POD_SELECTOR: &str = "pkg.crossplane.io/function=function-wasm";
pub const FUNCTION_CONTAINER: &str = "package-runtime";

pub fn gvk(api_version: &str, kind: &str) -> GroupVersionKind {
    let (group, version) = api_version.split_once('/').unwrap_or(("", api_version));
    GroupVersionKind::gvk(group, version, kind)
}

/// A dynamic API for kind, namespaced when namespace is given.
pub fn dynamic(
    client: &Client,
    api_version: &str,
    kind: &str,
    namespace: Option<&str>,
) -> Api<DynamicObject> {
    let resource = ApiResource::from_gvk(&gvk(api_version, kind));
    match namespace {
        Some(ns) => Api::namespaced_with(client.clone(), ns, &resource),
        None => Api::all_with(client.clone(), &resource),
    }
}

/// Server-side applies obj (which carries apiVersion, kind and metadata).
pub async fn apply(api: &Api<DynamicObject>, obj: &serde_json::Value) -> Result<DynamicObject> {
    let name = obj["metadata"]["name"]
        .as_str()
        .context("object without metadata.name")?;
    api.patch(
        name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(obj),
    )
    .await
    .with_context(|| format!("applying {} {}", obj["kind"], name))
}

/// Polls api for name until predicate holds or timeout passes; the error
/// says what was waited for and shows the object's conditions.
pub async fn wait_for<K>(
    api: &Api<K>,
    name: &str,
    timeout: Duration,
    what: &str,
    predicate: impl Fn(&K) -> bool,
) -> Result<K>
where
    K: Clone + DeserializeOwned + Debug + serde::Serialize,
{
    let deadline = Instant::now() + timeout;
    let mut last: Option<K> = None;
    loop {
        if let Some(obj) = api
            .get_opt(name)
            .await
            .with_context(|| format!("getting {name}"))?
        {
            if predicate(&obj) {
                return Ok(obj);
            }
            last = Some(obj);
        }
        if Instant::now() >= deadline {
            let seen = last
                .as_ref()
                .map(|o| {
                    let v = serde_json::to_value(o).unwrap_or_default();
                    serde_json::to_string_pretty(&v["status"]).unwrap_or_default()
                })
                .unwrap_or_else(|| "(not found)".to_string());
            bail!("{name}: {what} did not happen within {timeout:?}; last status:\n{seen}");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// A status condition of a dynamic object by type.
pub fn condition<'a>(obj: &'a DynamicObject, type_: &str) -> Option<&'a serde_json::Value> {
    obj.data["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|c| c["type"] == type_)
}

pub fn condition_is(obj: &DynamicObject, type_: &str, status: &str) -> bool {
    condition(obj, type_).is_some_and(|c| c["status"] == status)
}

pub fn condition_message(obj: &DynamicObject, type_: &str) -> String {
    condition(obj, type_)
        .and_then(|c| c["message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// The core events about an object in a namespace, oldest first.
pub async fn events_for(
    client: &Client,
    namespace: &str,
    kind: &str,
    name: &str,
) -> Result<Vec<Event>> {
    let events: Api<Event> = Api::namespaced(client.clone(), namespace);
    let list = events
        .list(&ListParams::default().fields(&format!(
            "involvedObject.kind={kind},involvedObject.name={name}"
        )))
        .await
        .with_context(|| format!("listing events for {kind}/{name}"))?;
    let mut items = list.items;
    items.sort_by(|a, b| a.last_timestamp.cmp(&b.last_timestamp));
    Ok(items)
}

/// The function pods, as the current rollout left them.
pub async fn function_pods(client: &Client) -> Result<Vec<Pod>> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), CROSSPLANE_NAMESPACE);
    Ok(pods
        .list(&ListParams::default().labels(FUNCTION_POD_SELECTOR))
        .await
        .context("listing function pods")?
        .items)
}

/// The runtime container's log of one function pod, whole.
pub async fn function_log(client: &Client, pod: &str) -> Result<String> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), CROSSPLANE_NAMESPACE);
    pods.logs(
        pod,
        &LogParams {
            container: Some(FUNCTION_CONTAINER.to_string()),
            ..Default::default()
        },
    )
    .await
    .with_context(|| format!("reading the log of {pod}"))
}

/// The args of a pod's runtime container.
pub fn runtime_args(pod: &Pod) -> Vec<String> {
    pod.spec
        .as_ref()
        .and_then(|s| s.containers.iter().find(|c| c.name == FUNCTION_CONTAINER))
        .and_then(|c| c.args.clone())
        .unwrap_or_default()
}

pub fn pod_is_ready(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod
            .status
            .as_ref()
            .and_then(|s| s.conditions.as_ref())
            .is_some_and(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
}
