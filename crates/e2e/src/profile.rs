//! The runtime profiles (profiles/<name>/drc.yaml and policy.cedar): one
//! Function, reconfigured between suites by switching its runtimeConfigRef
//! to the profile's DeploymentRuntimeConfig, then waiting until exactly one
//! function pod runs with that config's args and is Ready. Each test binary
//! is one profile; the switch runs once per process.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use kube::api::{Patch, PatchParams};
use serde_json::json;
use tokio::sync::OnceCell;

use crate::k8s::{self, CROSSPLANE_NAMESPACE, FUNCTION_NAME, dynamic};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Default,
    Strict,
    Lifecycle,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Default => "default",
            Profile::Strict => "strict",
            Profile::Lifecycle => "lifecycle",
        }
    }

    /// The DeploymentRuntimeConfig's name.
    pub fn runtime_config(self) -> String {
        format!("e2e-{}", self.name())
    }

    pub fn drc_yaml(self) -> &'static str {
        match self {
            Profile::Default => include_str!("../profiles/default/drc.yaml"),
            Profile::Strict => include_str!("../profiles/strict/drc.yaml"),
            Profile::Lifecycle => include_str!("../profiles/lifecycle/drc.yaml"),
        }
    }

    /// The operator policy the profile mounts.
    pub fn policy(self) -> Option<&'static str> {
        match self {
            Profile::Default => Some(include_str!("../profiles/default/policy.cedar")),
            Profile::Strict => Some(include_str!("../profiles/strict/policy.cedar")),
            Profile::Lifecycle => Some(include_str!("../profiles/lifecycle/policy.cedar")),
        }
    }

    /// The runtime args the profile's config gives the container.
    pub fn args(self) -> Result<Vec<String>> {
        let drc: serde_json::Value = serde_norway::from_str(self.drc_yaml())?;
        let containers =
            drc["spec"]["deploymentTemplate"]["spec"]["template"]["spec"]["containers"]
                .as_array()
                .context("drc.yaml: containers")?;
        let runtime = containers
            .iter()
            .find(|c| c["name"] == k8s::FUNCTION_CONTAINER)
            .context("drc.yaml: no package-runtime container")?;
        Ok(runtime["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }
}

static SWITCHED: OnceCell<Profile> = OnceCell::const_new();

/// Switches the Function to profile once per process and waits for the
/// rollout; a second profile in the same process is a test-file bug.
pub async fn ensure(client: &kube::Client, profile: Profile) -> Result<()> {
    let switched = SWITCHED
        .get_or_try_init(|| async {
            switch(client, profile).await?;
            Ok::<Profile, anyhow::Error>(profile)
        })
        .await?;
    if *switched != profile {
        bail!(
            "this process already switched the function to {switched:?}; one profile per test binary"
        );
    }
    Ok(())
}

async fn switch(client: &kube::Client, profile: Profile) -> Result<()> {
    let functions = dynamic(client, "pkg.crossplane.io/v1", "Function", None);
    functions
        .patch(
            FUNCTION_NAME,
            &PatchParams::apply(k8s::FIELD_MANAGER),
            &Patch::Merge(json!({
                "metadata": {"labels": {"e2e.function-wasm.io/profile": profile.name()}},
                "spec": {"runtimeConfigRef": {"name": profile.runtime_config()}},
            })),
        )
        .await
        .with_context(|| format!("switching {FUNCTION_NAME} to profile {}", profile.name()))?;
    let want = profile.args()?;
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let pods = k8s::function_pods(client).await?;
        let matching: Vec<_> = pods
            .iter()
            .filter(|p| k8s::runtime_args(p) == want)
            .collect();
        if matching.len() == 1 && pods.len() == 1 && matching.iter().all(|p| k8s::pod_is_ready(p)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let seen: Vec<_> = pods
                .iter()
                .map(|p| {
                    format!(
                        "{} args={:?} ready={}",
                        p.metadata.name.clone().unwrap_or_default(),
                        k8s::runtime_args(p),
                        k8s::pod_is_ready(p)
                    )
                })
                .collect();
            bail!(
                "the function did not roll out profile {} in {CROSSPLANE_NAMESPACE} within 240s; pods: {seen:?}",
                profile.name()
            );
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
