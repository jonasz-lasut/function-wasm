//! The oci privileges suite (docs/oci/modules.md, "The contract"): what an
//! image's process may do in its VM, on the oci default profile. The
//! runtime refuses an image that runs as root, and the init starts the
//! process with every capability set emptied and no_new_privs set. The
//! oracle is what the test module reports of its own attempts: each
//! operation a capability would allow is denied, and the setuid-root copy
//! of its binary in the image runs as the image's user.

use std::collections::BTreeMap;

use guestfn_e2e::guest::OCI_USER;
use guestfn_e2e::prelude::*;

const WAIT: Duration = Duration::from_secs(240);
/// A capability set with nothing in it, as /proc/self/status prints one.
const NONE: &str = "0000000000000000";

/// An image that sets no user would run its process as root: refused at
/// load from the image config, before a VM boots.
#[tokio::test]
async fn an_image_that_runs_as_root_is_refused() -> Result<()> {
    with_temp_ns("oci-root", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-root").reference());
        let guest = ns.guest("probe", &step, json!({})).await?;
        let message = guest.wait_fatal(WAIT).await?;
        ensure!(
            message.contains("image runs as root (its config sets no user)"),
            "{message}"
        );
        Ok(())
    })
    .await
}

#[tokio::test]
async fn the_process_holds_no_capability() -> Result<()> {
    with_temp_ns("oci-capabilities", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-plain").reference());
        let guest = ns
            .guest("probe", &step, json!({"privileges": true}))
            .await?;
        let status = guest.wait_status(WAIT, |s| s.privileges.is_some()).await?;
        let privileges = status.privileges.expect("privileges");
        let uid = OCI_USER.split(':').next().expect("a user");
        ensure!(privileges.uid.to_string() == uid, "{privileges:?}");
        let want: BTreeMap<String, String> = [
            ("CapInh", NONE),
            ("CapPrm", NONE),
            ("CapEff", NONE),
            ("CapBnd", NONE),
            ("CapAmb", NONE),
            ("NoNewPrivs", "1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        ensure!(
            privileges.capabilities == want,
            "{:?}",
            privileges.capabilities
        );
        Ok(())
    })
    .await
}

/// One operation and more per capability the attempts name: the kernel
/// denies every one of them.
#[tokio::test]
async fn privileged_operations_are_denied() -> Result<()> {
    with_temp_ns("oci-denied", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-plain").reference());
        let guest = ns
            .guest("probe", &step, json!({"privileges": true}))
            .await?;
        let status = guest.wait_status(WAIT, |s| s.privileges.is_some()).await?;
        let attempts = status.privileges.expect("privileges").attempts;
        ensure!(attempts.len() == 15, "{attempts:?}");
        let not_denied: Vec<_> = attempts.iter().filter(|(_, v)| *v != "denied").collect();
        ensure!(not_denied.is_empty(), "{not_denied:?}");
        Ok(())
    })
    .await
}

/// The image holds a copy of the binary owned by root with the setuid bit:
/// under no_new_privs it runs as the image's user all the same.
#[tokio::test]
async fn a_setuid_root_binary_gains_nothing() -> Result<()> {
    with_temp_ns("oci-setuid", |ns| async move {
        ns.ensure_profile(Profile::OciDefault).await?;
        let step = Step::oci(&ns.state.oci_variant("oci-plain").reference());
        let guest = ns
            .guest("probe", &step, json!({"privileges": true}))
            .await?;
        let status = guest.wait_status(WAIT, |s| s.privileges.is_some()).await?;
        let privileges = status.privileges.expect("privileges");
        let setuid = &privileges.setuid;
        ensure!(setuid.owner == Some(0) && setuid.bit, "{setuid:?}");
        ensure!(setuid.euid == Some(privileges.uid), "{setuid:?}");
        ensure!(setuid.euid != Some(0), "{setuid:?}");
        Ok(())
    })
    .await
}
