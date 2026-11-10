//! guestfn wit update and guestfn wit fetch: a guest's copies of the WIT
//! packages it builds against, from their published versions: guestfn:function
//! and the WASI packages its world imports, with everything those refer to.
//! Update bumps the contract and pins every package's digest in guestfn.lock;
//! fetch, which guestfn build runs first, pulls exactly what is pinned, so a
//! copy cannot drift and a re-pushed tag changes nothing.

use std::collections::{BTreeMap, BTreeSet};
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
const DECLARING_KEYWORDS: [&str; 4] = ["import", "export", "include", "use"];

/// The package is a few kilobytes of WIT and its tag list a few entries;
/// these only bound what a misbehaving registry can hand back.
const TAG_LIST_LIMIT: usize = 1 << 20;
const PACKAGE_SIZE_LIMIT: u64 = 1 << 20;

#[derive(clap::Subcommand, Debug)]
pub enum WitCmd {
    /// Bump a guest to a published version of the guestfn:function package:
    /// the newest of the major its wit/world.wit includes, or the one
    /// --version names. Writes that package and the WASI packages the world
    /// imports under wit/deps, at the versions the world declares, moves the
    /// include and pins every artifact's digest in guestfn.lock.
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

    /// Repository the guestfn:function package is published to.
    #[arg(long, default_value = "ghcr.io/jonasz-lasut/guestfn/function")]
    repository: String,

    /// Repository prefix the WASI packages are published under.
    #[arg(long, default_value = "ghcr.io/webassembly/wasi")]
    wasi_repository: String,
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct LockedPackage {
    name: String,
    version: Version,
    repository: String,
    digest: String,
}

/// A package at a version, as a world or another package asks for it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PackageRef {
    name: String,
    version: Version,
}

/// A package pulled from its repository, ready to write and to pin.
struct Pulled {
    pin: LockedPackage,
    path: String,
    text: String,
    dependencies: Vec<PackageRef>,
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
        let target = match &self.version {
            Some(version) => version.clone(),
            None => self.newest_of_the_major(&current)?,
        };

        let mut pending = declared_wasi(&world)?;
        pending.push(PackageRef {
            name: PACKAGE.to_string(),
            version: target.clone(),
        });
        let mut pulled: BTreeMap<String, Pulled> = BTreeMap::new();
        while let Some(wanted) = pending.pop() {
            if let Some(done) = pulled.get(&wanted.name) {
                if done.pin.version != wanted.version {
                    return Err(format!(
                        "{} is wanted at both {} and {}, and wit/deps holds one version of a package",
                        wanted.name, done.pin.version, wanted.version
                    ));
                }
                continue;
            }
            let package = self.pull_tagged(&wanted)?;
            pending.extend(package.dependencies.iter().cloned());
            pulled.insert(wanted.name, package);
        }

        let moved_world = move_includes(&world, &target);
        let lock = render_lock(pulled.values().map(|package| &package.pin));
        let lock_path = self.dir.join(LOCK_FILE);
        let copies_match = pulled
            .values()
            .all(|package| read_or_empty(&self.dir.join(&package.path)) == package.text);
        if copies_match && moved_world == world && read_or_empty(&lock_path) == lock {
            println!("{PACKAGE} {target} is up to date");
            return Ok(());
        }
        for package in pulled.values() {
            write_vendored(&self.dir.join(&package.path), &package.text)?;
        }
        write(&self.dir.join(GUEST_WORLD), &moved_world)?;
        write(&lock_path, &lock)?;
        println!(
            "{PACKAGE} {current} -> {target}: wrote {} packages under wit/deps, {GUEST_WORLD} and {LOCK_FILE}; regenerate any bindings checked in",
            pulled.len()
        );
        Ok(())
    }

    fn newest_of_the_major(&self, current: &Version) -> Result<Version, String> {
        let reference = parse_any_reference(&format!("{}:{current}", self.repository))?;
        let tags = client_for(&reference)
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

    fn repository_of(&self, name: &str) -> Result<String, String> {
        if name == PACKAGE {
            return Ok(self.repository.clone());
        }
        vendored_path(name)?;
        let (_, package) = name.split_once(':').expect("vendored_path checked it");
        Ok(format!("{}/{package}", self.wasi_repository))
    }

    fn pull_tagged(&self, wanted: &PackageRef) -> Result<Pulled, String> {
        let repository = self.repository_of(&wanted.name)?;
        let tag = wanted.version.to_string();
        let artifact = format!("{repository}:{tag}");
        let client = client_for(&parse_any_reference(&artifact)?);
        pull(&client, &repository, &artifact, &tag, wanted)
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
/// no lock keeps the copies it has.
pub fn refresh(dir: &Path) -> Result<Option<Refresh>, String> {
    let lock_path = dir.join(LOCK_FILE);
    let text = match std::fs::read_to_string(&lock_path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", lock_path.display())),
    };
    let lock: Lock = serde_saphyr::from_str(&text)
        .map_err(|e| format!("cannot parse {}: {e}", lock_path.display()))?;
    let world = read_world(dir)?;
    check_declared(&lock, &world)?;

    // One registry round trip per package is most of what a build waits for
    // here, so the packages are pulled side by side.
    let pulled: Vec<Pulled> = std::thread::scope(|scope| {
        let pulls: Vec<_> = lock
            .packages
            .iter()
            .map(|pin| scope.spawn(move || pull_pinned(pin)))
            .collect();
        pulls
            .into_iter()
            .map(|pull| pull.join().expect("a pull does not panic"))
            .collect::<Result<Vec<_>, String>>()
    })?;

    let total = pulled.len();
    let mut written = 0;
    for package in &pulled {
        let path = dir.join(&package.path);
        if read_or_empty(&path) != package.text {
            write_vendored(&path, &package.text)?;
            written += 1;
        }
    }
    Ok(Some(if written == 0 {
        Refresh::Unchanged(format!("wit/deps matches {LOCK_FILE} ({total} packages)"))
    } else {
        Refresh::Written(format!(
            "wit/deps: wrote {written} of {total} packages from {LOCK_FILE}"
        ))
    }))
}

fn pull_pinned(pin: &LockedPackage) -> Result<Pulled, String> {
    let artifact = format!("{}@{}", pin.repository, pin.digest);
    let tagged = format!("{}:{}", pin.repository, pin.version);
    let client = client_for(&parse_any_reference(&tagged)?);
    let wanted = PackageRef {
        name: pin.name.clone(),
        version: pin.version.clone(),
    };
    pull(&client, &pin.repository, &artifact, &pin.digest, &wanted)
}

/// Holds what the world declares to what the lock pins.
fn check_declared(lock: &Lock, world: &str) -> Result<(), String> {
    let pinned = |name: &str| lock.packages.iter().find(|package| package.name == name);
    let contract = pinned(PACKAGE).ok_or_else(|| format!("{LOCK_FILE} pins no {PACKAGE}"))?;
    let included = included_version(world)?;
    if included != contract.version {
        return Err(format!(
            "{GUEST_WORLD} includes {PACKAGE} {included}, but {LOCK_FILE} pins {}: guestfn wit update moves them together",
            contract.version
        ));
    }
    for declared in declared_wasi(world)? {
        let pin = pinned(&declared.name).ok_or_else(|| {
            format!(
                "{GUEST_WORLD} imports {} {}, which {LOCK_FILE} does not pin: guestfn wit update pins it",
                declared.name, declared.version
            )
        })?;
        if pin.version != declared.version {
            return Err(format!(
                "{GUEST_WORLD} imports {} {}, but {LOCK_FILE} pins {}: guestfn wit update pins what the world declares",
                declared.name, declared.version, pin.version
            ));
        }
    }
    Ok(())
}

/// Pulls the package a tag or a digest names and holds it to what was asked
/// for.
fn pull(
    client: &RegistryClient,
    repository: &str,
    artifact: &str,
    reference: &str,
    wanted: &PackageRef,
) -> Result<Pulled, String> {
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
    if package.name != wanted.name {
        return Err(format!(
            "{artifact} holds {}, not {}",
            package.name, wanted.name
        ));
    }
    let version = wanted.version.to_string();
    if package.version.as_deref() != Some(version.as_str()) {
        return Err(format!(
            "{artifact} holds version {}, not {version}",
            package.version.as_deref().unwrap_or("none")
        ));
    }
    let dependencies = dependencies_of(&package, artifact)?;
    Ok(Pulled {
        pin: LockedPackage {
            name: wanted.name.clone(),
            version: wanted.version.clone(),
            repository: repository.to_string(),
            digest,
        },
        path: vendored_path(&wanted.name)?,
        text: package.text,
        dependencies,
    })
}

fn dependencies_of(package: &WitPackage, artifact: &str) -> Result<Vec<PackageRef>, String> {
    package
        .dependencies
        .iter()
        .map(|(name, version)| {
            let version = version
                .as_deref()
                .ok_or_else(|| format!("{artifact} refers to {name} with no version"))?;
            Ok(PackageRef {
                name: name.clone(),
                version: version
                    .parse()
                    .map_err(|e| format!("{artifact} refers to {name} {version}: {e}"))?,
            })
        })
        .collect()
}

/// Where a package lives under wit/deps. WASI's names keep the file names
/// guests have always vendored them under.
fn vendored_path(name: &str) -> Result<String, String> {
    if name == PACKAGE {
        return Ok(VENDORED_PACKAGE.to_string());
    }
    match name.split_once(':') {
        Some(("wasi", package))
            if !package.is_empty()
                && package
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') =>
        {
            Ok(format!("wit/deps/{package}.wit"))
        }
        _ => Err(format!(
            "{name} is neither {PACKAGE} nor a WASI package, which is all guestfn wit keeps"
        )),
    }
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

fn render_lock<'a>(pins: impl Iterator<Item = &'a LockedPackage>) -> String {
    let mut out = String::from(
        "# Written by guestfn wit update. guestfn build pulls exactly this.\npackages:\n",
    );
    for pin in pins {
        out.push_str(&format!(
            "  - name: {}\n    version: {}\n    repository: {}\n    digest: {}\n",
            pin.name, pin.version, pin.repository, pin.digest
        ));
    }
    out
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

/// The WASI packages a world names, at the versions it names. The file is
/// read as text because the packages it needs to parse may not be fetched yet.
fn declared_wasi(world: &str) -> Result<Vec<PackageRef>, String> {
    let mut declared = BTreeSet::new();
    for line in world.lines() {
        let mut words = line.split_whitespace();
        let (Some(keyword), Some(item)) = (words.next(), words.next()) else {
            continue;
        };
        if !DECLARING_KEYWORDS.contains(&keyword) {
            continue;
        }
        let Some(rest) = item.strip_prefix("wasi:") else {
            continue;
        };
        let Some((path, version)) = rest.split_once('@') else {
            return Err(format!("{GUEST_WORLD}: wasi:{rest} names no version"));
        };
        let version = version
            .trim_end_matches(';')
            .split(".{")
            .next()
            .unwrap_or(version);
        let package = path.split('/').next().unwrap_or(path);
        declared.insert(PackageRef {
            name: format!("wasi:{package}"),
            version: version
                .parse()
                .map_err(|e| format!("{GUEST_WORLD}: wasi:{package} {version}: {e}"))?,
        });
    }
    Ok(declared.into_iter().collect())
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
        let pins = vec![
            LockedPackage {
                name: PACKAGE.to_string(),
                version: Version::new(1, 2, 3),
                repository: "ghcr.io/x/guestfn/function".to_string(),
                digest: digest.clone(),
            },
            LockedPackage {
                name: "wasi:http".to_string(),
                version: Version::new(0, 2, 12),
                repository: "ghcr.io/x/wasi/http".to_string(),
                digest,
            },
        ];
        let lock: Lock = serde_saphyr::from_str(&render_lock(pins.iter())).expect("parse the lock");
        assert_eq!(lock, Lock { packages: pins });
    }

    #[test]
    fn reads_the_wasi_packages_a_world_declares() {
        let world = "// import wasi:clocks/types@9.9.9;\nworld w {\n  import wasi:http/outgoing-handler@0.2.12;\n  import wasi:http/types@0.2.12;\n  use wasi:io/streams@0.2.12.{input-stream};\n  import log: func();\n}\n";
        let name = |n: &str| PackageRef {
            name: n.to_string(),
            version: Version::new(0, 2, 12),
        };
        assert_eq!(
            declared_wasi(world),
            Ok(vec![name("wasi:http"), name("wasi:io")])
        );
    }

    #[test]
    fn a_package_stays_inside_wit_deps() {
        assert_eq!(
            vendored_path("wasi:http"),
            Ok("wit/deps/http.wit".to_string())
        );
        assert_eq!(vendored_path(PACKAGE), Ok(VENDORED_PACKAGE.to_string()));
        for name in ["wasi:../../etc", "other:thing", "wasi:", "nocolon"] {
            assert!(vendored_path(name).is_err(), "{name}");
        }
    }
}
