//! The OCI engine of the `function-oci` runtime (docs/one-pager-oci-runtime.md),
//! behind the crate's `Engine` trait as the Wasmtime engine is: a module is an OCI image, handed over as an image layout tar
//! (`layout.rs`), compiled by unpacking its layers into a squashfs root
//! filesystem with the image config inside it (`rootfs.rs`, `image.rs`),
//! stored and loaded as that one file, and run once per request in a
//! microVM the `Sandbox` seam boots (`sandbox.rs`; Firecracker in
//! `firecracker.rs`), where the init the engine packs into an initramfs
//! (`initrd.rs`, the `guestfn-init` binary) runs the image's process under
//! the process contract: the request on stdin, the response on stdout,
//! logs on stderr, the exit status the verdict (`run.rs`).

pub mod firecracker;
pub mod image;
pub mod initrd;
pub mod layout;
pub mod rootfs;
mod run;
pub mod sandbox;
#[cfg(any(test, feature = "testutil"))]
pub mod testing;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use crate::{
    self as api, Config, Engine, Error, Extern, HttpRequester, Inspection, RunOptions, concurrency,
    duration, metrics, wire,
};
pub use image::ImageConfig;
pub use sandbox::Sandbox;

/// What the engine needs on the node besides the Config's ceilings.
#[derive(Debug, Clone)]
pub struct Options {
    /// The Firecracker binary.
    pub vmm: PathBuf,
    /// The guest kernel, an uncompressed vmlinux.
    pub kernel: PathBuf,
    /// The guest init, a static binary the engine packs into the initramfs.
    pub init: PathBuf,
    /// vCPUs per VM.
    pub vcpus: u8,
    /// Restore a snapshot per image in place of a boot (#255, a
    /// prototype): the first run of an image boots and snapshots the VM
    /// with its init parked, every later run restores it.
    pub snapshots: bool,
}

/// The device every VM needs; the engine refuses to start without it.
pub const KVM_DEVICE: &str = "/dev/kvm";

/// A compiled module: the root filesystem on disk and the config read
/// from inside it.
#[derive(Debug)]
pub struct Image {
    pub rootfs: Rootfs,
    pub config: ImageConfig,
}

/// Where a root filesystem lives: a file the engine wrote under its
/// scratch directory (removed with the last clone), or one the runtime's
/// compiled store holds.
#[derive(Debug)]
pub enum Rootfs {
    Scratch(tempfile::TempPath),
    Stored(PathBuf),
}

impl Rootfs {
    pub fn path(&self) -> &Path {
        match self {
            Rootfs::Scratch(p) => p,
            Rootfs::Stored(p) => p,
        }
    }
}

/// The module type the trait hands around: an Arc, so a run and the cache
/// share one file.
pub type Module = Arc<Image>;

pub struct Oci {
    config: Config,
    options: Options,
    initrd: tempfile::TempPath,
    scratch: tempfile::TempDir,
    scheduler: Option<concurrency::FairScheduler>,
    mem: Option<Arc<concurrency::MemPool>>,
    sandbox: Box<dyn Sandbox>,
}

impl Oci {
    /// The engine over Firecracker: the files checked, the initramfs
    /// packed, and `/dev/kvm` opened once so a node without it fails at
    /// startup with one sentence rather than at the first request.
    pub fn new(config: Config, options: Options) -> Result<Oci, Error> {
        let sandbox = Box::new(if options.snapshots {
            firecracker::Firecracker::with_snapshots(options.vmm.clone())
        } else {
            firecracker::Firecracker::new(options.vmm.clone())
        });
        for (what, path) in [("--vmm", &options.vmm), ("--guest-kernel", &options.kernel)] {
            if !path.is_file() {
                return Err(Error(format!(
                    "{what} {} is not a file: the OCI runtime's image carries it under /usr/lib/guestfn",
                    path.display()
                )));
            }
        }
        if let Err(e) = std::fs::File::open(KVM_DEVICE) {
            return Err(Error(format!(
                "cannot open {KVM_DEVICE}: {e}; the OCI runtime boots a microVM per request and needs the device granted to its pod (docs/oci/install.md)"
            )));
        }
        Self::with_sandbox(config, options, sandbox)
    }

    /// The engine over any sandbox (tests pass a fake).
    pub fn with_sandbox(
        config: Config,
        options: Options,
        sandbox: Box<dyn Sandbox>,
    ) -> Result<Oci, Error> {
        if config.max_total_run_memory > 0 && config.max_total_run_memory < config.memory_limit {
            return Err(Error(format!(
                "--max-total-run-memory {} is smaller than the per-run ceiling {} (--module-memory-limit): no full-limit run could ever reserve its memory",
                concurrency::format_bytes(config.max_total_run_memory),
                concurrency::format_bytes(config.memory_limit)
            )));
        }
        if options.vcpus == 0 {
            return Err(Error("--vm-vcpus must be at least 1".to_string()));
        }
        let init = std::fs::read(&options.init).map_err(|e| {
            Error(format!(
                "cannot read the guest init --guest-init {}: {e}",
                options.init.display()
            ))
        })?;
        let scratch = tempfile::Builder::new()
            .prefix("guestfn-oci-")
            .tempdir()
            .map_err(|e| Error(format!("cannot create the engine's scratch directory: {e}")))?;
        let mut initrd = tempfile::Builder::new()
            .prefix("initrd-")
            .tempfile_in(scratch.path())
            .map_err(|e| Error(format!("cannot write the initramfs: {e}")))?;
        std::io::Write::write_all(&mut initrd, &initrd::pack(&init))
            .map_err(|e| Error(format!("cannot write the initramfs: {e}")))?;
        Ok(Oci {
            scheduler: (config.max_concurrent_runs > 0)
                .then(|| concurrency::FairScheduler::new(config.max_concurrent_runs)),
            mem: (config.max_total_run_memory > 0)
                .then(|| Arc::new(concurrency::MemPool::new(config.max_total_run_memory))),
            config,
            options,
            initrd: initrd.into_temp_path(),
            scratch,
            sandbox,
        })
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    /// The run's ceilings narrowed by its options.
    pub(crate) fn effective(&self, opts: &RunOptions) -> Config {
        let mut cfg = self.config;
        if let Some(t) = opts.timeout
            && t < cfg.timeout
        {
            cfg.timeout = t;
        }
        if let Some(m) = opts.memory_limit
            && m < cfg.memory_limit
        {
            cfg.memory_limit = m;
        }
        cfg
    }

    /// Unpacks an image layout into a root filesystem file under the
    /// scratch directory, the config resolved and embedded.
    fn build(&self, bytes: &[u8]) -> Result<Image, Error> {
        let start = std::time::Instant::now();
        let layout = layout::parse(bytes)?;
        let mut config = ImageConfig::parse(&layout.config)?;
        let mut tree = rootfs::Tree::new();
        let mut size = 0u64;
        for (i, layer) in layout.layers.iter().enumerate() {
            tree.apply_layer(&layer.descriptor.media_type, &layer.bytes)
                .map_err(|e| {
                    Error(format!(
                        "cannot unpack layer {i} ({}): {e}",
                        layer.descriptor.digest
                    ))
                })?;
            size += layer.bytes.len() as u64;
        }
        config.layers = layout.layers.len();
        config.size = size;
        config
            .resolve_user(
                tree.text("etc/passwd").as_deref(),
                tree.text("etc/group").as_deref(),
            )
            .map_err(Error)?;
        config.check().map_err(Error)?;
        let config_json = serde_json::to_vec(&config).map_err(|e| Error(e.to_string()))?;
        let mut file = tempfile::Builder::new()
            .prefix("rootfs-")
            .tempfile_in(self.scratch.path())
            .map_err(|e| Error(format!("cannot create the root filesystem file: {e}")))?;
        tree.write_squashfs(&[(rootfs::CONFIG_PATH, &config_json)], &mut file)?;
        metrics::COMPILE_DURATION.observe(start.elapsed().as_secs_f64());
        Ok(Image {
            rootfs: Rootfs::Scratch(file.into_temp_path()),
            config,
        })
    }
}

impl Engine for Oci {
    const KIND: &'static str = "oci";
    const CONTRACT: &'static str = "oci@1.0";
    type Module = Module;

    fn config(&self) -> Config {
        self.config
    }

    fn compile(&self, bytes: &[u8]) -> Result<Module, Error> {
        Ok(Arc::new(self.build(bytes)?))
    }

    fn inspect(&self, bytes: &[u8]) -> Result<Inspection, Error> {
        let layout = layout::parse(bytes)?;
        let mut config = ImageConfig::parse(&layout.config)?;
        config.layers = layout.layers.len();
        config.size = layout.layers.iter().map(|l| l.bytes.len() as u64).sum();
        // The user is resolved from the layers at compile; inspect reads
        // the config alone and reports the verdict the contract gives it.
        Ok(Inspection {
            abi_error: config.check().err(),
            details: config.details(),
            ..Default::default()
        })
    }

    fn serialize(&self, m: &Module) -> Result<Vec<u8>, Error> {
        std::fs::read(m.rootfs.path())
            .map_err(|e| Error(format!("cannot read the root filesystem: {e}")))
    }

    fn deserialize_file(&self, path: &Path) -> Result<Module, Error> {
        let raw = rootfs::read_file(path, rootfs::CONFIG_PATH)?;
        let config: ImageConfig = serde_json::from_slice(&raw)
            .map_err(|e| Error(format!("cannot parse the stored image config: {e}")))?;
        Ok(Arc::new(Image {
            rootfs: Rootfs::Stored(path.to_path_buf()),
            config,
        }))
    }

    fn run(&self, m: &Module, request: &[u8], opts: RunOptions) -> Result<Vec<u8>, Error> {
        run::run(self, m, request, opts)
    }

    fn version() -> String {
        version()
    }
}

/// What namespaces the compiled store: the root filesystem's format is
/// this crate's, so its version and the platform.
pub fn version() -> String {
    format!(
        "oci-v{}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::sandbox::{Ended, Outcome, VmSpec};
    use crate::oci::testing::{FakeSandbox, fixture_image};
    use crate::protocol::Exit;

    fn engine(sandbox: FakeSandbox) -> Oci {
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join("init");
        std::fs::write(&init, b"#!/bin/sh\n").unwrap();
        let options = Options {
            vmm: dir.path().join("firecracker"),
            kernel: dir.path().join("vmlinux"),
            init,
            vcpus: 1,
            snapshots: false,
        };
        let e = Oci::with_sandbox(Config::default(), options, Box::new(sandbox)).unwrap();
        // The temp dir goes; the engine read the init already.
        std::mem::forget(dir);
        e
    }

    #[test]
    fn compiles_inspects_stores_and_runs_an_image() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fake = FakeSandbox::new(Arc::clone(&seen), |spec: &VmSpec<'_>| Outcome {
            stdout: spec.request.to_vec(),
            stderr: b"a line\n{\"level\":\"warn\",\"msg\":\"typed\",\"k\":1}\n".to_vec(),
            end: Ended::Exit(Exit {
                status: Some(0),
                ..Default::default()
            }),
            console: String::new(),
        });
        let e = engine(fake);
        let image = fixture_image(
            b"#!/bin/sh\necho hi\n",
            &["/app/run", "--flag"],
            "app",
            &["A=1"],
        );
        let shape = e.inspect(&image).unwrap();
        assert_eq!(shape.abi_error, None);
        assert!(
            shape
                .details
                .iter()
                .any(|(k, v)| k == "entrypoint" && v == "/app/run --flag")
        );
        let m = e.compile(&image).unwrap();
        assert_eq!((m.config.uid, m.config.gid), (1000, 1000));
        assert_eq!(m.config.argv(), vec!["/app/run", "--flag"]);
        // Through the store and back.
        let bytes = e.serialize(&m).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let stored = dir.path().join("digest");
        std::fs::write(&stored, &bytes).unwrap();
        let loaded = e.deserialize_file(&stored).unwrap();
        assert_eq!(loaded.config, m.config);
        let mut opts = RunOptions::default();
        opts.env.insert("B".to_string(), "bound".to_string());
        opts.module = "oci example".to_string();
        let out = e.run(&loaded, b"request bytes", opts).unwrap();
        assert_eq!(out, b"request bytes");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].argv, vec!["/app/run", "--flag"]);
        assert!(seen[0].env.contains(&"A=1".to_string()));
        assert!(seen[0].env.contains(&"B=bound".to_string()));
        assert_eq!((seen[0].uid, seen[0].gid), (1000, 1000));
    }

    #[test]
    fn the_endings_become_the_contracts_fatals() {
        let cases: Vec<(Ended, Vec<u8>, &str)> = vec![
            (
                Ended::Deadline,
                vec![],
                "run failed: module exceeded its execution deadline (30s)",
            ),
            (
                Ended::Lost("the VMM exited".into()),
                vec![],
                "run failed: the VMM exited",
            ),
            (
                Ended::Exit(Exit {
                    status: Some(3),
                    ..Default::default()
                }),
                b"warming up\nout of ideas\n".to_vec(),
                "process exited with status 3: out of ideas",
            ),
            (
                Ended::Exit(Exit {
                    signal: Some(9),
                    oom: true,
                    ..Default::default()
                }),
                vec![],
                "process was killed: the run's memory limit of 512Mi was exceeded",
            ),
            (
                Ended::Exit(Exit {
                    signal: Some(11),
                    ..Default::default()
                }),
                vec![],
                "process was killed by signal 11",
            ),
            (
                Ended::Exit(Exit {
                    error: "cannot start /app/run: No such file".into(),
                    ..Default::default()
                }),
                vec![],
                "cannot start the process: cannot start /app/run: No such file",
            ),
            (
                Ended::Exit(Exit {
                    status: Some(0),
                    ..Default::default()
                }),
                vec![],
                "process exited with status 0 and wrote no response to its standard output",
            ),
        ];
        for (end, stderr, want) in cases {
            let end = std::sync::Mutex::new(Some(end));
            let fake = FakeSandbox::new(
                Arc::new(std::sync::Mutex::new(Vec::new())),
                move |_: &VmSpec<'_>| Outcome {
                    stdout: Vec::new(),
                    stderr: stderr.clone(),
                    end: end.lock().unwrap().take().unwrap(),
                    console: String::new(),
                },
            );
            let e = engine(fake);
            let m = e.compile(&fixture_image(b"x", &["/x"], "", &[])).unwrap();
            let err = e.run(&m, b"r", RunOptions::default()).unwrap_err();
            assert_eq!(err.0, want);
        }
    }

    #[test]
    fn an_image_without_a_command_or_user_is_refused_at_compile() {
        let e = engine(FakeSandbox::new(
            Arc::new(std::sync::Mutex::new(Vec::new())),
            |_: &VmSpec<'_>| unreachable!(),
        ));
        let no_cmd = fixture_image(b"x", &[], "", &[]);
        assert!(
            e.inspect(&no_cmd)
                .unwrap()
                .abi_error
                .unwrap()
                .starts_with("image has neither an entrypoint")
        );
        assert!(
            e.compile(&no_cmd)
                .unwrap_err()
                .0
                .starts_with("image has neither an entrypoint")
        );
        let nobody = fixture_image(b"x", &["/x"], "nobody", &[]);
        assert_eq!(
            e.compile(&nobody).unwrap_err().0,
            "image user \"nobody\" is not in the image's /etc/passwd"
        );
        assert!(e.compile(b"garbage").is_err());
    }
}
