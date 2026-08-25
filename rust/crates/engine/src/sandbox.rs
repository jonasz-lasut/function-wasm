//! Sandbox mechanics of one run (docs/one-pager-sandbox.md): a WASI pre-open
//! for the private /tmp - the only directory a guest is ever given; host
//! directories are deliberately not mountable - and WASI environ for the
//! environment. Nothing here touches the ABI: a guest uses its language's
//! file and environment APIs.

use std::path::Path;

use wasmtime_wasi::{FsPerms, WasiCtxBuilder};

use crate::{Error, RunOptions};

/// Where a guest sees its private /tmp; Go's os.TempDir and Rust's
/// env::temp_dir default to it on WASI.
pub const PRIVATE_TMP_GUEST_PATH: &str = "/tmp";
/// Names the per-run directories under the host's temp dir.
pub const PRIVATE_TMP_PREFIX: &str = "function-wasm-private-tmp-";

/// A run's private /tmp, if any; dropping it removes the directory. A removal
/// failure is logged, not returned: the run's outcome is the guest's, and a
/// leftover directory is the operator's to notice.
pub(crate) struct PrivateTmp(Option<tempfile::TempDir>);

impl PrivateTmp {
    pub(crate) fn create(want: bool) -> Result<Self, Error> {
        if !want {
            return Ok(PrivateTmp(None));
        }
        let dir = tempfile::Builder::new()
            .prefix(PRIVATE_TMP_PREFIX)
            .tempdir()
            .map_err(|e| Error(format!("cannot create the private /tmp: {e}")))?;
        Ok(PrivateTmp(Some(dir)))
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        self.0.as_ref().map(|d| d.path())
    }
}

impl Drop for PrivateTmp {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            let path = dir.path().to_owned();
            if let Err(e) = dir.close() {
                tracing::info!(dir = %path.display(), error = %e, "Cannot remove the private /tmp");
            }
        }
    }
}

/// Applies the run's grants to the WASI config: the private /tmp (if any)
/// pre-opened writable at /tmp, and the environment. With no grants it does
/// nothing, so the default store stays exactly as before.
pub(crate) fn configure(
    wasi: &mut WasiCtxBuilder,
    opts: &RunOptions,
    tmp: Option<&Path>,
) -> Result<(), Error> {
    if let Some(dir) = tmp {
        wasi.preopened_dir(dir, PRIVATE_TMP_GUEST_PATH, FsPerms::ReadWrite)
            .map_err(|e| Error(format!("cannot pre-open the private /tmp: {e}")))?;
    }
    // A BTreeMap iterates sorted by key, like the Go engine's sorted SetEnv.
    for (k, v) in &opts.env {
        wasi.env(k, v);
    }
    Ok(())
}
