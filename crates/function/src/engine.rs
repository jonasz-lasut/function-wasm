//! The engine behind the runtime, behind the `Engine` trait of
//! guestfn-engine: what `runner.rs` compiles, inspects and runs
//! modules through, what `cache.rs` keeps and what `validate.rs` reads. A
//! Cargo feature selects the one engine a package serves with (`wasm`:
//! Wasmtime; `oci`: the OCI engine of docs/one-pager-oci-runtime.md, built
//! with --no-default-features --features oci; each the engine crate's
//! feature of the same name), and the rest of the crate
//! names it only as `Selected`, so a second engine is a second feature and
//! no other change here. With both features on, which is what the lint's
//! --all-features does, wasm serves. The engine's own flags, if any, are
//! `EngineArgs`, flattened into serve's and validate's.

pub use guestfn_engine::{Config, Engine, Error, Inspection, RunOptions};

#[cfg(feature = "wasm")]
pub use guestfn_engine::Wasmtime as Selected;

#[cfg(all(feature = "oci", not(feature = "wasm")))]
pub use guestfn_engine::oci::Oci as Selected;

#[cfg(not(any(feature = "wasm", feature = "oci")))]
compile_error!("function-wasm serves with one engine: build with the `wasm` or the `oci` feature");

/// A compiled module of the selected engine.
pub type Module = <Selected as Engine>::Module;

/// The Wasmtime engine takes nothing beyond the ceilings.
#[cfg(feature = "wasm")]
#[derive(clap::Args, Debug, Default, Clone)]
pub struct EngineArgs {}

/// The OCI engine's flags: the VM pieces the package image puts under
/// /usr/lib/guestfn, and the vCPUs per VM (docs/oci/install.md).
#[cfg(all(feature = "oci", not(feature = "wasm")))]
#[derive(clap::Args, Debug, Clone)]
pub struct EngineArgs {
    /// The Firecracker binary that boots a microVM per request.
    #[arg(
        long,
        default_value = "/usr/lib/guestfn/firecracker",
        env = "GUESTFN_VMM"
    )]
    pub vmm: std::path::PathBuf,

    /// The guest kernel, an uncompressed vmlinux.
    #[arg(
        long,
        default_value = "/usr/lib/guestfn/vmlinux",
        env = "GUESTFN_KERNEL"
    )]
    pub guest_kernel: std::path::PathBuf,

    /// The init the runtime boots as the VM's first process.
    #[arg(long, default_value = "/usr/lib/guestfn/init", env = "GUESTFN_INIT")]
    pub guest_init: std::path::PathBuf,

    /// vCPUs per VM.
    #[arg(long, default_value_t = 1, env = "GUESTFN_VM_VCPUS")]
    pub vm_vcpus: u8,

    /// Restore a VM snapshot per image in place of a boot: the first run
    /// of an image boots and snapshots the VM with its init waiting, every
    /// later run restores it into a fresh VM. A prototype (#255), off by
    /// default: the snapshots live in the engine's scratch directory for
    /// the process's life.
    #[arg(long, env = "GUESTFN_VM_SNAPSHOTS")]
    pub vm_snapshots: bool,
}

#[cfg(all(feature = "oci", not(feature = "wasm")))]
impl Default for EngineArgs {
    fn default() -> Self {
        EngineArgs {
            vmm: "/usr/lib/guestfn/firecracker".into(),
            guest_kernel: "/usr/lib/guestfn/vmlinux".into(),
            guest_init: "/usr/lib/guestfn/init".into(),
            vm_vcpus: 1,
            vm_snapshots: false,
        }
    }
}

/// The selected engine under the ceilings and its own flags.
#[cfg(feature = "wasm")]
pub fn new(config: Config, _args: &EngineArgs) -> Result<Selected, Error> {
    Selected::new(config)
}

#[cfg(all(feature = "oci", not(feature = "wasm")))]
pub fn new(config: Config, args: &EngineArgs) -> Result<Selected, Error> {
    Selected::new(
        config,
        guestfn_engine::oci::Options {
            vmm: args.vmm.clone(),
            kernel: args.guest_kernel.clone(),
            init: args.guest_init.clone(),
            vcpus: args.vm_vcpus,
            snapshots: args.vm_snapshots,
        },
    )
}
