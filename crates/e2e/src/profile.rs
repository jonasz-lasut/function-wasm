//! The runtime profiles (profiles/<runtime>/<name>/drc.yaml, with a
//! policy.cedar where the profile has a policy of its own): one Function
//! per runtime, function-wasm or function-oci, reconfigured between suites
//! by switching its runtimeConfigRef to the profile's
//! DeploymentRuntimeConfig, then waiting until exactly one function pod
//! runs with that config's args and is Ready. Each test binary
//! (tests/<runtime>_<name>.rs) is one profile; the switch runs once per
//! process.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use kube::api::{Patch, PatchParams};
use serde_json::json;
use tokio::sync::OnceCell;

use crate::k8s::{self, CROSSPLANE_NAMESPACE, dynamic};

/// The runtime a profile configures: one installed Function each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Wasm,
    /// The function-oci package (docs/one-pager-oci-runtime.md): a second
    /// Function beside function-wasm, installed when the node has /dev/kvm.
    Oci,
}

impl Runtime {
    pub fn name(self) -> &'static str {
        match self {
            Runtime::Wasm => "wasm",
            Runtime::Oci => "oci",
        }
    }

    /// The Function the runtime is installed as.
    pub fn function_name(self) -> &'static str {
        match self {
            Runtime::Wasm => k8s::FUNCTION_NAME,
            Runtime::Oci => k8s::OCI_FUNCTION_NAME,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    WasmDefault,
    WasmStrict,
    WasmLifecycle,
    /// function-oci under the wasm default profile's policy and verifiers.
    OciDefault,
    /// The oci default with VM snapshots on and the published example
    /// warmed.
    OciSnapshots,
}

impl Profile {
    /// Every profile, in the order setup creates their cluster objects.
    pub const ALL: [Profile; 5] = [
        Profile::WasmDefault,
        Profile::WasmStrict,
        Profile::WasmLifecycle,
        Profile::OciDefault,
        Profile::OciSnapshots,
    ];

    pub fn runtime(self) -> Runtime {
        match self {
            Profile::WasmDefault | Profile::WasmStrict | Profile::WasmLifecycle => Runtime::Wasm,
            Profile::OciDefault | Profile::OciSnapshots => Runtime::Oci,
        }
    }

    /// The profile's name within its runtime (the directory under
    /// profiles/<runtime>/ and the test target's suffix).
    pub fn short_name(self) -> &'static str {
        match self {
            Profile::WasmDefault | Profile::OciDefault => "default",
            Profile::WasmStrict => "strict",
            Profile::WasmLifecycle => "lifecycle",
            Profile::OciSnapshots => "snapshots",
        }
    }

    /// The profile's full name, `<runtime>-<name>`: the label the switch
    /// writes and the stem of the runtime config and policy names.
    pub fn name(self) -> String {
        format!("{}-{}", self.runtime().name(), self.short_name())
    }

    /// The Function the profile reconfigures and the tests compose with.
    pub fn function_name(self) -> &'static str {
        self.runtime().function_name()
    }

    /// The label selector of that Function's pods.
    pub fn pod_selector(self) -> String {
        format!("pkg.crossplane.io/function={}", self.function_name())
    }

    /// The DeploymentRuntimeConfig's name.
    pub fn runtime_config(self) -> String {
        format!("e2e-{}", self.name())
    }

    pub fn drc_yaml(self) -> &'static str {
        match self {
            Profile::WasmDefault => include_str!("../profiles/wasm/default/drc.yaml"),
            Profile::WasmStrict => include_str!("../profiles/wasm/strict/drc.yaml"),
            Profile::WasmLifecycle => include_str!("../profiles/wasm/lifecycle/drc.yaml"),
            Profile::OciDefault => include_str!("../profiles/oci/default/drc.yaml"),
            Profile::OciSnapshots => include_str!("../profiles/oci/snapshots/drc.yaml"),
        }
    }

    /// The profile whose policy ConfigMap this profile's runtime config
    /// mounts: itself, or the wasm default for the oci profiles (their
    /// drc.yaml names e2e-policy-wasm-default). Setup writes a policy when
    /// a wanted profile mounts it, so the oci cluster needs no wasm profile.
    pub fn policy_source(self) -> Profile {
        match self {
            Profile::OciDefault | Profile::OciSnapshots => Profile::WasmDefault,
            own => own,
        }
    }

    /// The operator policy the profile owns; a profile whose runtime config
    /// mounts another profile's policy (`policy_source`) has none of its own.
    pub fn policy(self) -> Option<&'static str> {
        match self {
            Profile::WasmDefault => Some(include_str!("../profiles/wasm/default/policy.cedar")),
            Profile::WasmStrict => Some(include_str!("../profiles/wasm/strict/policy.cedar")),
            Profile::WasmLifecycle => Some(include_str!("../profiles/wasm/lifecycle/policy.cedar")),
            Profile::OciDefault | Profile::OciSnapshots => None,
        }
    }

    /// The runtime args the profile's config gives the container.
    pub fn args(self) -> Result<Vec<String>> {
        let drc: serde_json::Value = serde_saphyr::from_str(self.drc_yaml())?;
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

/// The profile this process switched to, once it has.
pub fn current() -> Option<Profile> {
    SWITCHED.get().copied()
}

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
    let function = profile.function_name();
    functions
        .patch(
            function,
            &PatchParams::apply(k8s::FIELD_MANAGER),
            &Patch::Merge(json!({
                "metadata": {"labels": {"e2e.guestfn.io/profile": profile.name()}},
                "spec": {"runtimeConfigRef": {"name": profile.runtime_config()}},
            })),
        )
        .await
        .with_context(|| format!("switching {function} to profile {}", profile.name()))?;
    let want = profile.args()?;
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let pods = k8s::function_pods_of(client, &profile.pod_selector()).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every profile mounts a policy that exists, and the one it mounts is
    /// named in its runtime config: what setup relies on to write the
    /// policies of the wanted profiles alone.
    #[test]
    fn every_profile_mounts_an_existing_policy() {
        for profile in Profile::ALL {
            let source = profile.policy_source();
            assert!(
                source.policy().is_some(),
                "{}: mounts {}'s policy, which has none",
                profile.name(),
                source.name()
            );
            let mount = format!("e2e-policy-{}", source.name());
            assert!(
                profile.drc_yaml().contains(&mount),
                "{}: drc.yaml does not name {mount}",
                profile.name()
            );
        }
    }
}
