//! guestfn wit update and guestfn wit fetch: a guest's copy of the
//! guestfn:function package, from its published versions. Update bumps the
//! version the guest declares and pins the artifact's digest in guestfn.lock;
//! fetch, which guestfn build runs first, pulls exactly what is pinned, so a
//! copy cannot drift and a re-pushed tag changes nothing.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use guestfn_engine_wasm::componentize::{self, WitPackage};
use guestfn_runtime::location::parse_any_reference;
use guestfn_runtime::oci::{self, OciManifest, RegistryClient};
use semver::Version;
use serde::Deserialize;

use crate::inspect::client_for;

const PACKAGE: &str = "guestfn:function";
const VENDORED_PACKAGE: &str = "wit/deps/guestfn-function.wit";
const GUEST_WORLD: &str = "wit/world.wit";
const LOCK_FILE: &str = "guestfn.lock";
const INCLUDE_PREFIX: &str = "include guestfn:function/";

/// The package is a few kilobytes of WIT and its tag list a few entries;
/// these only bound what a misbehaving registry can hand back.
const TAG_LIST_LIMIT: usize = 1 << 20;
const PACKAGE_SIZE_LIMIT: u64 = 1 << 20;

#[derive(clap::Subcommand, Debug)]
pub enum WitCmd {
    /// Bump a guest to a published version of the guestfn:function package:
    /// the newest of the major its wit/world.wit includes, or the one
    /// --version names. Writes the package under wit/deps, moves the include
    /// and pins the artifact's digest in guestfn.lock.
    Update(UpdateCmd),
    /// Pull exactly what guestfn.lock pins into wit/deps, as guestfn build
    /// does first, for a build that does not go through it.
    Fetch(FetchCmd),
}

impl WitCmd {
    pub fn run(&self) -> Result<(), String> {
        match self {
            WitCmd::Update(cmd) => cmd.run(),
            WitCmd::Fetch(cmd) => cmd.run(),
        }
    }
}

#[derive(clap::Args, Debug)]
pub struct UpdateCmd {
    /// Project directory.
    #[arg(long, default_value = ".")]
    dir: PathBuf,

    /// Install this published version, a new major included.
    #[arg(long)]
    version: Option<Version>,

    /// Repository the package is published to.
    #[arg(long, default_value = "ghcr.io/jonasz-lasut/guestfn/function")]
    repository: String,
}

#[derive(clap::Args, Debug)]
pub struct FetchCmd {
    /// Project directory.
    #[arg(long, default_value = ".")]
    dir: PathBuf,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Lock {
    packages: Vec<LockedPackage>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct LockedPackage {
    name: String,
    version: Version,
    repository: String,
    digest: String,
}

/// What a refresh found, as the line to print.
pub enum Refresh {
    Written(String),
    Unchanged(String),
}

impl UpdateCmd {
    pub fn run(&self) -> Result<(), String> {
        let world = read_world(&self.dir)?;
        let current = included_version(&world)?;
        let reference = parse_any_reference(&format!("{}:{current}", self.repository))?;
        let client = client_for(&reference);
        let target = match &self.version {
            Some(version) => version.clone(),
            None => self.newest_of_the_major(&client, &current)?,
        };

        let artifact = format!("{}:{target}", self.repository);
        let (package, digest) = pull(&client, &artifact, &target.to_string(), &target)?;
        let moved_world = move_includes(&world, &target);
        let lock = render_lock(&self.repository, &target, &digest);
        let vendored_path = self.dir.join(VENDORED_PACKAGE);
        let lock_path = self.dir.join(LOCK_FILE);
        if read_or_empty(&vendored_path) == package.text
            && moved_world == world
            && read_or_empty(&lock_path) == lock
        {
            println!("{PACKAGE} {target} is up to date");
            return Ok(());
        }
        write_vendored(&vendored_path, &package.text)?;
        write(&self.dir.join(GUEST_WORLD), &moved_world)?;
        write(&lock_path, &lock)?;
        println!(
            "{PACKAGE} {current} -> {target}: wrote {VENDORED_PACKAGE}, {GUEST_WORLD} and {LOCK_FILE}; regenerate any bindings checked in"
        );
        Ok(())
    }

    fn newest_of_the_major(
        &self,
        client: &RegistryClient,
        current: &Version,
    ) -> Result<Version, String> {
        let tags = client
            .tags(TAG_LIST_LIMIT)
            .map_err(|e| format!("cannot list the versions at {}: {e}", self.repository))?;
        if let Some(next) = newest(&tags, |v| v.major > current.major) {
            println!("{PACKAGE} {next} is published: a new major, which --version {next} moves to");
        }
        newest(&tags, |v| v.major == current.major).ok_or_else(|| {
            format!(
                "{} has no {}.x version of {PACKAGE}",
                self.repository, current.major
            )
        })
    }
}

impl FetchCmd {
    pub fn run(&self) -> Result<(), String> {
        match refresh(&self.dir)? {
            None => Err(format!(
                "{} not found (guestfn wit update writes it)",
                self.dir.join(LOCK_FILE).display()
            )),
            Some(Refresh::Written(line) | Refresh::Unchanged(line)) => {
                println!("{line}");
                Ok(())
            }
        }
    }
}

/// Pulls what the project's guestfn.lock pins into wit/deps. A project with
/// no lock keeps the copy it has.
pub fn refresh(dir: &Path) -> Result<Option<Refresh>, String> {
    let lock_path = dir.join(LOCK_FILE);
    let text = match std::fs::read_to_string(&lock_path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", lock_path.display())),
    };
    let lock: Lock = serde_saphyr::from_str(&text)
        .map_err(|e| format!("cannot parse {}: {e}", lock_path.display()))?;
    let pin = lock
        .packages
        .iter()
        .find(|package| package.name == PACKAGE)
        .ok_or_else(|| format!("{} pins no {PACKAGE}", lock_path.display()))?;
    let included = included_version(&read_world(dir)?)?;
    if included != pin.version {
        return Err(format!(
            "{GUEST_WORLD} includes {PACKAGE} {included}, but {LOCK_FILE} pins {}: guestfn wit update moves them together",
            pin.version
        ));
    }

    let reference = parse_any_reference(&format!("{}:{}", pin.repository, pin.version))?;
    let client = client_for(&reference);
    let artifact = format!("{}@{}", pin.repository, pin.digest);
    let (package, _) = pull(&client, &artifact, &pin.digest, &pin.version)?;
    let vendored_path = dir.join(VENDORED_PACKAGE);
    if read_or_empty(&vendored_path) == package.text {
        return Ok(Some(Refresh::Unchanged(format!(
            "{PACKAGE} {}: {VENDORED_PACKAGE} matches {LOCK_FILE}",
            pin.version
        ))));
    }
    write_vendored(&vendored_path, &package.text)?;
    Ok(Some(Refresh::Written(format!(
        "{PACKAGE} {}: wrote {VENDORED_PACKAGE} from {LOCK_FILE}",
        pin.version
    ))))
}

/// Pulls the package a tag or a digest names and holds it to what was asked
/// for. Returns it with the digest of its manifest.
fn pull(
    client: &RegistryClient,
    artifact: &str,
    reference: &str,
    version: &Version,
) -> Result<(WitPackage, String), String> {
    let (raw, digest) = client.raw_manifest(reference)?;
    if reference.starts_with("sha256:") && digest != reference {
        return Err(format!("{artifact} is served as {digest}"));
    }
    let manifest: OciManifest = serde_json::from_slice(&raw)
        .map_err(|e| format!("cannot parse the manifest of {artifact}: {e}"))?;
    let layer = oci::wasm_layer(&manifest).map_err(|e| format!("{artifact} {e}"))?;
    let wasm = client.verified_blob(&layer.digest, PACKAGE_SIZE_LIMIT, "WIT package")?;
    let package =
        componentize::decode_wit_package(&wasm).map_err(|e| format!("{artifact}: {}", e.0))?;
    if package.name != PACKAGE {
        return Err(format!("{artifact} holds {}, not {PACKAGE}", package.name));
    }
    let want = version.to_string();
    if package.version.as_deref() != Some(want.as_str()) {
        return Err(format!(
            "{artifact} holds version {}, not {want}",
            package.version.as_deref().unwrap_or("none")
        ));
    }
    Ok((package, digest))
}

fn read_world(dir: &Path) -> Result<String, String> {
    let path = dir.join(GUEST_WORLD);
    std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {}: {e} (guestfn wit keeps a guest whose world includes {PACKAGE}, as guestfn init writes it)",
            path.display()
        )
    })
}

fn read_or_empty(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn write_vendored(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    write(path, text)
}

fn render_lock(repository: &str, version: &Version, digest: &str) -> String {
    format!(
        "# Written by guestfn wit update. guestfn build pulls exactly this.\npackages:\n  - name: {PACKAGE}\n    version: {version}\n    repository: {repository}\n    digest: {digest}\n"
    )
}

fn included_version(world: &str) -> Result<Version, String> {
    world
        .lines()
        .find_map(|line| {
            let included = line.trim().strip_prefix(INCLUDE_PREFIX)?;
            included.split_once('@')?.1.strip_suffix(';')?.parse().ok()
        })
        .ok_or_else(|| format!("{GUEST_WORLD} includes no version of {PACKAGE}"))
}

/// The newest stable version among the tags that `keep` admits: a tag that
/// is no version (a referrers tag, a cosign signature) is passed over.
fn newest(tags: &[String], keep: impl Fn(&Version) -> bool) -> Option<Version> {
    tags.iter()
        .filter_map(|tag| tag.parse::<Version>().ok())
        .filter(|version| version.pre.is_empty() && keep(version))
        .max()
}

fn move_includes(world: &str, to: &Version) -> String {
    world
        .split_inclusive('\n')
        .map(|line| {
            let includes_the_package = line.trim_start().starts_with(INCLUDE_PREFIX);
            match (includes_the_package, line.rfind('@'), line.rfind(';')) {
                (true, Some(at), Some(end)) if at < end => {
                    format!("{}@{to}{}", &line[..at], &line[end..])
                }
                _ => line.to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORLD: &str = "package local:guest;\n\nworld function {\n    include guestfn:function/function-sync@1.0.0;\n\n    import wasi:http/outgoing-handler@0.2.12;\n}\n";

    #[test]
    fn reads_the_version_the_world_includes() {
        assert_eq!(included_version(WORLD), Ok(Version::new(1, 0, 0)));
        assert_eq!(
            included_version("package local:guest;\n"),
            Err(format!("{GUEST_WORLD} includes no version of {PACKAGE}"))
        );
    }

    #[test]
    fn passes_over_prereleases_and_tags_that_are_no_version() {
        let published: Vec<String> = [
            "1.0.0",
            "1.1.0",
            "1.10.0",
            "1.11.0-rc.1",
            "sha256-abc",
            "latest",
            "2.0.0",
        ]
        .iter()
        .map(|tag| tag.to_string())
        .collect();
        assert_eq!(
            newest(&published, |v| v.major == 1),
            Some(Version::new(1, 10, 0))
        );
        assert_eq!(
            newest(&published, |v| v.major > 1),
            Some(Version::new(2, 0, 0))
        );
        assert_eq!(newest(&published, |v| v.major == 3), None);
    }

    #[test]
    fn moves_only_the_contract_include() {
        let moved = move_includes(WORLD, &Version::new(1, 4, 0));
        assert_eq!(
            moved,
            WORLD.replace("function-sync@1.0.0", "function-sync@1.4.0")
        );
    }

    #[test]
    fn the_lock_it_writes_is_the_lock_it_reads() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let text = render_lock(
            "ghcr.io/x/guestfn/function",
            &Version::new(1, 2, 3),
            &digest,
        );
        let lock: Lock = serde_saphyr::from_str(&text).expect("parse the lock");
        assert_eq!(
            lock,
            Lock {
                packages: vec![LockedPackage {
                    name: PACKAGE.to_string(),
                    version: Version::new(1, 2, 3),
                    repository: "ghcr.io/x/guestfn/function".to_string(),
                    digest,
                }],
            }
        );
    }
}
