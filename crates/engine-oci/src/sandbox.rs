//! The seam between the engine and the VMM (docs/one-pager-oci-runtime.md,
//! "Sandbox evaluation"): what one run needs booted, and what came back.
//! One type per VMM implements it; `firecracker.rs` is the first.

use std::collections::{HashMap, VecDeque};
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
    /// The directory of both files when the sandbox keeps them under the
    /// engine's scratch directory, which no sweep bounds: it goes with the
    /// snapshot, so with the loaded image that remembers it. None for a
    /// snapshot in a store, whose files are the store's to remove.
    pub scratch: Option<PathBuf>,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if let Some(dir) = &self.scratch {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
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
    /// The function answered the call: the response's bytes, as they came.
    Answered(Vec<u8>),
    /// The function answered the call with a gRPC status other than OK.
    Refused { code: String, message: String },
    /// The init reported the process's exit, before an answer.
    Exit(Exit),
    /// The deadline passed first; the VM was stopped.
    Deadline,
    /// The VM or the connection ended before the init reported, with what
    /// the sandbox saw.
    Lost(String),
}

/// The most of a process's output the host keeps for one run. The output
/// becomes log lines and nothing else, so the bound is sized for a log: a
/// function that reports what it did writes a few kilobytes, and 256 KiB
/// is some thousands of lines, four times what the VM's console is kept
/// to. It is far below the response's bound because a response is the
/// run's result and this is not: every concurrent run holds one of these,
/// and all of it goes to the pod's log.
pub const OUTPUT_CAP: usize = 256 << 10;

/// The end of what a process wrote: the last bytes up to a bound, and how
/// many went before them. The end is what is kept because the last line
/// is what a fatal result quotes and what says why a process stopped.
#[derive(Debug)]
pub struct Output {
    tail: VecDeque<u8>,
    cap: usize,
    dropped: u64,
    /// The last byte dropped: a newline means the kept bytes start a line.
    last_dropped: Option<u8>,
}

impl Default for Output {
    fn default() -> Output {
        Output::with_cap(OUTPUT_CAP)
    }
}

impl From<&[u8]> for Output {
    fn from(bytes: &[u8]) -> Output {
        let mut output = Output::default();
        output.push(bytes);
        output
    }
}

impl Output {
    pub fn with_cap(cap: usize) -> Output {
        Output {
            tail: VecDeque::new(),
            cap,
            dropped: 0,
            last_dropped: None,
        }
    }

    /// Appends what the process wrote next, letting go of the oldest bytes
    /// past the bound.
    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.cap {
            let kept_from = bytes.len() - self.cap;
            self.last_dropped = match kept_from {
                0 => self.tail.back().copied().or(self.last_dropped),
                n => Some(bytes[n - 1]),
            };
            self.dropped += (self.tail.len() + kept_from) as u64;
            self.tail.clear();
            self.tail.extend(&bytes[kept_from..]);
            return;
        }
        let over = (self.tail.len() + bytes.len()).saturating_sub(self.cap);
        if over > 0 {
            self.last_dropped = Some(self.tail[over - 1]);
            self.tail.drain(..over);
            self.dropped += over as u64;
        }
        self.tail.extend(bytes);
    }

    /// The bytes kept.
    pub fn len(&self) -> usize {
        self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tail.is_empty()
    }

    /// The kept output as text and the bytes dropped before it. Output
    /// that lost its beginning starts on a whole line: when the cut fell
    /// inside a line, the rest of that line is dropped too, unless it is
    /// all there is. Bytes that are not UTF-8, a character the cut split among
    /// them, read as the replacement character.
    pub fn into_text(self) -> (String, u64) {
        let text = String::from_utf8(Vec::from(self.tail))
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        if self.dropped == 0 || self.last_dropped == Some(b'\n') {
            return (text, self.dropped);
        }
        match text.split_once('\n') {
            Some((cut, rest)) if !rest.trim().is_empty() => {
                (rest.to_string(), self.dropped + cut.len() as u64 + 1)
            }
            _ => (text, self.dropped),
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    /// What the process wrote to its standard output and standard error:
    /// the end of it, when there was more than a run keeps.
    pub log: Output,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(cap: usize, chunks: &[&[u8]]) -> Output {
        let mut output = Output::with_cap(cap);
        for chunk in chunks {
            output.push(chunk);
        }
        output
    }

    #[test]
    fn output_keeps_its_end_and_counts_what_went_before() {
        type Case<'a> = (&'a str, usize, &'a [&'a [u8]], &'a str, u64);
        let cases: &[Case<'_>] = &[
            ("nothing written", 8, &[], "", 0),
            ("under the bound", 8, &[b"ab\n", b"cd\n"], "ab\ncd\n", 0),
            (
                "exactly the bound",
                8,
                &[b"abc\n", b"def\n"],
                "abc\ndef\n",
                0,
            ),
            (
                "over the bound, the cut on a line's end",
                8,
                &[b"abc\n", b"def\n", b"ghi\n"],
                "def\nghi\n",
                4,
            ),
            (
                "over the bound, the cut inside a line",
                8,
                &[b"abcdef\n", b"gh\n", b"ij\n"],
                "gh\nij\n",
                7,
            ),
            (
                "one chunk larger than the bound",
                4,
                &[b"ab\n", b"0123456\nxy\n"],
                "xy\n",
                11,
            ),
            (
                "a chunk of the bound's size after another",
                4,
                &[b"ab\n", b"cde\n"],
                "cde\n",
                3,
            ),
            (
                "one line longer than the bound",
                4,
                &[b"abcdefgh"],
                "efgh",
                4,
            ),
            (
                "the cut line is the only one with words",
                6,
                &[b"abcdefgh\n\n"],
                "efgh\n\n",
                4,
            ),
        ];
        for (name, cap, chunks, want, dropped) in cases {
            let got = filled(*cap, chunks).into_text();
            assert_eq!(got, (want.to_string(), *dropped), "{name}");
        }
    }

    #[test]
    fn output_is_never_larger_than_its_bound() {
        let mut output = Output::with_cap(1000);
        let mut written = 0u64;
        for n in 0..500usize {
            let chunk = vec![b'a' + (n % 26) as u8; n * 7 % 1300];
            written += chunk.len() as u64;
            output.push(&chunk);
            assert!(output.len() <= 1000);
        }
        let kept = output.len() as u64;
        assert!(!output.is_empty());
        let (_, dropped) = output.into_text();
        assert_eq!(kept + dropped, written);
    }

    #[test]
    fn a_character_split_by_the_cut_reads_as_a_replacement() {
        // Two bytes each: a bound of five cuts the first kept one in half.
        let (text, dropped) = filled(5, &["\u{e9}\u{e9}\u{e9}\u{e9}".as_bytes()]).into_text();
        assert_eq!(text, "\u{fffd}\u{e9}\u{e9}");
        assert_eq!(dropped, 3);
        let (text, dropped) = filled(8, &[b"\xff\xfe ok"]).into_text();
        assert_eq!(text, "\u{fffd}\u{fffd} ok");
        assert_eq!(dropped, 0);
    }
}
