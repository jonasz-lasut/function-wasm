//! What `e2e-setup` leaves for the tests: the registry's address and
//! credential, every pushed variant's digest-pinned reference, the signer
//! identities and the published module the lifecycle profile warms. Written
//! to `_output/e2e/state.json` (or `E2E_STATE`), read at the start of every
//! test, so a test never recomputes or rediscovers what setup decided.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    /// The registry as the cluster names it: `registry.e2e.svc:5000`.
    pub registry: String,
    pub pull_username: String,
    pub pull_password: String,
    /// Every pushed variant by name: its repository and manifest digest.
    pub variants: BTreeMap<String, Pushed>,
    pub trusted_issuer: String,
    pub trusted_identity: String,
    pub untrusted_identity: String,
    /// The published pdb-addon, pinned: the real keyless signature and the
    /// lifecycle profile's warm-up module.
    pub pdb_addon: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pushed {
    pub repository: String,
    pub digest: String,
}

impl Pushed {
    /// The digest-pinned reference an Input names.
    pub fn reference(&self) -> String {
        format!("{}@{}", self.repository, self.digest)
    }
}

impl State {
    pub fn path() -> PathBuf {
        std::env::var_os("E2E_STATE")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo_root().join("_output/e2e/state.json"))
    }

    pub fn load() -> Result<State> {
        let path = Self::path();
        let raw = std::fs::read(&path)
            .with_context(|| format!("cannot read {}: run make e2e.up first", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("cannot parse {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().expect("parent"))?;
        std::fs::write(&path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("cannot write {}", path.display()))
    }

    /// A pushed variant by name; a name setup never pushed is a test bug.
    pub fn variant(&self, name: &str) -> &Pushed {
        self.variants
            .get(name)
            .unwrap_or_else(|| panic!("no variant {name:?} in {}", Self::path().display()))
    }

    /// The registry's `/v2/` endpoint, the target of every egress case.
    pub fn registry_v2_url(&self) -> String {
        format!("https://{}/v2/", self.registry)
    }
}

/// The repository root: this crate is crates/e2e.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// Where the suite writes what it leaves behind: state, logs of failed tests.
pub fn output_dir() -> PathBuf {
    repo_root().join("_output/e2e")
}
