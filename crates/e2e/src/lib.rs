//! The Kubernetes-native end-to-end suite (docs/project/testing.md, issue
//! #185): this tree's runtime installed as a Function in a kind cluster
//! (`make e2e.up`), driven through composite resources of the committed
//! test module under `guest/`, which composes provider-nop resources and
//! writes what it observed into the composite's status. The tests are
//! `tests/<profile>.rs`, one target per runtime profile (`profiles/`), each
//! switching the Function to its profile's DeploymentRuntimeConfig before
//! its first test; `src/bin/setup.rs` is what `make e2e.up` runs after the
//! cluster exists: the module registry with its TLS and auth, provider-nop,
//! the test module pushed and signed under every variant, the profiles'
//! ConfigMaps and Secrets, the XRD.
//!
//! The shape is Linkerd's `policy-test`: a namespace per test, typed
//! helpers over kube-rs, waits with timeouts, `assert_eq!` on what the
//! cluster holds. Nothing is provisioned from inside a test. Everything
//! needs the `cluster` feature, so the crate is empty without it.

#[cfg(feature = "cluster")]
pub mod fixtures;
#[cfg(feature = "cluster")]
pub mod forward;
#[cfg(feature = "cluster")]
pub mod guest;
#[cfg(feature = "cluster")]
pub mod k8s;
#[cfg(feature = "cluster")]
pub mod metrics;
#[cfg(feature = "cluster")]
pub mod ns;
#[cfg(feature = "cluster")]
pub mod profile;
#[cfg(feature = "cluster")]
pub mod registry;
#[cfg(feature = "cluster")]
pub mod state;

/// What a test file imports.
#[cfg(feature = "cluster")]
pub mod prelude {
    pub use crate::fixtures::{Status, Step};
    pub use crate::guest;
    pub use crate::k8s::{condition, condition_message};
    pub use crate::metrics;
    pub use crate::ns::{Guest, TempNs, with_temp_ns};
    pub use crate::profile::Profile;
    pub use crate::state::State;
    pub use anyhow::{Context as _, Result, anyhow, bail, ensure};
    pub use serde_json::json;
    pub use std::time::Duration;
}
