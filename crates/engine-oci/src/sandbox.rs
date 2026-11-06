//! The seam between the engine and the VMM (docs/one-pager-oci-runtime.md,
//! "Sandbox evaluation"): what one run needs booted, and what came back.
//! One type per VMM implements it; `firecracker.rs` is the first.

use std::path::Path;
use std::time::Instant;

use function_wasm_engine_api::Error;
use guestfn_init::{Description, Exit};

/// One VM to boot and drive through the init's protocol.
pub struct VmSpec<'a> {
    pub kernel: &'a Path,
    pub initrd: &'a Path,
    /// The image's root filesystem, a squashfs the VM mounts read-only.
    pub rootfs: &'a Path,
    pub memory_mib: u64,
    pub vcpus: u8,
    pub description: &'a Description,
    pub request: &'a [u8],
    /// When the run must be over: the sandbox stops the VM at it.
    pub deadline: Instant,
    /// Where the sandbox keeps the run's sockets and files, removed by
    /// the caller after the run.
    pub scratch: &'a Path,
}

/// How a run ended.
#[derive(Debug)]
pub enum Ended {
    /// The init reported the process's exit.
    Exit(Exit),
    /// The deadline passed first; the VM was stopped.
    Deadline,
    /// The VM or the connection ended before the init reported, with what
    /// the sandbox saw.
    Lost(String),
}

#[derive(Debug)]
pub struct Outcome {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub end: Ended,
    /// The serial console's output: the kernel's and the init's own
    /// lines, for the debug log.
    pub console: String,
}

pub trait Sandbox: Send + Sync + 'static {
    /// The VMM's name, for logs.
    fn name(&self) -> &'static str;
    /// Boots the VM, drives the run through the protocol and stops the VM
    /// whatever happened; an Err is a failure of the sandbox itself (the
    /// VMM would not start), not of the process.
    fn run(&self, spec: VmSpec<'_>) -> Result<Outcome, Error>;
}
