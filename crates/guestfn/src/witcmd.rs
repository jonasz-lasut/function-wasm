//! guestfn wit update: refreshes a guest's copy of the guestfn:function
//! package from its published versions.

use std::path::PathBuf;

use guestfn_engine_wasm::componentize::{self, WitPackage};
use guestfn_runtime::location::parse_any_reference;
use guestfn_runtime::oci::{self, OciManifest, RegistryClient};
use semver::Version;

use crate::inspect::client_for;

const PACKAGE: &str = "guestfn:function";
const VENDORED_PACKAGE: &str = "wit/deps/guestfn-function.wit";
const GUEST_WORLD: &str = "wit/world.wit";
const INCLUDE_PREFIX: &str = "include guestfn:function/";
const DECLARATION_PREFIX: &str = "package guestfn:function@";

/// The package is a few kilobytes of WIT and its tag list a few entries;
/// these only bound what a misbehaving registry can hand back.
const TAG_LIST_LIMIT: usize = 1 << 20;
const PACKAGE_SIZE_LIMIT: u64 = 1 << 20;

#[derive(clap::Subcommand, Debug)]
pub enum WitCmd {
    /// Refresh a guest's copy of the guestfn:function package from its
    /// published versions: the newest of the major the copy declares, or
    /// the one --version names. The include in wit/world.wit moves with it.
    Update(UpdateCmd),
}

impl WitCmd {
    pub fn run(&self) -> Result<(), String> {
        match self {
            WitCmd::Update(cmd) => cmd.run(),
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

impl UpdateCmd {
    pub fn run(&self) -> Result<(), String> {
        let vendored_path = self.dir.join(VENDORED_PACKAGE);
        let vendored = std::fs::read_to_string(&vendored_path).map_err(|e| {
            format!(
                "cannot read {}: {e} (guestfn wit update refreshes a guest that vendors {PACKAGE} there, as guestfn init writes it)",
                vendored_path.display()
            )
        })?;
        let current = declared_version(&vendored)
            .ok_or_else(|| format!("{} declares no {PACKAGE} version", vendored_path.display()))?;
        let world_path = self.dir.join(GUEST_WORLD);
        let world = std::fs::read_to_string(&world_path)
            .map_err(|e| format!("cannot read {}: {e}", world_path.display()))?;

        let reference = parse_any_reference(&format!("{}:{current}", self.repository))?;
        let client = client_for(&reference);
        let target = match &self.version {
            Some(version) => version.clone(),
            None => self.newest_of_the_major(&client, &current)?,
        };

        let package = self.pull(&client, &target)?;
        let moved_world = move_includes(&world, &target);
        if package.text == vendored && moved_world == world {
            println!("{PACKAGE} {target} is up to date");
            return Ok(());
        }
        std::fs::write(&vendored_path, &package.text)
            .map_err(|e| format!("cannot write {}: {e}", vendored_path.display()))?;
        std::fs::write(&world_path, &moved_world)
            .map_err(|e| format!("cannot write {}: {e}", world_path.display()))?;
        println!(
            "{PACKAGE} {current} -> {target}: wrote {VENDORED_PACKAGE} and {GUEST_WORLD}; regenerate any bindings checked in"
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

    fn pull(&self, client: &RegistryClient, version: &Version) -> Result<WitPackage, String> {
        let tag = version.to_string();
        let artifact = format!("{}:{tag}", self.repository);
        let (raw, _) = client.raw_manifest(&tag)?;
        let manifest: OciManifest = serde_json::from_slice(&raw)
            .map_err(|e| format!("cannot parse the manifest of {artifact}: {e}"))?;
        let layer = oci::wasm_layer(&manifest).map_err(|e| format!("{artifact} {e}"))?;
        let wasm = client.verified_blob(&layer.digest, PACKAGE_SIZE_LIMIT, "WIT package")?;
        let package =
            componentize::decode_wit_package(&wasm).map_err(|e| format!("{artifact}: {}", e.0))?;
        if package.name != PACKAGE {
            return Err(format!("{artifact} holds {}, not {PACKAGE}", package.name));
        }
        if package.version.as_deref() != Some(tag.as_str()) {
            return Err(format!(
                "{artifact} holds version {}, not {tag}",
                package.version.as_deref().unwrap_or("none")
            ));
        }
        Ok(package)
    }
}

fn declared_version(wit: &str) -> Option<Version> {
    wit.lines().find_map(|line| {
        line.trim()
            .strip_prefix(DECLARATION_PREFIX)?
            .strip_suffix(';')?
            .parse()
            .ok()
    })
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

    fn tags(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn reads_the_declared_version_past_the_doc_comments() {
        let wit = "/// The contract.\n///\n/// package guestfn:function@9.9.9;\npackage guestfn:function@1.2.3;\n";
        assert_eq!(declared_version(wit), Some(Version::new(1, 2, 3)));
        assert_eq!(declared_version("package other:thing@1.0.0;\n"), None);
    }

    #[test]
    fn passes_over_prereleases_and_tags_that_are_no_version() {
        let published = tags(&[
            "1.0.0",
            "1.1.0",
            "1.10.0",
            "1.11.0-rc.1",
            "sha256-abc",
            "latest",
            "2.0.0",
        ]);
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
        let world = "package local:guest;\n\nworld function {\n    include guestfn:function/function-sync@1.0.0;\n\n    import wasi:http/outgoing-handler@0.2.12;\n}\n";
        let want = "package local:guest;\n\nworld function {\n    include guestfn:function/function-sync@1.4.0;\n\n    import wasi:http/outgoing-handler@0.2.12;\n}\n";
        assert_eq!(move_includes(world, &Version::new(1, 4, 0)), want);
    }
}
