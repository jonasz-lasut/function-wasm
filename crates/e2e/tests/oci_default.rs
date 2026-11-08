//! The oci default profile (docs/one-pager-oci-runtime.md): the
//! function-oci package, with /dev/kvm from the node, running the test
//! module as a process in a Firecracker VM per request under the wasm
//! default profile's policy. Every case is that profile's oracle over the
//! same Guest XR; what the OCI runtime does not
//! serve yet (egress) is refused by name.

use function_wasm_e2e::prelude::*;

const WAIT: Duration = Duration::from_secs(240);

#[tokio::test]
async fn an_image_module_composes_nop_resources() -> Result<()> {
    with_temp_ns("oci-signed", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
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
        Ok(())
    })
    .await
}

#[tokio::test]
async fn an_unsigned_image_module_is_refused() -> Result<()> {
    with_temp_ns("oci-unsigned", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-unsigned").reference());
        let guest = ns.guest("probe", &step, json!({})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(message.contains("cannot verify module"), "{message}");
        ensure!(message.contains("carries no cosign signature"), "{message}");
        Ok(())
    })
    .await
}

/// A WebAssembly artifact pointed at function-oci: refused by name, with
/// the package that serves it.
#[tokio::test]
async fn a_wasm_module_is_refused_by_name() -> Result<()> {
    with_temp_ns("oci-wasm", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.variant("plain").reference());
        let guest = ns.guest("probe", &step, json!({})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains("is a WebAssembly module (type wasm), which function-oci does not serve; function-wasm serves it"),
            "{message}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_manifest_requiring_egress_is_refused_by_name() -> Result<()> {
    with_temp_ns("oci-egress", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-egress").reference());
        let guest = ns.guest("probe", &step, json!({})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains(
                "requires egress (requires.egress.http), which function-oci does not serve yet"
            ),
            "{message}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn only_the_granted_credential_reaches_the_process() -> Result<()> {
    with_temp_ns("oci-credentials", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        ns.secret("e2e-secret", &[("token", "t-123"), ("other-key", "x")])
            .await?;
        ns.secret("other-secret", &[("password", "p")]).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-credentials").reference())
            .with_credential("e2e", "e2e-secret")
            .with_credential("other", "other-secret");
        let guest = ns
            .guest(
                "probe",
                &step,
                json!({"credentials": ["e2e", "other", "registry"]}),
            )
            .await?;
        let status = guest.wait_status(WAIT, |s| s.credentials.is_some()).await?;
        let credentials = status.credentials.expect("credentials");
        assert_eq!(credentials["e2e"], vec!["other-key", "token"]);
        assert_eq!(credentials["other"], Vec::<String>::new());
        assert_eq!(credentials["registry"], Vec::<String>::new());
        Ok(())
    })
    .await
}

#[tokio::test]
async fn an_env_binding_reaches_the_process_environment() -> Result<()> {
    with_temp_ns("oci-env", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
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
        Ok(())
    })
    .await
}

/// The VM's /tmp is the process's scratch space whether or not the
/// manifest requires it: the contract promises it.
#[tokio::test]
async fn tmp_is_writable_in_the_vm() -> Result<()> {
    with_temp_ns("oci-tmp", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        for variant in ["oci-tmp", "oci-plain"] {
            let step = Step::oci(&ns.state.oci_variant(variant).reference());
            let guest = ns
                .guest(variant, &step, json!({"privateTmp": true}))
                .await?;
            let status = guest.wait_status(WAIT, |s| s.tmp.is_some()).await?;
            let tmp = status.tmp.expect("tmp");
            ensure!(tmp.ok, "{variant}: {tmp:?}");
        }
        Ok(())
    })
    .await
}

/// A guest error is the process's exit status 1 with the message as the
/// last stderr line, which the runtime turns into the fatal result.
#[tokio::test]
async fn a_fatal_result_names_the_exit_status_and_the_message() -> Result<()> {
    with_temp_ns("oci-fatal", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-plain").reference());
        let guest = ns
            .guest("probe", &step, json!({"fatal": "asked to fail"}))
            .await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains("process exited with status 1: asked to fail"),
            "{message}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn the_deadline_stops_a_spinning_process() -> Result<()> {
    with_temp_ns("oci-spin", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-plain").reference())
            .with_limits(json!({"timeout": "5s"}));
        let guest = ns.guest("probe", &step, json!({"spin": true})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains("module exceeded its execution deadline (5s)"),
            "{message}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn the_memory_limit_is_the_vms() -> Result<()> {
    with_temp_ns("oci-grow", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-plain").reference())
            .with_limits(json!({"memory": "96Mi"}));
        let guest = ns.guest("probe", &step, json!({"growMiB": 256})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains("the run's memory limit of 96Mi was exceeded")
                || message.contains("process was killed by signal 9"),
            "{message}"
        );
        Ok(())
    })
    .await
}
