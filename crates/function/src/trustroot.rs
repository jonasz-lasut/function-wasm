//! The Sigstore trusted root fetched over TUF and refreshed while serving
//! (--cosign-trust-instance, #183).
//!
//! A trusted root names the Fulcio authorities, the Rekor and
//! certificate-transparency keys and the timestamp authorities a keyless
//! signature is verified against, each with its validity window, and
//! Sigstore rotates them. A runtime holding a stale file keeps admitting
//! old signatures and refuses every module signed after the next rotation.
//! Instead of a file (--cosign-trusted-root, which stays the air-gapped
//! mode), the operator may name a well-known instance: the TUF root
//! embedded in sigstore-trust-root bootstraps the walk - root, timestamp,
//! snapshot and targets, each signed and expiring - and trusted_root.json
//! is fetched by the length and hashes the verified metadata pins. The
//! verified metadata and the target are cached under the module cache
//! root (tuf/<instance>), so a pod that cannot reach the repository at
//! startup starts from what an earlier one verified, and a refresh runs
//! while serving: a changed root replaces the verifier's and forgets its
//! verified-digest memo, a failed one keeps the last good root. The
//! request path never touches the network: a refresh can only make the
//! root older, never a request slower.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use function_wasm_engine::duration;
use function_wasm_engine::metrics::{LabeledCounter, PlainGauge, plain_gauge};
use sigstore_trust_root::SigstoreInstance;
use sigstore_tuf::{FileStore, HttpRepository, StoreRepository, Updater};

use crate::cosign::Verifier;

/// The TUF target that is the trusted root.
const TRUSTED_ROOT_TARGET: &str = "trusted_root.json";
/// Bounds one fetch of the root, the whole TUF walk: the client's own
/// per-request timeouts guard each step against a slow repository, this
/// guards the startup and the refresh against all of them together.
pub const FETCH_BOUND: Duration = Duration::from_secs(60);
/// The least --cosign-tuf-refresh accepted.
pub const MIN_REFRESH: Duration = Duration::from_secs(60);
/// How soon a failed refresh is retried while the runtime has no root at
/// all - keyless signatures are refused until one arrives.
const RETRY_WITHOUT_ROOT: Duration = Duration::from_secs(60);

pub const OUTCOME_UPDATED: &str = "updated";
pub const OUTCOME_UNCHANGED: &str = "unchanged";
pub const OUTCOME_ERROR: &str = "error";

static REFRESHES: LazyLock<LabeledCounter> = LazyLock::new(|| {
    LabeledCounter::new(
        "function_wasm_trusted_root_refreshes",
        "Refreshes of the Sigstore trusted root over TUF by outcome: updated (a root put in use, the first one included), unchanged (the same root fetched again), error (the last good root kept).",
        &["outcome"],
    )
});

static LAST_REFRESH: LazyLock<PlainGauge> = LazyLock::new(|| {
    plain_gauge(
        "function_wasm_trusted_root_last_refresh_timestamp_seconds",
        "Unix time of the last successful refresh of the Sigstore trusted root over TUF; 0 until the first.",
    )
});

/// A well-known Sigstore instance whose trusted root the runtime fetches
/// over TUF, bootstrapped from the TUF root sigstore-trust-root embeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Instance {
    /// The public-good instance (sigstore.dev), which cosign signs with by
    /// default.
    PublicGood,
    /// Sigstore's staging instance.
    Staging,
    /// GitHub's instance, which signs artifact attestations for private
    /// repositories.
    #[value(name = "github")]
    GitHub,
}

impl Instance {
    /// The name the flag takes and the cache directory carries.
    pub fn name(self) -> &'static str {
        match self {
            Instance::PublicGood => "public-good",
            Instance::Staging => "staging",
            Instance::GitHub => "github",
        }
    }

    fn sigstore(self) -> SigstoreInstance {
        match self {
            Instance::PublicGood => SigstoreInstance::PublicGood,
            Instance::Staging => SigstoreInstance::Staging,
            Instance::GitHub => SigstoreInstance::GitHub,
        }
    }

    /// Where the verified TUF metadata and the root are cached: beside the
    /// module caches, which the LRU sweep walks by store and so never
    /// touches this directory.
    pub fn cache_dir(self) -> PathBuf {
        Path::new(crate::store::DEFAULT_DIR)
            .join(crate::store::TUF_DIR)
            .join(self.name())
    }

    /// The instance's TUF repository.
    pub fn source(self) -> Source {
        Source {
            name: self.name().to_string(),
            url: self.sigstore().tuf_url().to_string(),
            root: self.sigstore().tuf_root().to_vec(),
            cache_dir: self.cache_dir(),
        }
    }
}

/// The two ways to a trusted root exclude each other: the file is the
/// root, or the instance's is fetched.
pub fn exclusive(file: Option<&Path>, instance: Option<Instance>) -> Result<(), String> {
    if file.is_some() && instance.is_some() {
        return Err(
            "--cosign-trusted-root and --cosign-trust-instance are mutually exclusive: the file is the trusted root, or the instance's root is fetched over TUF"
                .to_string(),
        );
    }
    Ok(())
}

/// Holds --cosign-tuf-refresh to its floor.
pub fn check_refresh(refresh: Duration) -> Result<(), String> {
    if refresh < MIN_REFRESH {
        return Err(format!(
            "--cosign-tuf-refresh must be at least {}, got {}",
            duration::format(MIN_REFRESH),
            duration::format(refresh)
        ));
    }
    Ok(())
}

/// A TUF repository the trusted root is fetched from: its URL, the TUF
/// root that bootstraps the walk and the directory that caches what the
/// walk verified.
#[derive(Clone, Debug)]
pub struct Source {
    name: String,
    url: String,
    root: Vec<u8>,
    cache_dir: PathBuf,
}

impl Source {
    /// A repository other than a well-known instance's: the tests' local
    /// one, and what a private mirror flag would build.
    pub fn custom(
        name: impl Into<String>,
        url: impl Into<String>,
        root: Vec<u8>,
        cache_dir: impl Into<PathBuf>,
    ) -> Source {
        Source {
            name: name.into(),
            url: url.into(),
            root,
            cache_dir: cache_dir.into(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Fetches the trusted root: refreshes the TUF metadata from the
    /// repository (from the cache's state when there is one) and reads the
    /// target it pins, caching both. Bounded by [`FETCH_BOUND`] in all.
    pub async fn fetch(&self) -> Result<Vec<u8>, String> {
        tokio::time::timeout(FETCH_BOUND, self.walk(false))
            .await
            .map_err(|_| {
                format!(
                    "fetching the trusted root of {} from {} took longer than {}",
                    self.name,
                    self.url,
                    duration::format(FETCH_BOUND)
                )
            })?
    }

    /// Reads the trusted root from the cache alone, re-verifying the cached
    /// metadata from the embedded TUF root: as trustworthy as a fetch, only
    /// as old as the cache, and refused once the metadata has expired.
    pub async fn cached(&self) -> Result<Vec<u8>, String> {
        self.walk(true).await
    }

    /// The root from the repository or, failing that, from the cache - the
    /// startup order, when the runtime has no root to keep.
    pub async fn fetch_or_cached(&self) -> Result<(Vec<u8>, Origin), String> {
        let fetch_error = match self.fetch().await {
            Ok(raw) => return Ok((raw, Origin::Repository)),
            Err(e) => e,
        };
        match self.cached().await {
            Ok(raw) => {
                tracing::warn!(
                    instance = %self.name,
                    error = %fetch_error,
                    "Cannot fetch the trusted root; using the verified cache"
                );
                Ok((raw, Origin::Cache))
            }
            Err(cache_error) => Err(format!("{fetch_error}; the cache: {cache_error}")),
        }
    }

    async fn walk(&self, offline: bool) -> Result<Vec<u8>, String> {
        let now = jiff::Timestamp::now();
        let store = FileStore::new(&self.cache_dir);
        let updater = if offline {
            Updater::new(StoreRepository::new(store.clone()), &self.root)
        } else {
            tokio::fs::create_dir_all(&self.cache_dir)
                .await
                .map_err(|e| {
                    format!(
                        "cannot create the TUF cache directory {}: {e}",
                        self.cache_dir.display()
                    )
                })?;
            let repository = HttpRepository::new(&self.url)
                .map_err(|e| format!("TUF repository {}: {}", self.url, chain(&e)))?
                .with_http_client(http_client()?);
            Updater::new(repository, &self.root)
        };
        let mut updater = updater
            .map_err(|e| format!("the embedded TUF root of {}: {}", self.name, chain(&e)))?
            .with_store(store);
        updater.refresh(now).await.map_err(|e| {
            if offline {
                format!(
                    "the cached TUF metadata of {} does not verify: {}",
                    self.name,
                    chain(&e)
                )
            } else {
                format!(
                    "cannot refresh the TUF metadata of {} from {}: {}",
                    self.name,
                    self.url,
                    chain(&e)
                )
            }
        })?;
        updater
            .get_target(TRUSTED_ROOT_TARGET, now)
            .await
            .map_err(|e| {
                format!(
                    "cannot read {TRUSTED_ROOT_TARGET} of {}: {}",
                    self.name,
                    chain(&e)
                )
            })
    }
}

/// Where a root came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Repository,
    Cache,
}

/// The client every TUF request goes through: the runtime's own, with the
/// timeouts the TUF client asks its callers for against a repository that
/// answers slowly on purpose.
fn http_client() -> Result<sigstore_tuf::reqwest::Client, String> {
    let version = crate::manifest::runtime_version();
    let agent = if version.is_empty() {
        "function-wasm".to_string()
    } else {
        format!("function-wasm/{version}")
    };
    sigstore_tuf::reqwest::Client::builder()
        .user_agent(agent)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("cannot build the TUF HTTP client: {e}"))
}

/// A TUF error with its causes, which carry the useful part (the role that
/// expired, the signature that failed).
fn chain(error: &sigstore_tuf::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

/// What one refresh did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A root was put in use: the first, or one that differs from the last.
    Updated,
    /// The repository served the root in use again.
    Unchanged,
    /// Nothing changed: the last good root stays, if there is one.
    Failed(String),
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Updated => OUTCOME_UPDATED,
            Outcome::Unchanged => OUTCOME_UNCHANGED,
            Outcome::Failed(_) => OUTCOME_ERROR,
        }
    }
}

/// Keeps a verifier's trusted root current: one fetch at startup, then one
/// per interval, each counted and logged once per change of outcome.
pub struct Refresher {
    source: Source,
    verifier: Arc<Verifier>,
    interval: Duration,
    last_outcome: Mutex<Option<&'static str>>,
}

impl Refresher {
    pub fn new(source: Source, verifier: Arc<Verifier>, interval: Duration) -> Refresher {
        // Every outcome exists at zero before the first refresh, so a scrape
        // sees the full set.
        for outcome in [OUTCOME_UPDATED, OUTCOME_UNCHANGED, OUTCOME_ERROR] {
            let _ = REFRESHES.with_label_values(&[outcome]);
        }
        LAST_REFRESH.set(0.0);
        Refresher {
            source,
            verifier,
            interval,
            last_outcome: Mutex::new(None),
        }
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// One refresh: the root from the repository or, while the verifier
    /// has none, from the cache, put in use when it differs from the root
    /// in use. A failure records why for the refusals that name it.
    pub async fn refresh(&self) -> Outcome {
        let fetched = if self.verifier.has_trusted_root() {
            self.source
                .fetch()
                .await
                .map(|raw| (raw, Origin::Repository))
        } else {
            self.source.fetch_or_cached().await
        };
        let outcome = match fetched {
            Ok((raw, origin)) => match self.verifier.set_trusted_root(&raw) {
                Ok(true) => {
                    tracing::info!(
                        instance = %self.source.name,
                        origin = ?origin,
                        bytes = raw.len(),
                        "Trusted root updated"
                    );
                    Outcome::Updated
                }
                Ok(false) => Outcome::Unchanged,
                Err(e) => Outcome::Failed(e),
            },
            Err(e) => Outcome::Failed(e),
        };
        self.record(&outcome);
        outcome
    }

    fn record(&self, outcome: &Outcome) {
        REFRESHES.with_label_values(&[outcome.label()]).inc();
        if let Outcome::Failed(e) = outcome {
            self.verifier.note_unavailable(e.clone());
        } else if let Ok(now) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            LAST_REFRESH.set(now.as_secs_f64());
        }
        let mut last = self.last_outcome.lock().expect("poisoned");
        let changed = last.replace(outcome.label()) != Some(outcome.label());
        match outcome {
            Outcome::Failed(e) if changed => tracing::warn!(
                instance = %self.source.name,
                error = %e,
                has_root = self.verifier.has_trusted_root(),
                "Cannot refresh the trusted root"
            ),
            Outcome::Failed(e) => tracing::debug!(
                instance = %self.source.name,
                error = %e,
                "Cannot refresh the trusted root"
            ),
            Outcome::Unchanged if changed => {
                tracing::info!(instance = %self.source.name, "Trusted root refreshed, unchanged")
            }
            _ => {
                tracing::debug!(instance = %self.source.name, outcome = outcome.label(), "Trusted root refresh")
            }
        }
    }

    /// The first refresh, then the loop: once per interval, or once a
    /// minute while the verifier still has no root.
    pub async fn start(self: Arc<Self>) {
        self.refresh().await;
        tokio::spawn(async move {
            loop {
                let wait = if self.verifier.has_trusted_root() {
                    self.interval
                } else {
                    self.interval.min(RETRY_WITHOUT_ROOT)
                };
                tokio::time::sleep(wait).await;
                self.refresh().await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instances_name_their_cache_directories() {
        assert_eq!(
            Instance::PublicGood.cache_dir(),
            Path::new("/tmp/function-wasm-cache/tuf/public-good")
        );
        assert_eq!(Instance::GitHub.cache_dir().file_name().unwrap(), "github");
        assert_eq!(Instance::Staging.source().name(), "staging");
        assert!(
            Instance::PublicGood
                .source()
                .url
                .starts_with("https://tuf-repo-cdn.sigstore.dev")
        );
    }

    #[test]
    fn the_file_and_the_instance_exclude_each_other() {
        assert!(exclusive(None, None).is_ok());
        assert!(exclusive(Some(Path::new("root.json")), None).is_ok());
        assert!(exclusive(None, Some(Instance::PublicGood)).is_ok());
        assert_eq!(
            exclusive(Some(Path::new("root.json")), Some(Instance::PublicGood)).unwrap_err(),
            "--cosign-trusted-root and --cosign-trust-instance are mutually exclusive: the file is the trusted root, or the instance's root is fetched over TUF"
        );
    }

    #[test]
    fn the_refresh_interval_has_a_floor() {
        assert!(check_refresh(Duration::from_secs(60)).is_ok());
        assert_eq!(
            check_refresh(Duration::from_secs(30)).unwrap_err(),
            "--cosign-tuf-refresh must be at least 1m0s, got 30s"
        );
    }
}
