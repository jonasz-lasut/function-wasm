//! The image config as the engine keeps it, the process contract's
//! preconditions over it, and the run description the init gets (the
//! config resolved against the run's grants).

use std::collections::BTreeMap;

use function_wasm_engine_api::{Error, RunOptions};
use guestfn_init::Description;
use serde::{Deserialize, Serialize};

/// What the engine keeps of an OCI image config, plus what it resolved
/// from the root filesystem at compile time: the user as numbers. Stored
/// inside the squashfs it describes (`rootfs::CONFIG_PATH`), so the
/// compiled store needs no sidecar.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ImageConfig {
    pub architecture: String,
    pub os: String,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    /// The config's `User` as written: a name, a number, or either with a
    /// group.
    pub user: String,
    pub working_dir: String,
    /// The user resolved against the image's /etc/passwd and /etc/group.
    pub uid: u32,
    pub gid: u32,
    pub groups: Vec<u32>,
    /// How many layers the image has, and their bytes together.
    pub layers: usize,
    pub size: u64,
}

/// The PATH a process gets when the image sets none - Docker's default.
pub const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// The guest's hostname.
pub const HOSTNAME: &str = "guestfn";

/// The OCI image config's fields the engine reads.
#[derive(Deserialize)]
struct RawConfig {
    #[serde(default)]
    architecture: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    config: RawInner,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct RawInner {
    #[serde(default)]
    entrypoint: Vec<String>,
    #[serde(default)]
    cmd: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    user: String,
    #[serde(default)]
    working_dir: String,
}

impl ImageConfig {
    /// Reads the OCI image config blob; the user stays unresolved (uid and
    /// gid 0) until `resolve_user`.
    pub fn parse(raw: &[u8]) -> Result<ImageConfig, Error> {
        let c: RawConfig = serde_json::from_slice(raw)
            .map_err(|e| Error(format!("cannot parse the image config: {e}")))?;
        Ok(ImageConfig {
            architecture: c.architecture,
            os: c.os,
            entrypoint: c.config.entrypoint,
            cmd: c.config.cmd,
            env: c.config.env,
            user: c.config.user,
            working_dir: c.config.working_dir,
            ..Default::default()
        })
    }

    /// The process contract's preconditions: a command to run, and a
    /// platform this host boots. None is a load-time refusal, so a
    /// Composition fails at `function validate --resolve` and at the first
    /// request alike, before a VM boots.
    pub fn check(&self) -> Result<(), String> {
        if self.entrypoint.is_empty() && self.cmd.is_empty() {
            return Err(
                "image has neither an entrypoint nor a command: the process contract runs the image's configured command (docs/one-pager-oci-runtime.md)"
                    .to_string(),
            );
        }
        if !self.os.is_empty() && self.os != "linux" {
            return Err(format!(
                "image is built for {}, and the OCI runtime boots linux images only",
                self.os
            ));
        }
        let host = host_architecture();
        if !self.architecture.is_empty() && self.architecture != host {
            return Err(format!(
                "image is built for {}/{}, and this runtime runs on linux/{host}",
                if self.os.is_empty() {
                    "linux"
                } else {
                    &self.os
                },
                self.architecture
            ));
        }
        Ok(())
    }

    /// The command line the contract runs: the entrypoint, then the
    /// command.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = self.entrypoint.clone();
        argv.extend(self.cmd.iter().cloned());
        argv
    }

    /// The environment the process gets: the image's, PATH set where it
    /// is not, and the run's bindings over it (a binding names a variable
    /// the manifest requested, which wins over the image's value).
    pub fn environment(&self, bindings: &BTreeMap<String, String>) -> Vec<String> {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        env.insert("PATH".to_string(), DEFAULT_PATH.to_string());
        for entry in &self.env {
            if let Some((k, v)) = entry.split_once('=') {
                env.insert(k.to_string(), v.to_string());
            }
        }
        for (k, v) in bindings {
            env.insert(k.clone(), v.clone());
        }
        env.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
    }

    /// The run's description for the init, from the config and the run's
    /// grants.
    pub fn describe(&self, opts: &RunOptions) -> Description {
        Description {
            argv: self.argv(),
            env: self.environment(&opts.env),
            cwd: if self.working_dir.is_empty() {
                "/".to_string()
            } else {
                self.working_dir.clone()
            },
            uid: self.uid,
            gid: self.gid,
            groups: self.groups.clone(),
            hostname: HOSTNAME.to_string(),
        }
    }

    /// The lines a tool prints for the image.
    pub fn details(&self) -> Vec<(String, String)> {
        let mut out = vec![(
            "platform".to_string(),
            format!(
                "{}/{}",
                if self.os.is_empty() {
                    "linux"
                } else {
                    &self.os
                },
                if self.architecture.is_empty() {
                    "unknown"
                } else {
                    &self.architecture
                }
            ),
        )];
        if !self.entrypoint.is_empty() {
            out.push(("entrypoint".to_string(), shell_words(&self.entrypoint)));
        }
        if !self.cmd.is_empty() {
            out.push(("cmd".to_string(), shell_words(&self.cmd)));
        }
        out.push((
            "user".to_string(),
            if self.user.is_empty() {
                format!("root ({}:{})", self.uid, self.gid)
            } else {
                format!("{} ({}:{})", self.user, self.uid, self.gid)
            },
        ));
        if !self.working_dir.is_empty() {
            out.push(("workdir".to_string(), self.working_dir.clone()));
        }
        out.push((
            "layers".to_string(),
            format!(
                "{} ({})",
                self.layers,
                function_wasm_engine_api::concurrency::format_bytes(self.size)
            ),
        ));
        out
    }

    /// Resolves `user` against the image's /etc/passwd and /etc/group
    /// (either may be absent: numbers then stand alone, and a name is a
    /// refusal), the way a container runtime does: `uid`, `uid:gid`,
    /// `name`, `name:group`, `uid:group`, `name:gid`; the supplementary
    /// groups are the groups that list the user's name, when it has one.
    pub fn resolve_user(
        &mut self,
        passwd: Option<&str>,
        group: Option<&str>,
    ) -> Result<(), String> {
        let (user_part, group_part) = match self.user.split_once(':') {
            Some((u, g)) => (u, Some(g)),
            None => (self.user.as_str(), None),
        };
        let mut name: Option<String> = None;
        let (uid, passwd_gid) = if user_part.is_empty() {
            (0, Some(0))
        } else if let Ok(n) = user_part.parse::<u32>() {
            let entry = passwd.and_then(|p| passwd_entry(p, |fields| fields[2] == n.to_string()));
            if let Some(e) = &entry {
                name = Some(e.0.clone());
            }
            (n, entry.map(|e| e.2))
        } else {
            let entry = passwd
                .and_then(|p| passwd_entry(p, |fields| fields[0] == user_part))
                .ok_or_else(|| {
                    format!("image user {user_part:?} is not in the image's /etc/passwd")
                })?;
            name = Some(entry.0.clone());
            (entry.1, Some(entry.2))
        };
        let gid = match group_part {
            None => passwd_gid.unwrap_or(0),
            Some("") => passwd_gid.unwrap_or(0),
            Some(g) => match g.parse::<u32>() {
                Ok(n) => n,
                Err(_) => group
                    .and_then(|text| group_entry(text, g))
                    .ok_or_else(|| format!("image group {g:?} is not in the image's /etc/group"))?,
            },
        };
        let mut groups = Vec::new();
        // A group given explicitly stands alone, as for a container.
        if group_part.is_none()
            && let (Some(name), Some(text)) = (&name, group)
        {
            for line in text.lines() {
                let fields: Vec<&str> = line.split(':').collect();
                if fields.len() < 4 {
                    continue;
                }
                let Ok(g) = fields[2].parse::<u32>() else {
                    continue;
                };
                if fields[3].split(',').any(|m| m == name) && g != gid {
                    groups.push(g);
                }
            }
        }
        groups.sort_unstable();
        groups.dedup();
        self.uid = uid;
        self.gid = gid;
        self.groups = groups;
        Ok(())
    }
}

/// (name, uid, gid) of the first passwd line the predicate accepts.
fn passwd_entry(text: &str, pred: impl Fn(&[&str]) -> bool) -> Option<(String, u32, u32)> {
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 4 || !pred(&fields) {
            continue;
        }
        if let (Ok(uid), Ok(gid)) = (fields[2].parse(), fields[3].parse()) {
            return Some((fields[0].to_string(), uid, gid));
        }
    }
    None
}

fn group_entry(text: &str, name: &str) -> Option<u32> {
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 3 && fields[0] == name {
            return fields[2].parse().ok();
        }
    }
    None
}

/// The image architecture of this host, as an OCI config spells it.
pub fn host_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn shell_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            if w.is_empty() || w.contains(char::is_whitespace) {
                format!("{w:?}")
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000::/home/app:/bin/sh\n";
    const GROUP: &str = "root:x:0:\napp:x:1000:\naudio:x:29:app,other\nvideo:x:44:app\n";

    fn config(user: &str) -> ImageConfig {
        ImageConfig {
            user: user.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn resolves_users_like_a_container_runtime() {
        let mut c = config("");
        c.resolve_user(Some(PASSWD), Some(GROUP)).unwrap();
        assert_eq!((c.uid, c.gid, c.groups.clone()), (0, 0, vec![]));
        let mut c = config("app");
        c.resolve_user(Some(PASSWD), Some(GROUP)).unwrap();
        assert_eq!((c.uid, c.gid, c.groups.clone()), (1000, 1000, vec![29, 44]));
        let mut c = config("1000");
        c.resolve_user(Some(PASSWD), Some(GROUP)).unwrap();
        assert_eq!((c.uid, c.gid, c.groups.clone()), (1000, 1000, vec![29, 44]));
        let mut c = config("app:audio");
        c.resolve_user(Some(PASSWD), Some(GROUP)).unwrap();
        assert_eq!((c.uid, c.gid, c.groups.clone()), (1000, 29, vec![]));
        let mut c = config("65532:65532");
        c.resolve_user(None, None).unwrap();
        assert_eq!((c.uid, c.gid), (65532, 65532));
        let mut c = config("2000");
        c.resolve_user(Some(PASSWD), Some(GROUP)).unwrap();
        assert_eq!((c.uid, c.gid), (2000, 0));
        assert_eq!(
            config("nobody")
                .resolve_user(Some(PASSWD), Some(GROUP))
                .unwrap_err(),
            "image user \"nobody\" is not in the image's /etc/passwd"
        );
        assert_eq!(
            config("app:nogroup")
                .resolve_user(Some(PASSWD), Some(GROUP))
                .unwrap_err(),
            "image group \"nogroup\" is not in the image's /etc/group"
        );
    }

    #[test]
    fn the_contract_needs_a_command_on_this_platform() {
        let c = ImageConfig::parse(br#"{"architecture":"amd64","os":"linux","config":{"Entrypoint":["/app"],"Cmd":["--flag"],"Env":["A=1"],"User":"app","WorkingDir":"/work"}}"#).unwrap();
        assert_eq!(c.argv(), vec!["/app", "--flag"]);
        let empty = ImageConfig::parse(br#"{"os":"linux","config":{}}"#).unwrap();
        assert!(
            empty
                .check()
                .unwrap_err()
                .starts_with("image has neither an entrypoint nor a command")
        );
        let windows = ImageConfig::parse(br#"{"os":"windows","config":{"Cmd":["x"]}}"#).unwrap();
        assert!(windows.check().unwrap_err().contains("linux images only"));
        let other =
            ImageConfig::parse(br#"{"os":"linux","architecture":"s390x","config":{"Cmd":["x"]}}"#)
                .unwrap();
        assert!(other.check().unwrap_err().contains("built for linux/s390x"));
    }

    #[test]
    fn the_environment_layers_path_the_image_and_the_bindings() {
        let c = ImageConfig {
            env: vec!["A=image".into(), "B=image".into()],
            ..Default::default()
        };
        let mut bindings = BTreeMap::new();
        bindings.insert("B".to_string(), "bound".to_string());
        bindings.insert("C".to_string(), "bound".to_string());
        assert_eq!(
            c.environment(&bindings),
            vec![
                "A=image".to_string(),
                "B=bound".to_string(),
                "C=bound".to_string(),
                format!("PATH={DEFAULT_PATH}")
            ]
        );
        let with_path = ImageConfig {
            env: vec!["PATH=/app/bin".into()],
            ..Default::default()
        };
        assert_eq!(
            with_path.environment(&BTreeMap::new()),
            vec!["PATH=/app/bin".to_string()]
        );
    }
}
