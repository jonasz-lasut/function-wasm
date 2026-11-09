//! The function-oci package's binary: the guestfn runtime over the OCI
//! engine of docs/one-pager-oci-runtime.md.

use std::path::PathBuf;
use std::process::ExitCode;

use guestfn_engine_oci::{Oci, Options};

/// The OCI engine's flags: the VM pieces the package image puts under
/// /usr/lib/guestfn, and the vCPUs per VM (docs/oci/install.md).
#[derive(clap::Args, Debug)]
struct EngineArgs {
    /// The Firecracker binary that boots a microVM per request.
    #[arg(
        long,
        default_value = "/usr/lib/guestfn/firecracker",
        env = "GUESTFN_VMM"
    )]
    vmm: PathBuf,

    /// The guest kernel, an uncompressed vmlinux.
    #[arg(
        long,
        default_value = "/usr/lib/guestfn/vmlinux",
        env = "GUESTFN_KERNEL"
    )]
    guest_kernel: PathBuf,

    /// The init the runtime boots as the VM's first process.
    #[arg(long, default_value = "/usr/lib/guestfn/init", env = "GUESTFN_INIT")]
    guest_init: PathBuf,

    /// vCPUs per VM.
    #[arg(long, default_value_t = 1, env = "GUESTFN_VM_VCPUS")]
    vm_vcpus: u8,

    /// Restore a VM snapshot per image in place of a boot: the runtime
    /// boots an image's VM once, at warm-up or at its first run, snapshots
    /// it with the init waiting, and restores that snapshot into a fresh
    /// VM for every later run. An alpha feature, off by default
    /// (docs/one-pager-vm-snapshots.md).
    #[arg(long, env = "GUESTFN_ENABLE_VM_SNAPSHOTS")]
    enable_vm_snapshots: bool,
}

fn main() -> ExitCode {
    guestfn_runtime::cli::run(
        "A Crossplane composition function that runs OCI images in microVMs.",
        |config, args: &EngineArgs| {
            Oci::new(
                config,
                Options {
                    vmm: args.vmm.clone(),
                    kernel: args.guest_kernel.clone(),
                    init: args.guest_init.clone(),
                    vcpus: args.vm_vcpus,
                    snapshots: args.enable_vm_snapshots,
                },
            )
        },
    )
}
