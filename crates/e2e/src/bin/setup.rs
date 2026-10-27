//! What `make e2e.up` runs once the kind cluster and Crossplane exist: the
//! cluster-side state every profile and test shares. In order: the e2e
//! namespace; the registry's TLS material and Basic auth, its Deployment
//! and Service; provider-nop; the test module pushed and signed under every
//! variant through a port-forward; the profiles' ConfigMaps and Secrets in
//! crossplane-system (policies, the cosign key, the merged trusted root, the
//! registry CA, the lifecycle cache volume); the Guest XRD; and the state
//! file the tests read. Idempotent: every write is a server-side apply.

use std::time::Duration;

use anyhow::{Context as _, Result};
use function_wasm_e2e::fixtures;
use function_wasm_e2e::forward::Bridge;
use function_wasm_e2e::guest::{self, VARIANTS};
use function_wasm_e2e::k8s::{self, CROSSPLANE_NAMESPACE, apply, condition_is, dynamic, wait_for};
use function_wasm_e2e::profile::Profile;
use function_wasm_e2e::registry::{self, Pusher};
use function_wasm_e2e::state::{Pushed, State};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{Api, ListParams};
use serde_json::json;

/// The published pdb-addon: signed keyless by this repository's release
/// workflow (the real keyless signature) and large enough for the
/// lifecycle profile's warm-up and cache cases to be observable.
const PDB_ADDON: &str = "ghcr.io/jonasz-lasut/wasmfn/examples/pdb-addon@sha256:f80a8364a61555f02d1429a17af9feaf0e9e8505c8fbb0d64589f84b05b14346";
const REGISTRY_IMAGE: &str = "registry:3";

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
    drop(bridge);

    step("the profiles' policies, keys and volumes in crossplane-system");
    for profile in [Profile::Default, Profile::Strict, Profile::Lifecycle] {
        if let Some(policy) = profile.policy() {
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
    apply_value(&client, &json!({
        "apiVersion": "v1", "kind": "PersistentVolumeClaim",
        "metadata": {"name": "e2e-cache", "namespace": CROSSPLANE_NAMESPACE},
        "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "1Gi"}}},
    })).await?;

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

    step("waiting for provider-nop to be healthy");
    wait_for(
        &providers,
        "provider-nop",
        Duration::from_secs(300),
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
        pdb_addon: PDB_ADDON.to_string(),
    };
    state.save()?;
    println!("==> state written to {}", State::path().display());
    Ok(())
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
