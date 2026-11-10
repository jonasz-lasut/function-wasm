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
        env = "VMM_BINARY_PATH"
    )]
    vmm_binary_path: PathBuf,

    /// The guest kernel, an uncompressed vmlinux.
    #[arg(
        long,
        default_value = "/usr/lib/guestfn/vmlinux",
        env = "GUEST_KERNEL_PATH"
    )]
    guest_kernel_path: PathBuf,

    /// The init the runtime boots as the VM's first process.
    #[arg(long, default_value = "/usr/lib/guestfn/init", env = "GUEST_INIT_PATH")]
    guest_init_path: PathBuf,

    /// vCPUs per VM.
    #[arg(long, default_value_t = 1, env = "VM_VCPUS")]
    vm_vcpus: u8,

    /// Restore a VM snapshot per image in place of a boot: the runtime
    /// boots an image's VM once, at warm-up or at its first run, snapshots
    /// it with the init waiting, and restores that snapshot into a fresh
    /// VM for every later run. An alpha feature, off by default
    /// (docs/one-pager-vm-snapshots.md).
    #[arg(long, default_value_t = false, env = "ENABLE_VM_SNAPSHOTS", action = clap::ArgAction::Set)]
    enable_vm_snapshots: bool,
}

fn main() -> ExitCode {
    guestfn_runtime::cli::run(
        "A Crossplane composition function that runs OCI images in microVMs.",
        |config, args: &EngineArgs| {
            Oci::new(
                config,
                Options {
                    vmm: args.vmm_binary_path.clone(),
                    kernel: args.guest_kernel_path.clone(),
                    init: args.guest_init_path.clone(),
                    vcpus: args.vm_vcpus,
                    snapshots: args.enable_vm_snapshots,
                },
            )
        },
    )
}
