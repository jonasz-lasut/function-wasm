//! The seam between the engine and the VMM (docs/one-pager-oci-runtime.md,
//! "Sandbox evaluation"): what one run needs booted, and what came back.
//! One type per VMM implements it; `firecracker.rs` is the first.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use crate::protocol::{Description, Exit};
use crate::{ArtifactStore, Error};

/// A VM frozen with its init parked: the state file and the memory file
/// the VMM wrote, which a later run of the same image restores.
#[derive(Debug)]
pub struct Snapshot {
    pub state: PathBuf,
    pub mem: PathBuf,
}

/// One machine shape's snapshot once made. The write side is held while
/// the snapshot is made and the read side while a run looks it up, so
/// concurrent first runs make one and runs restoring it never meet.
pub type SnapshotSlot = Arc<RwLock<Option<Arc<Snapshot>>>>;

/// The snapshots of one loaded image by machine shape (the VM's memory in
/// MiB and its vCPUs): a snapshot restores only into the shape it came
/// from. The memo lives and dies with the image in memory, as the
/// verified root filesystem does: an image loaded again starts empty and
/// takes its snapshots from the store, verified, or makes them.
#[derive(Debug, Default)]
pub struct SnapshotMemo {
    slots: Mutex<HashMap<(u64, u8), SnapshotSlot>>,
    off: AtomicBool,
}

impl SnapshotMemo {
    pub fn slot(&self, memory_mib: u64, vcpus: u8) -> SnapshotSlot {
        let mut slots = self.slots.lock().expect("poisoned");
        Arc::clone(slots.entry((memory_mib, vcpus)).or_default())
    }

    /// Stops snapshots for this image until it is loaded again: one that
    /// could not be made or restored costs its run a boot, not every run
    /// after it another try.
    pub fn turn_off(&self) {
        self.off.store(true, Ordering::Relaxed);
    }

    pub fn is_off(&self) -> bool {
        self.off.load(Ordering::Relaxed)
    }
}

/// One VM to boot and drive through the init's protocol.
#[derive(Clone, Copy)]
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
    /// The image's snapshots, for a sandbox that restores one in place of
    /// a boot.
    pub snapshots: &'a SnapshotMemo,
    /// Where a snapshot is kept beside the compiled artifacts, when the
    /// root filesystem is one of them. None keeps it under scratch for
    /// the life of the process.
    pub store: Option<&'a dyn ArtifactStore>,
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

    /// Makes ready what a later run of this VM would otherwise make
    /// first: a sandbox that restores snapshots takes the snapshot here.
    /// The spec's description and request say nothing yet. The default
    /// does nothing.
    fn prepare(&self, _spec: VmSpec<'_>) -> Result<(), Error> {
        Ok(())
    }
}
