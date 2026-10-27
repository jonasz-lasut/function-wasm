//! The strict profile: no verifier, a policy that requires a signature for
//! the plain variant and grants egress to the registry host but opens no
//! address for it, and nothing else. Every test here is a refusal an
//! operator's narrow layer produces, as Crossplane or the module shows it.

use function_wasm_e2e::prelude::*;

const WAIT: Duration = Duration::from_secs(180);

#[tokio::test]
async fn a_required_signature_cannot_be_verified_without_a_verifier() -> Result<()> {
    with_temp_ns("no-verifier", |ns| async move {
        ns.ensure_profile(Profile::Strict).await?;
        let step = Step::oci(&ns.state.variant("plain").reference());
        let guest = ns.guest("probe", &step, json!({})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains("the operator policy requires a cosign signature"),
            "{message}"
        );
        ensure!(
            message.contains("neither --cosign-key nor --cosign-trusted-root"),
            "{message}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn egress_to_a_blocked_address_is_refused_in_band() -> Result<()> {
    with_temp_ns("ssrf", |ns| async move {
        ns.ensure_profile(Profile::Strict).await?;
        let step = Step::oci(&ns.state.variant("egress").reference());
        let url = ns.state.registry_v2_url();
        let guest = ns
            .guest("probe", &step, json!({"egress": {"url": url}}))
            .await?;
        let status = guest.wait_status(WAIT, |s| s.egress.is_some()).await?;
        let egress = status.egress.expect("egress");
        // The host is admitted by the rule and the grant; its address, in
        // kind's service CIDR, is inside the default block list, and this
        // profile opens no hole.
        let error = egress.error.unwrap_or_default();
        ensure!(
            error.contains("resolves to an address the egress policy blocks"),
            "{error}"
        );
        let scraped = metrics::scrape(&ns.client).await?;
        let refused = metrics::sample(
            &scraped,
            "function_wasm_module_http_requests_total",
            &[("outcome", "refused")],
        );
        ensure!(
            refused.unwrap_or_default() >= 1.0,
            "http_requests_total{{refused}}: {refused:?}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn an_env_binding_is_refused_without_setenv() -> Result<()> {
    with_temp_ns("env-refused", |ns| async move {
        ns.ensure_profile(Profile::Strict).await?;
        ns.secret("e2e-secret", &[("token", "t-123")]).await?;
        let step =
            Step::oci(&ns.state.variant("env").reference()).with_credential("e2e", "e2e-secret");
        let guest = ns
            .guest("probe", &step, json!({"env": ["E2E_TOKEN"]}))
            .await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(message.contains("requires env"), "{message}");
        ensure!(message.contains("operator policy"), "{message}");
        ensure!(message.contains("(setEnv)"), "{message}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_required_credential_is_refused_without_spendcredential() -> Result<()> {
    with_temp_ns("credential-refused", |ns| async move {
        ns.ensure_profile(Profile::Strict).await?;
        ns.secret("e2e-secret", &[("token", "t-123")]).await?;
        let step = Step::oci(&ns.state.variant("credentials").reference())
            .with_credential("e2e", "e2e-secret");
        let guest = ns
            .guest("probe", &step, json!({"credentials": ["e2e"]}))
            .await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains(r#"requires credential "e2e""#),
            "{message}"
        );
        ensure!(message.contains("operator policy"), "{message}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_private_tmp_is_refused_without_the_grant() -> Result<()> {
    with_temp_ns("tmp-refused", |ns| async move {
        ns.ensure_profile(Profile::Strict).await?;
        let step = Step::oci(&ns.state.variant("tmp").reference());
        let guest = ns
            .guest("probe", &step, json!({"privateTmp": true}))
            .await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(message.contains("requires a private /tmp"), "{message}");
        ensure!(message.contains("operator policy"), "{message}");
        Ok(())
    })
    .await
}
