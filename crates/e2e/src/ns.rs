//! A namespace per test, Linkerd's `with_temp_ns`: created before the test
//! body, removed with everything the test wrote when the body passes, kept
//! (and its state dumped under _output/e2e/logs) when it fails. `TempNs`
//! carries the typed helpers a test needs - the pull Secret, a Composition
//! and a Guest for one module reference, the profile switch - and `Guest`
//! the waits over one composite resource.

use std::future::Future;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use k8s_openapi::api::core::v1::Namespace;
use kube::Client;
use kube::api::{Api, DeleteParams, DynamicObject, ListParams, PostParams};
use serde_json::{Value, json};

use crate::fixtures::{self, Status, Step};
use crate::k8s::{self, apply, condition_is, dynamic, wait_for};
use crate::profile::Profile;
use crate::state::State;

pub struct TempNs {
    pub client: Client,
    pub name: String,
    pub state: State,
}

/// Runs test in a fresh namespace named after it.
pub async fn with_temp_ns<F, Fut>(test: &str, body: F) -> Result<()>
where
    F: FnOnce(TempNs) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let client = Client::try_default()
        .await
        .context("connecting to the cluster (KUBECONFIG)")?;
    let state = State::load()?;
    let name = format!(
        "e2e-{}-{:06x}",
        test.replace('_', "-"),
        rand::random::<u32>() & 0xff_ffff
    );
    let namespaces: Api<Namespace> = Api::all(client.clone());
    namespaces
        .create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": {"name": name, "labels": {"e2e.function-wasm.io/suite": "true"}},
            }))?,
        )
        .await
        .with_context(|| format!("creating namespace {name}"))?;
    let ns = TempNs {
        client: client.clone(),
        name: name.clone(),
        state,
    };
    match body(ns).await {
        Ok(()) => {
            // The cluster-scoped objects a test wrote carry its namespace as
            // a label.
            for (api_version, kind) in [
                ("apiextensions.crossplane.io/v1", "Composition"),
                (
                    fixtures::ENVIRONMENT_CONFIG_API_VERSION,
                    fixtures::ENVIRONMENT_CONFIG_KIND,
                ),
            ] {
                dynamic(&client, api_version, kind, None)
                    .delete_collection(
                        &DeleteParams::default(),
                        &ListParams::default()
                            .labels(&format!("{}={name}", fixtures::NAMESPACE_LABEL)),
                    )
                    .await
                    .with_context(|| format!("deleting the {kind}s of {name}"))?;
            }
            namespaces
                .delete(&name, &DeleteParams::default())
                .await
                .with_context(|| format!("deleting namespace {name}"))?;
            Ok(())
        }
        Err(e) => {
            let dir = crate::state::output_dir().join("logs").join(&name);
            dump(&client, &name, &dir).await;
            Err(e.context(format!(
                "namespace {name} is kept for inspection; its state is under {}",
                dir.display()
            )))
        }
    }
}

/// The namespace's Guests, their NopResources and events, and the function
/// pods' logs, written for a failed test.
async fn dump(client: &Client, namespace: &str, dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    let write = |file: &str, text: String| {
        let _ = std::fs::write(dir.join(file), text);
    };
    for (api_version, kind, file) in [
        (fixtures::API_VERSION, fixtures::KIND, "guests.json"),
        (
            fixtures::NOP_API_VERSION,
            fixtures::NOP_KIND,
            "nopresources.json",
        ),
    ] {
        if let Ok(list) = dynamic(client, api_version, kind, Some(namespace))
            .list(&ListParams::default())
            .await
        {
            write(
                file,
                serde_json::to_string_pretty(&list.items).unwrap_or_default(),
            );
        }
    }
    if let Ok(events) = k8s::events_for(client, namespace, fixtures::KIND, "").await {
        write(
            "events.json",
            serde_json::to_string_pretty(&events).unwrap_or_default(),
        );
    }
    if let Ok(pods) = k8s::function_pods(client).await {
        for pod in pods {
            let Some(name) = pod.metadata.name.clone() else {
                continue;
            };
            if let Ok(log) = k8s::function_log(client, &name).await {
                write(&format!("{name}.log"), log);
            }
        }
    }
}

impl TempNs {
    /// Switches the Function to profile (once per process) before the test
    /// touches it.
    pub async fn ensure_profile(&self, profile: Profile) -> Result<()> {
        crate::profile::ensure(&self.client, profile).await
    }

    pub async fn apply(&self, obj: &Value) -> Result<DynamicObject> {
        let api_version = obj["apiVersion"].as_str().context("apiVersion")?;
        let kind = obj["kind"].as_str().context("kind")?;
        let namespaced = obj["metadata"]["namespace"].as_str();
        apply(&dynamic(&self.client, api_version, kind, namespaced), obj).await
    }

    /// The Secret the module pull reads, in this namespace.
    pub async fn pull_secret(&self) -> Result<()> {
        self.apply(&fixtures::secret(
            &self.name,
            fixtures::PULL_SECRET,
            &[
                ("username", &self.state.pull_username),
                ("password", &self.state.pull_password),
            ],
        ))
        .await?;
        Ok(())
    }

    pub async fn secret(&self, name: &str, data: &[(&str, &str)]) -> Result<()> {
        self.apply(&fixtures::secret(&self.name, name, data))
            .await?;
        Ok(())
    }

    /// A cluster-scoped EnvironmentConfig named after this namespace,
    /// removed with it.
    pub async fn environment_config(&self, data: Value) -> Result<String> {
        let name = format!("{}-env", self.name);
        self.apply(&fixtures::environment_config(&name, &self.name, data))
            .await?;
        Ok(name)
    }

    /// A Composition for step and a Guest whose spec drives the module.
    pub async fn guest(&self, name: &str, step: &Step, spec: Value) -> Result<Guest> {
        if step
            .credentials
            .iter()
            .any(|(_, secret)| secret == fixtures::PULL_SECRET)
        {
            self.pull_secret().await?;
        }
        let composition = format!("{}-{name}", self.name);
        self.apply(&fixtures::composition(&composition, &self.name, step))
            .await?;
        // Crossplane validates a revision's pipeline (every functionRef
        // installed, with the composition capability) before a composite
        // can use it; a Guest created earlier reconciles once against the
        // unknown verdict and shows it as a transient Synced=False.
        self.wait_valid_pipeline(&composition).await?;
        self.apply(&fixtures::guest(&self.name, name, &composition, spec))
            .await?;
        Ok(Guest {
            api: dynamic(
                &self.client,
                fixtures::API_VERSION,
                fixtures::KIND,
                Some(&self.name),
            ),
            nops: dynamic(
                &self.client,
                fixtures::NOP_API_VERSION,
                fixtures::NOP_KIND,
                Some(&self.name),
            ),
            client: self.client.clone(),
            namespace: self.name.clone(),
            name: name.to_string(),
        })
    }
}

impl TempNs {
    async fn wait_valid_pipeline(&self, composition: &str) -> Result<()> {
        let revisions = dynamic(
            &self.client,
            "apiextensions.crossplane.io/v1",
            "CompositionRevision",
            None,
        );
        let selector = format!("crossplane.io/composition-name={composition}");
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        loop {
            let list = revisions
                .list(&ListParams::default().labels(&selector))
                .await
                .with_context(|| format!("listing the revisions of {composition}"))?;
            if list
                .items
                .iter()
                .any(|r| condition_is(r, "ValidPipeline", "True"))
            {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                bail!("{composition}: no revision reached ValidPipeline=True within 90s");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

/// One Guest composite resource and the waits over it.
pub struct Guest {
    api: Api<DynamicObject>,
    nops: Api<DynamicObject>,
    client: Client,
    pub namespace: String,
    pub name: String,
}

impl Guest {
    pub async fn get(&self) -> Result<DynamicObject> {
        self.api
            .get(&self.name)
            .await
            .with_context(|| format!("getting guest {}", self.name))
    }

    /// Waits until Ready=True: every composed resource reconciled.
    pub async fn wait_ready(&self, timeout: Duration) -> Result<DynamicObject> {
        wait_for(&self.api, &self.name, timeout, "Ready=True", |g| {
            condition_is(g, "Ready", "True")
        })
        .await
    }

    /// Waits until Synced=True, the pipeline having run without a fatal.
    pub async fn wait_synced(&self, timeout: Duration) -> Result<DynamicObject> {
        wait_for(&self.api, &self.name, timeout, "Synced=True", |g| {
            condition_is(g, "Synced", "True")
        })
        .await
    }

    /// Waits for the module's status to satisfy predicate and returns it.
    pub async fn wait_status(
        &self,
        timeout: Duration,
        predicate: impl Fn(&Status) -> bool,
    ) -> Result<Status> {
        let obj = wait_for(&self.api, &self.name, timeout, "the module's status", |g| {
            status_of(g).is_some_and(|s| predicate(&s))
        })
        .await?;
        status_of(&obj).context("status vanished")
    }

    /// Waits for the pipeline's fatal result and returns the Synced
    /// condition's message, which carries it: how Crossplane shows a fatal
    /// result or a refusal.
    pub async fn wait_fatal(&self, timeout: Duration) -> Result<String> {
        let obj = wait_for(
            &self.api,
            &self.name,
            timeout,
            "a fatal result from the pipeline",
            |g| {
                condition_is(g, "Synced", "False")
                    && k8s::condition_message(g, "Synced").contains("returned a fatal result")
            },
        )
        .await?;
        Ok(k8s::condition_message(&obj, "Synced"))
    }

    /// The NopResources composed for this guest, by name.
    pub async fn composed_nops(&self) -> Result<Vec<DynamicObject>> {
        let list = self
            .nops
            .list(&ListParams::default().labels(&format!("crossplane.io/composite={}", self.name)))
            .await
            .context("listing composed NopResources")?;
        let mut items = list.items;
        items.sort_by(|a, b| a.metadata.name.cmp(&b.metadata.name));
        Ok(items)
    }

    /// Waits until exactly count NopResources exist for this guest.
    pub async fn wait_composed(
        &self,
        count: usize,
        timeout: Duration,
    ) -> Result<Vec<DynamicObject>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let nops = self.composed_nops().await?;
            if nops.len() == count {
                return Ok(nops);
            }
            if std::time::Instant::now() >= deadline {
                bail!(
                    "guest {}: {} NopResources composed, want {count}",
                    self.name,
                    nops.len()
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn events(&self) -> Result<Vec<k8s_openapi::api::core::v1::Event>> {
        k8s::events_for(&self.client, &self.namespace, fixtures::KIND, &self.name).await
    }
}

/// The module's status as the composite carries it, None until written.
pub fn status_of(guest: &DynamicObject) -> Option<Status> {
    let status = guest.data.get("status")?;
    status.get("module")?;
    serde_json::from_value(status.clone()).ok()
}
