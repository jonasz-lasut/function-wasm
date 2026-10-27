//! The default profile: both verifiers, the registry CA trusted, a policy
//! that grants the test module everything the suite's admitted cases need.
//! Every test here is a feature the runtime admits; what Crossplane then
//! shows is the oracle.

use function_wasm_e2e::prelude::*;

const WAIT: Duration = Duration::from_secs(180);

fn module_from_policy(registry: &str) -> String {
    format!(
        r#"permit (principal, action == Action::"pullModule", resource in Repository::"{registry}/e2e");
permit (principal, action == Action::"spendCredential", resource == Credential::"registry")
when {{ context has repository && context.repository in Repository::"{registry}/e2e" }};
"#
    )
}

#[tokio::test]
async fn a_signed_module_composes_nop_resources() -> Result<()> {
    with_temp_ns("signed", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("plain").reference());
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
async fn an_unsigned_module_is_refused() -> Result<()> {
    with_temp_ns("unsigned", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("unsigned").reference());
        let guest = ns.guest("probe", &step, json!({})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(message.contains("cannot verify module"), "{message}");
        ensure!(message.contains("carries no cosign signature"), "{message}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_keyless_signature_by_a_trusted_signer_is_admitted() -> Result<()> {
    with_temp_ns("keyless-trusted", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("keyless-trusted").reference());
        let guest = ns.guest("probe", &step, json!({})).await?;
        guest.wait_synced(WAIT).await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_keyless_signature_by_an_untrusted_signer_is_refused() -> Result<()> {
    with_temp_ns("keyless-untrusted", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("keyless-untrusted").reference());
        let guest = ns.guest("probe", &step, json!({})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(message.contains(&ns.state.untrusted_identity), "{message}");
        ensure!(message.contains("is not trusted for"), "{message}");
        ensure!(message.contains("trustSigner"), "{message}");
        Ok(())
    })
    .await
}

/// The published pdb-addon, signed keyless by this repository's release
/// workflow: the real Fulcio, Rekor and timestamp authorities, verified
/// against the public-good half of the merged trusted root. The module
/// composes nothing for a Guest (no Deployment to protect), so the oracle is
/// the pipeline running at all.
#[tokio::test]
async fn a_published_keyless_signature_verifies_against_the_public_good_root() -> Result<()> {
    with_temp_ns("public-good", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci_public(&ns.state.pdb_addon);
        let guest = ns.guest("probe", &step, json!({})).await?;
        guest.wait_synced(Duration::from_secs(300)).await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn egress_reaches_the_registry_through_the_host() -> Result<()> {
    with_temp_ns("egress", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("egress").reference());
        let url = ns.state.registry_v2_url();
        let guest = ns
            .guest("probe", &step, json!({"egress": {"url": url}}))
            .await?;
        let status = guest.wait_status(WAIT, |s| s.egress.is_some()).await?;
        let egress = status.egress.expect("egress");
        // The registry asks for Basic auth: the answer proves the request
        // was made and answered, body included.
        assert_eq!(egress.error, None);
        assert_eq!(egress.status, Some(401));
        ensure!(
            egress
                .body
                .as_deref()
                .unwrap_or_default()
                .contains("UNAUTHORIZED"),
            "{egress:?}"
        );
        let scraped = metrics::scrape(&ns.client).await?;
        let ok = metrics::sample(
            &scraped,
            "function_wasm_module_http_requests_total",
            &[("outcome", "ok")],
        );
        ensure!(
            ok.unwrap_or_default() >= 1.0,
            "http_requests_total{{ok}}: {ok:?}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn egress_outside_the_rules_is_refused_in_band() -> Result<()> {
    with_temp_ns("egress-rules", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("egress").reference());
        let guest = ns
            .guest(
                "probe",
                &step,
                json!({"egress": {"url": "http://169.254.169.254/latest/meta-data/"}}),
            )
            .await?;
        let status = guest.wait_status(WAIT, |s| s.egress.is_some()).await?;
        let error = status.egress.expect("egress").error.unwrap_or_default();
        ensure!(
            error.contains("sandbox.egress: no rule admits host"),
            "{error}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn only_the_granted_credential_reaches_the_module() -> Result<()> {
    with_temp_ns("credentials", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        ns.secret("e2e-secret", &[("token", "t-123"), ("other-key", "x")])
            .await?;
        ns.secret("other-secret", &[("password", "p")]).await?;
        let step = Step::oci(&ns.state.variant("credentials").reference())
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
        // The pull credential never reaches a module.
        assert_eq!(credentials["registry"], Vec::<String>::new());
        Ok(())
    })
    .await
}

#[tokio::test]
async fn an_env_binding_delivers_the_credential_key() -> Result<()> {
    with_temp_ns("env", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        ns.secret("e2e-secret", &[("token", "t-123")]).await?;
        let step =
            Step::oci(&ns.state.variant("env").reference()).with_credential("e2e", "e2e-secret");
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

#[tokio::test]
async fn a_private_tmp_is_writable_when_granted() -> Result<()> {
    with_temp_ns("tmp", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("tmp").reference());
        let guest = ns
            .guest("probe", &step, json!({"privateTmp": true}))
            .await?;
        let status = guest.wait_status(WAIT, |s| s.tmp.is_some()).await?;
        let tmp = status.tmp.expect("tmp");
        ensure!(tmp.ok, "{tmp:?}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn without_the_requirement_tmp_is_not_writable() -> Result<()> {
    with_temp_ns("tmp-none", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("plain").reference());
        let guest = ns
            .guest("probe", &step, json!({"privateTmp": true}))
            .await?;
        let status = guest.wait_status(WAIT, |s| s.tmp.is_some()).await?;
        let tmp = status.tmp.expect("tmp");
        ensure!(!tmp.ok, "{tmp:?}");
        ensure!(tmp.error.is_some(), "{tmp:?}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn the_requirements_round_trip_resolves_an_environment_config() -> Result<()> {
    with_temp_ns("requirements", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let env = ns
            .environment_config(json!({"region": "eu-central-1", "tier": "gold"}))
            .await?;
        let step = Step::oci(&ns.state.variant("plain").reference());
        let guest = ns
            .guest(
                "probe",
                &step,
                json!({"requirements": {"environmentConfig": env}}),
            )
            .await?;
        let status = guest
            .wait_status(WAIT, |s| s.environment_config.is_some())
            .await?;
        assert_eq!(
            status.environment_config,
            Some(json!({"region": "eu-central-1", "tier": "gold"}))
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_fatal_result_shows_on_the_composite_and_its_events() -> Result<()> {
    with_temp_ns("fatal", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("plain").reference());
        let guest = ns
            .guest("probe", &step, json!({"fatal": "asked to fail"}))
            .await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(message.contains("asked to fail"), "{message}");
        let events = guest.events().await?;
        ensure!(
            events.iter().any(|e| {
                e.type_.as_deref() == Some("Warning")
                    && e.message
                        .as_deref()
                        .unwrap_or_default()
                        .contains("asked to fail")
            }),
            "no warning event carries the fatal message: {:?}",
            events.iter().map(|e| e.message.clone()).collect::<Vec<_>>()
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_run_past_the_deadline_is_fatal() -> Result<()> {
    with_temp_ns("deadline", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step =
            Step::oci(&ns.state.variant("plain").reference()).with_limits(json!({"timeout": "1s"}));
        let guest = ns.guest("probe", &step, json!({"spin": true})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains("exceeded its execution deadline"),
            "{message}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn a_growth_past_the_memory_limit_is_refused() -> Result<()> {
    with_temp_ns("memory", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::oci(&ns.state.variant("plain").reference())
            .with_limits(json!({"memory": "32Mi"}));
        let guest = ns.guest("probe", &step, json!({"growMiB": 64})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(message.contains("memory growth refused"), "{message}");
        ensure!(message.contains("(limits.memory)"), "{message}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn module_from_inside_the_fence_runs() -> Result<()> {
    with_temp_ns("from", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::from(
            "spec.module",
            false,
            &module_from_policy(&ns.state.registry),
        );
        let module =
            json!({"ref": ns.state.variant("plain").reference(), "credentials": "registry"});
        let guest = ns.guest("probe", &step, json!({"module": module})).await?;
        guest.wait_ready(WAIT).await?;
        guest.wait_composed(1, WAIT).await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn module_from_outside_the_fence_is_refused() -> Result<()> {
    with_temp_ns("from-outside", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::from(
            "spec.module",
            false,
            &module_from_policy(&ns.state.registry),
        );
        let module =
            json!({"ref": ns.state.variant("outside").reference(), "credentials": "registry"});
        let guest = ns.guest("probe", &step, json!({"module": module})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains("cannot resolve module: module.from"),
            "{message}"
        );
        ensure!(message.contains("compositionPolicy"), "{message}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn module_from_an_unsigned_module_is_refused() -> Result<()> {
    with_temp_ns("from-unsigned", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let step = Step::from(
            "spec.module",
            false,
            &module_from_policy(&ns.state.registry),
        );
        let module =
            json!({"ref": ns.state.variant("unsigned").reference(), "credentials": "registry"});
        let guest = ns.guest("probe", &step, json!({"module": module})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(message.contains("carries no cosign signature"), "{message}");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn module_from_unset_skips_the_step() -> Result<()> {
    with_temp_ns("from-empty", |ns| async move {
        ns.ensure_profile(Profile::Default).await?;
        let before = metrics::sample(
            &metrics::scrape(&ns.client).await?,
            "function_wasm_module_requests_total",
            &[("outcome", "skipped")],
        )
        .unwrap_or_default();
        let step = Step::from("spec.module", true, &module_from_policy(&ns.state.registry));
        let guest = ns.guest("probe", &step, json!({})).await?;
        guest.wait_synced(WAIT).await?;
        assert_eq!(guest.composed_nops().await?.len(), 0);
        let after = metrics::sample(
            &metrics::scrape(&ns.client).await?,
            "function_wasm_module_requests_total",
            &[("outcome", "skipped")],
        )
        .unwrap_or_default();
        ensure!(
            after > before,
            "requests_total{{skipped}} did not move: {before} -> {after}"
        );
        Ok(())
    })
    .await
}
