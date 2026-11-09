//! What `make e2e.up` runs once the kind cluster and Crossplane exist: the
//! cluster-side state every profile and test shares. In order: the e2e
//! namespace; the registry's TLS material and Basic auth, its Deployment
//! and Service; provider-nop; the test module pushed and signed under every
//! variant through a port-forward; the profiles' ConfigMaps and Secrets in
//! crossplane-system (policies, the cosign key, the merged trusted root, the
//! registry CA, the lifecycle cache volume) and their
//! DeploymentRuntimeConfigs; the kvm device plugin, when the kind node has
//! /dev/kvm (the oci profiles need it; E2E_OCI=1 makes a node without it
//! a failure, E2E_OCI=0 leaves it out); the Guest XRD; and the state file the tests
//! read. E2E_WASM=0 (function-wasm left out) skips the wasm profiles'
//! runtime configs and the lifecycle cache volume, nothing the oci
//! profiles read. Idempotent: every write is a server-side apply.

use std::time::Duration;

use anyhow::{Context as _, Result};
use guestfn_e2e::fixtures;
use guestfn_e2e::forward::Bridge;
use guestfn_e2e::guest::{self, OCI_VARIANTS, VARIANTS};
use guestfn_e2e::k8s::{self, CROSSPLANE_NAMESPACE, apply, condition_is, dynamic, wait_for};
use guestfn_e2e::profile::{Profile, Runtime};
use guestfn_e2e::registry::{self, Pusher};
use guestfn_e2e::state::{Pushed, State};
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment};
use k8s_openapi::api::core::v1::{Node, Pod};
use kube::Client;
use kube::api::{Api, ListParams};
use serde_json::json;

/// The published `examples/function-wasm-go`: signed keyless by this
/// repository's `Publish Examples` workflow (the real keyless signature)
/// and large enough for the lifecycle profile's warm-up and cache cases to
/// be observable.
const PUBLISHED_WASM: &str = "ghcr.io/jonasz-lasut/guestfn/examples/wasm-go@sha256:253aadf6a20a3fa8c487fa57e1f0133d39e66093ee4b25ba4433a00247e1c159";
/// The published `examples/function-oci-go`: a module image for both
/// platforms the runtime serves, under the same signer.
const PUBLISHED_OCI: &str = "ghcr.io/jonasz-lasut/guestfn/examples/oci-go@sha256:f0b0fd646ca7270d1bb1683a86e8387ed72fff4989c1c26e7e789ea9ea99dd80";
const REGISTRY_IMAGE: &str = "registry:3";
/// The device plugin that admits the node's /dev/kvm into function-oci's
/// pod (docs/oci/install.md, "Node requirements"), and the
/// resource it advertises.
const KVM_DEVICE_PLUGIN: &str = include_str!("../../cluster/kvm-device-plugin.yaml");
const KVM_RESOURCE: &str = "devic.es/kvm";

#[tokio::main]
async fn main() -> Result<()> {
    let client = Client::try_default()
        .await
        .context("connecting to the cluster (KUBECONFIG)")?;
    let provider_nop_version =
        std::env::var("PROVIDER_NOP_VERSION").unwrap_or_else(|_| "v0.6.0".to_string());

    step("namespace e2e");
    apply_value(&client, &json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": registry::NAMESPACE}})).await?;

    step("the registry's TLS material and auth");
    // Reused across runs: the registry pod keeps serving the certificate it
    // started with, so a second run must trust the same CA.
    let tls = match existing_tls(&client).await? {
        Some(tls) => tls,
        None => {
            let tls = registry::tls()?;
            apply_value(&client, &json!({
                "apiVersion": "v1", "kind": "Secret",
                "metadata": {"name": "registry-tls", "namespace": registry::NAMESPACE},
                "type": "kubernetes.io/tls",
                "stringData": {"tls.crt": tls.cert_pem, "tls.key": tls.key_pem, "ca.crt": tls.ca_pem},
            })).await?;
            tls
        }
    };
    apply_value(
        &client,
        &json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": "registry-auth", "namespace": registry::NAMESPACE},
            "stringData": {"htpasswd": registry::HTPASSWD},
        }),
    )
    .await?;

    step("the registry");
    apply_value(&client, &registry_deployment()).await?;
    apply_value(&client, &json!({
        "apiVersion": "v1", "kind": "Service",
        "metadata": {"name": registry::SERVICE, "namespace": registry::NAMESPACE},
        "spec": {"selector": {"app": "registry"}, "ports": [{"name": "https", "port": registry::PORT, "targetPort": registry::PORT}]},
    })).await?;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), registry::NAMESPACE);
    wait_for(
        &deployments,
        registry::SERVICE,
        Duration::from_secs(180),
        "the registry to be available",
        |d| {
            d.status
                .as_ref()
                .and_then(|s| s.available_replicas)
                .unwrap_or(0)
                >= 1
        },
    )
    .await?;
    // The Deployment's new pod may still be replacing an old one.
    let registry_pod =
        wait_for_running_pod(&client, registry::NAMESPACE, registry::SELECTOR).await?;

    step(&format!("provider-nop {provider_nop_version}"));
    let providers = dynamic(&client, "pkg.crossplane.io/v1", "Provider", None);
    apply(&providers, &json!({
        "apiVersion": "pkg.crossplane.io/v1", "kind": "Provider",
        "metadata": {"name": "provider-nop"},
        "spec": {"package": format!("xpkg.crossplane.io/crossplane-contrib/provider-nop:{provider_nop_version}")},
    })).await?;

    step("pushing the test module under every variant");
    let bridge = Bridge::to_pod(
        client.clone(),
        registry::NAMESPACE,
        &registry_pod,
        registry::PORT,
    )
    .await?;
    let pusher = Pusher::new(&bridge.url("https", ""), &tls.ca_pem)?;
    let registry_address = registry::address();
    let mut variants = std::collections::BTreeMap::new();
    for variant in VARIANTS {
        let artifact = registry::module_artifact(guest::WASM, &variant.manifest_json()?);
        let digest = pusher
            .push(&variant.repository(), guest::TAG, &artifact)
            .await
            .with_context(|| format!("pushing variant {}", variant.name))?;
        if let Some(bundle) = guest::bundle(variant, &digest) {
            pusher
                .attach(
                    &variant.repository(),
                    &digest,
                    &registry::referrer(&digest, &bundle),
                )
                .await
                .with_context(|| format!("attaching the signature of variant {}", variant.name))?;
        }
        println!(
            "    {:<18} {registry_address}/{}@{digest}",
            variant.name,
            variant.repository()
        );
        variants.insert(
            variant.name.to_string(),
            Pushed {
                repository: format!("{registry_address}/{}", variant.repository()),
                digest,
            },
        );
    }
    // The plain variant once more, outside the fenced repository.
    let plain = VARIANTS
        .iter()
        .find(|v| v.name == "plain")
        .expect("plain variant");
    let outside = registry::module_artifact(guest::WASM, &plain.manifest_json()?);
    let outside_repo = format!("{}/plain", guest::OUTSIDE_REPOSITORY);
    let digest = pusher
        .push(&outside_repo, guest::TAG, &outside)
        .await
        .context("pushing the outside copy")?;
    let referrer = registry::referrer(&digest, &guest::bundle(plain, &digest).expect("signed"));
    pusher.attach(&outside_repo, &digest, &referrer).await?;
    variants.insert(
        "outside".to_string(),
        Pushed {
            repository: format!("{registry_address}/{outside_repo}"),
            digest,
        },
    );
    // The oci profile's image and artifacts, when the module as a process
    // was built (make e2e.oci.guest); the suite's other profiles need none
    // of it.
    let mut oci_variants = std::collections::BTreeMap::new();
    let mut oci_image = String::new();
    if let Some(path) = std::env::var_os("E2E_OCI_GUEST") {
        step(
            "pushing the test module as an image, then with every oci variant's manifest appended",
        );
        let binary = std::fs::read(&path)
            .with_context(|| format!("reading E2E_OCI_GUEST {}", path.to_string_lossy()))?;
        let image = registry::image(&binary, guest::OCI_PROCESS_PATH, "65532:65532");
        let digest = pusher
            .push(guest::OCI_IMAGE_REPOSITORY, guest::TAG, &image)
            .await
            .context("pushing the test image")?;
        oci_image = format!(
            "{registry_address}/{}@{digest}",
            guest::OCI_IMAGE_REPOSITORY
        );
        println!("    {:<18} {oci_image}", "image");
        for variant in OCI_VARIANTS {
            let artifact =
                registry::module_image(&image, &variant.manifest_json_for(Some(&oci_image))?);
            let digest = pusher
                .push(&variant.repository(), guest::TAG, &artifact)
                .await
                .with_context(|| format!("pushing variant {}", variant.name))?;
            if let Some(bundle) = guest::bundle(variant, &digest) {
                pusher
                    .attach(
                        &variant.repository(),
                        &digest,
                        &registry::referrer(&digest, &bundle),
                    )
                    .await
                    .with_context(|| {
                        format!("attaching the signature of variant {}", variant.name)
                    })?;
            }
            println!(
                "    {:<18} {registry_address}/{}@{digest}",
                variant.name,
                variant.repository()
            );
            oci_variants.insert(
                variant.name.to_string(),
                Pushed {
                    repository: format!("{registry_address}/{}", variant.repository()),
                    digest,
                },
            );
        }
    }
    drop(bridge);

    // The kind node's /dev/kvm decides whether the oci profiles can run:
    // the plugin advertises it and the oci runtime config joins the node's
    // kvm group, the operator page's recipe for a hosted cluster. E2E_OCI
    // (the Makefile's knob) is auto, 1 or 0: by the node, required, or
    // left out.
    let oci_mode = std::env::var("E2E_OCI").unwrap_or_else(|_| "auto".to_string());
    let kvm_gid = if oci_mode == "0" {
        println!("    function-oci is left out (E2E_OCI=0)");
        None
    } else {
        kind_node_kvm_gid()
    };
    match kvm_gid {
        Some(gid) => {
            step(&format!(
                "the kvm device plugin (the node's /dev/kvm is group {gid})"
            ));
            install_kvm_device_plugin(&client).await?;
        }
        None if oci_mode == "1" => {
            anyhow::bail!("the kind node has no /dev/kvm and E2E_OCI=1 asks for it")
        }
        None if oci_mode != "0" => println!(
            "    the kind node has no /dev/kvm: function-oci stays uninstalled, the oci profiles are skipped"
        ),
        None => {}
    }

    // E2E_WASM (the Makefile's other knob) is 1 or 0: whether function-wasm
    // is installed, so whether the wasm profiles are wanted. Every variant
    // was pushed either way (the oci suite's wrong-type case names the
    // plain wasm one), and a policy is written when a wanted profile mounts
    // it (the oci default mounts the wasm default's).
    let wasm_wanted = std::env::var("E2E_WASM").unwrap_or_else(|_| "1".to_string()) == "1";
    if !wasm_wanted {
        println!(
            "    function-wasm is left out (E2E_WASM=0): the wasm profiles' runtime configs and cache volume are skipped"
        );
    }
    let wanted = |profile: Profile| wasm_wanted || profile.runtime() != Runtime::Wasm;

    step("the profiles' policies, keys and volumes in crossplane-system");
    for profile in Profile::ALL {
        let mounted = Profile::ALL
            .iter()
            .any(|p| wanted(*p) && p.policy_source() == profile);
        if mounted && let Some(policy) = profile.policy() {
            apply_value(&client, &json!({
                "apiVersion": "v1", "kind": "ConfigMap",
                "metadata": {"name": format!("e2e-policy-{}", profile.name()), "namespace": CROSSPLANE_NAMESPACE},
                "data": {"policy.cedar": policy},
            })).await?;
        }
    }
    apply_value(
        &client,
        &json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": "e2e-cosign-key", "namespace": CROSSPLANE_NAMESPACE},
            "stringData": {"cosign.pub": guest::key().public_pem},
        }),
    )
    .await?;
    apply_value(
        &client,
        &json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "e2e-trusted-root", "namespace": CROSSPLANE_NAMESPACE},
            "data": {"trusted_root.json": String::from_utf8(guest::merged_trusted_root()?)?},
        }),
    )
    .await?;
    apply_value(
        &client,
        &json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "e2e-registry-ca", "namespace": CROSSPLANE_NAMESPACE},
            "data": {"ca.crt": tls.ca_pem},
        }),
    )
    .await?;
    // The lifecycle profile's cache volume.
    if wasm_wanted {
        apply_value(&client, &json!({
            "apiVersion": "v1", "kind": "PersistentVolumeClaim",
            "metadata": {"name": "e2e-cache", "namespace": CROSSPLANE_NAMESPACE},
            "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}},
        })).await?;
    }

    step("the profiles' runtime configs");
    let drcs = dynamic(
        &client,
        "pkg.crossplane.io/v1beta1",
        "DeploymentRuntimeConfig",
        None,
    );
    for profile in Profile::ALL.into_iter().filter(|p| wanted(*p)) {
        let mut drc: serde_json::Value = serde_saphyr::from_str(profile.drc_yaml())
            .with_context(|| format!("{}: drc.yaml", profile.name()))?;
        if profile.runtime() == Runtime::Oci
            && let Some(gid) = kvm_gid
        {
            // The device node the kubelet admits keeps the node's owner and
            // mode (root:kvm 0660), and the pod's user is the package's.
            drc["spec"]["deploymentTemplate"]["spec"]["template"]["spec"]["securityContext"] =
                json!({"supplementalGroups": [gid]});
        }
        apply(&drcs, &drc).await?;
    }

    step("the Guest XRD");
    let xrds = dynamic(
        &client,
        "apiextensions.crossplane.io/v2",
        "CompositeResourceDefinition",
        None,
    );
    apply(&xrds, &fixtures::xrd()).await?;
    wait_for(
        &xrds,
        fixtures::XRD_NAME,
        Duration::from_secs(120),
        "Established=True",
        |x| condition_is(x, "Established", "True"),
    )
    .await?;

    // Ten minutes: the package manager pulls provider-nop from
    // xpkg.crossplane.io on the runner, and a slow pull has taken five.
    step("waiting for provider-nop to be healthy");
    wait_for(
        &providers,
        "provider-nop",
        Duration::from_secs(600),
        "Healthy=True",
        |p| condition_is(p, "Healthy", "True"),
    )
    .await?;

    let state = State {
        registry: registry_address,
        pull_username: registry::USERNAME.to_string(),
        pull_password: registry::PASSWORD.to_string(),
        variants,
        trusted_issuer: guest::TRUSTED_ISSUER.to_string(),
        trusted_identity: guest::TRUSTED_IDENTITY.to_string(),
        untrusted_identity: guest::UNTRUSTED_IDENTITY.to_string(),
        published_wasm: PUBLISHED_WASM.to_string(),
        published_oci: PUBLISHED_OCI.to_string(),
        oci_variants,
        oci_image,
        kvm: kvm_gid.is_some(),
    };
    state.save()?;
    println!("==> state written to {}", State::path().display());
    Ok(())
}

/// The gid of the kind node's /dev/kvm, read with docker exec on the node
/// container (KIND_CLUSTER_NAME, the Makefile's default otherwise); None
/// when the node has no such device, as on a host without KVM.
fn kind_node_kvm_gid() -> Option<u32> {
    let cluster =
        std::env::var("KIND_CLUSTER_NAME").unwrap_or_else(|_| "function-wasm-e2e".to_string());
    let out = std::process::Command::new("docker")
        .args([
            "exec",
            &format!("{cluster}-control-plane"),
            "stat",
            "-c",
            "%g",
            "/dev/kvm",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Applies the device plugin's DaemonSet and waits until the node
/// allocates the kvm resource; a node that never does prints its
/// allocatable map, so the plugin's log is the next place to look.
async fn install_kvm_device_plugin(client: &Client) -> Result<()> {
    let objects: Vec<serde_json::Value> =
        serde_saphyr::from_str_multiple(KVM_DEVICE_PLUGIN).context("kvm-device-plugin.yaml")?;
    for obj in objects.iter().filter(|o| !o.is_null()) {
        apply_value(client, obj).await?;
    }
    let daemonsets: Api<DaemonSet> = Api::namespaced(client.clone(), "kube-system");
    wait_for(
        &daemonsets,
        "generic-device-plugin",
        Duration::from_secs(120),
        "the kvm device plugin to be ready on the node",
        |d| {
            d.status
                .as_ref()
                .map(|s| s.number_ready >= 1)
                .unwrap_or(false)
        },
    )
    .await?;
    let nodes: Api<Node> = Api::all(client.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let list = nodes.list(&ListParams::default()).await?;
        let allocatable = list
            .items
            .first()
            .and_then(|n| n.status.as_ref())
            .and_then(|s| s.allocatable.clone())
            .unwrap_or_default();
        let count = allocatable
            .get(KVM_RESOURCE)
            .map(|q| q.0.as_str())
            .unwrap_or("0");
        if count != "0" {
            println!("    the node allocates {count} {KVM_RESOURCE}");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "the node allocates no {KVM_RESOURCE} after 120s; allocatable: {}",
                serde_json::to_string(&allocatable)?
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn step(what: &str) {
    println!("==> {what}");
}

/// The TLS material an earlier run left in the registry-tls Secret, if any.
async fn existing_tls(client: &Client) -> Result<Option<registry::Tls>> {
    let secrets: Api<k8s_openapi::api::core::v1::Secret> =
        Api::namespaced(client.clone(), registry::NAMESPACE);
    let Some(secret) = secrets.get_opt("registry-tls").await? else {
        return Ok(None);
    };
    let data = secret.data.unwrap_or_default();
    let text = |key: &str| -> Result<String> {
        let bytes = data
            .get(key)
            .with_context(|| format!("registry-tls has no {key}"))?;
        Ok(String::from_utf8(bytes.0.clone())?)
    };
    Ok(Some(registry::Tls {
        ca_pem: text("ca.crt")?,
        cert_pem: text("tls.crt")?,
        key_pem: text("tls.key")?,
    }))
}

async fn apply_value(client: &Client, obj: &serde_json::Value) -> Result<()> {
    let api_version = obj["apiVersion"].as_str().context("apiVersion")?;
    let kind = obj["kind"].as_str().context("kind")?;
    let namespace = obj["metadata"]["namespace"].as_str();
    apply(&dynamic(client, api_version, kind, namespace), obj).await?;
    Ok(())
}

async fn wait_for_running_pod(client: &Client, namespace: &str, selector: &str) -> Result<String> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let list = pods.list(&ListParams::default().labels(selector)).await?;
        if let Some(pod) = list.items.iter().find(|p| k8s::pod_is_ready(p)) {
            return Ok(pod.metadata.name.clone().expect("pod name"));
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("no ready pod {selector} in {namespace} within 120s");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// `registry:3` with TLS and Basic auth from the Secrets above.
fn registry_deployment() -> serde_json::Value {
    json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": registry::SERVICE, "namespace": registry::NAMESPACE},
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": "registry"}},
            "template": {
                "metadata": {"labels": {"app": "registry"}},
                "spec": {
                    "containers": [{
                        "name": "registry",
                        "image": REGISTRY_IMAGE,
                        "ports": [{"containerPort": registry::PORT}],
                        "env": [
                            {"name": "REGISTRY_HTTP_ADDR", "value": format!(":{}", registry::PORT)},
                            {"name": "REGISTRY_HTTP_TLS_CERTIFICATE", "value": "/certs/tls.crt"},
                            {"name": "REGISTRY_HTTP_TLS_KEY", "value": "/certs/tls.key"},
                            {"name": "REGISTRY_AUTH", "value": "htpasswd"},
                            {"name": "REGISTRY_AUTH_HTPASSWD_REALM", "value": "e2e"},
                            {"name": "REGISTRY_AUTH_HTPASSWD_PATH", "value": "/auth/htpasswd"},
                        ],
                        "volumeMounts": [
                            {"name": "tls", "mountPath": "/certs", "readOnly": true},
                            {"name": "auth", "mountPath": "/auth", "readOnly": true},
                        ],
                        "readinessProbe": {"tcpSocket": {"port": registry::PORT}, "periodSeconds": 2},
                    }],
                    "volumes": [
                        {"name": "tls", "secret": {"secretName": "registry-tls"}},
                        {"name": "auth", "secret": {"secretName": "registry-auth"}},
                    ],
                },
            },
        },
    })
}
