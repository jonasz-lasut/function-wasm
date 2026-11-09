//! The protocol between the OCI runtime's engine (the host) and the init
//! it boots as the microVM's first process (`crates/oci-init`), over one vsock
//! connection the init opens to the host (docs/one-pager-oci-runtime.md,
//! "The sandbox"). Frames carry a kind byte, a big-endian length and the
//! payload. The init says hello; the host answers with the run's
//! description; the init starts the process and relays its stdout and
//! stderr as they arrive. When the function's server accepts connections
//! the init opens a second connection to the host and bridges it to that
//! server, and the host calls `RunFunction` over it. The host then says
//! finish, the init stops the process and reports its exit, and the
//! connection ends. Nothing here knows a VMM: the host side is any listener
//! the sandbox puts the guest's ports on (Unix sockets for Firecracker).

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// The vsock port the init connects to, on the host's well-known CID.
pub const PORT: u32 = 5000;
/// The vsock port the init bridges the function's server to.
pub const GRPC_PORT: u32 = 5001;
/// The port the contract says the function's server listens on, the one
/// Crossplane's function runtime uses.
pub const SERVER_PORT: u16 = 9443;
pub const HOST_CID: u32 = 2;

/// The protocol's version, in the init's hello: a host meeting another
/// refuses the run rather than guess.
pub const VERSION: u32 = 3;

/// Most bytes one frame carries; stdout and stderr travel in frames of at
/// most this size.
pub const MAX_FRAME: usize = 1 << 20;

/// The frame kinds. The numbers are the wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// init → host, first: the protocol version as a Hello.
    Hello = 1,
    /// host → init: the run's Description.
    Run = 2,
    /// init → host: a chunk of stdout.
    Stdout = 5,
    /// init → host: a chunk of stderr.
    Stderr = 6,
    /// init → host, last: the process's Exit.
    Exit = 7,
    /// host → init, in place of Run: the host is about to snapshot the VM.
    /// The init answers Parked and waits on this connection, which a
    /// restore resets: that reset is what wakes the init in a clone, and it
    /// then connects again and says hello as after a boot.
    Park = 8,
    /// init → host: the answer to Park. The init sends nothing more and
    /// does nothing until the connection ends, so the VM is still when
    /// the host freezes it.
    Parked = 9,
    /// host → init: the call has its answer. The init stops the process
    /// and reports its exit, which flushes what the process logged.
    Finish = 10,
}

impl Kind {
    pub fn from_byte(b: u8) -> Option<Kind> {
        Some(match b {
            1 => Kind::Hello,
            2 => Kind::Run,
            5 => Kind::Stdout,
            6 => Kind::Stderr,
            7 => Kind::Exit,
            8 => Kind::Park,
            9 => Kind::Parked,
            10 => Kind::Finish,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub version: u32,
}

/// What the init needs to run the image's process: the host resolves the
/// image config (entrypoint and cmd, user, working directory, env) and the
/// run's grants (the env bindings) into this before the VM boots, so the
/// init reads no config of its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Description {
    /// The command line: the image's entrypoint followed by its cmd, or by
    /// the manifest's `oci.args` where it sets them.
    pub argv: Vec<String>,
    /// `KEY=VALUE` entries, the image's environment plus the bindings the
    /// run was granted; PATH among them, which the init resolves argv[0]
    /// against.
    pub env: Vec<String>,
    /// The working directory, `/` when the image sets none.
    pub cwd: String,
    pub uid: u32,
    pub gid: u32,
    /// The supplementary groups, from the image's /etc/group.
    pub groups: Vec<u32>,
    pub hostname: String,
    /// The process's memory limit in bytes, the step's `limits.memory`:
    /// the init runs the process in a cgroup capped at it, so the kernel's
    /// out-of-memory killer ends a process past it at once rather than
    /// the VM thrashing its page cache first. 0 is no cap.
    #[serde(default)]
    pub memory_max: u64,
    /// The host's wall clock in nanoseconds since the epoch, for an init
    /// that was parked: a restored clone's clock stands at the snapshot's
    /// moment until the init sets it. 0 leaves the clock alone.
    #[serde(default)]
    pub now_unix_ns: u64,
    /// Fresh randomness from the host as hex, for an init that was parked:
    /// every clone of one snapshot starts with the same kernel random
    /// state, which the init reseeds with this before the process runs.
    #[serde(default)]
    pub seed: String,
}

/// How the process ended.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Exit {
    /// The exit status when the process exited.
    pub status: Option<i32>,
    /// The signal when it was killed by one.
    pub signal: Option<i32>,
    /// Whether the kernel's out-of-memory killer took it: the guest's
    /// memory is the run's memory limit.
    pub oom: bool,
    /// What the init could not do, when it could not run the process at
    /// all (an entrypoint that does not exist, a user it cannot become).
    pub error: String,
}

pub fn write_frame(w: &mut impl Write, kind: Kind, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(std::io::Error::other(format!(
            "frame of {} bytes exceeds the protocol's {MAX_FRAME}",
            payload.len()
        )));
    }
    let mut head = [0u8; 5];
    head[0] = kind as u8;
    head[1..].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    w.write_all(&head)?;
    w.write_all(payload)?;
    w.flush()
}

pub fn write_json<T: Serialize>(w: &mut impl Write, kind: Kind, value: &T) -> std::io::Result<()> {
    let payload = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    write_frame(w, kind, &payload)
}

/// Reads one frame; a kind this version does not know is an error, as is
/// a frame past MAX_FRAME.
pub fn read_frame(r: &mut impl Read) -> std::io::Result<(Kind, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let kind = Kind::from_byte(head[0])
        .ok_or_else(|| std::io::Error::other(format!("unknown frame kind {}", head[0])))?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::other(format!(
            "frame of {len} bytes exceeds the protocol's {MAX_FRAME}"
        )));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok((kind, payload))
}

pub fn read_json<T: for<'de> Deserialize<'de>>(
    r: &mut impl Read,
    want: Kind,
) -> std::io::Result<T> {
    let (kind, payload) = read_frame(r)?;
    if kind != want {
        return Err(std::io::Error::other(format!(
            "expected a {want:?} frame, got {kind:?}"
        )));
    }
    serde_json::from_slice(&payload).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_json(&mut buf, Kind::Hello, &Hello { version: VERSION }).unwrap();
        let d = Description {
            argv: vec!["/bin/echo".into(), "hi".into()],
            env: vec!["PATH=/bin".into()],
            cwd: "/".into(),
            uid: 1000,
            gid: 1000,
            groups: vec![4],
            hostname: "guest".into(),
            memory_max: 96 << 20,
            now_unix_ns: 1_700_000_000_000_000_000,
            seed: "00ff".into(),
        };
        write_json(&mut buf, Kind::Run, &d).unwrap();
        write_frame(&mut buf, Kind::Finish, &[]).unwrap();
        write_frame(&mut buf, Kind::Park, &[]).unwrap();
        write_frame(&mut buf, Kind::Parked, &[]).unwrap();
        let mut r = buf.as_slice();
        let hello: Hello = read_json(&mut r, Kind::Hello).unwrap();
        assert_eq!(hello.version, VERSION);
        let got: Description = read_json(&mut r, Kind::Run).unwrap();
        assert_eq!(got, d);
        assert_eq!(read_frame(&mut r).unwrap().0, Kind::Finish);
        assert_eq!(read_frame(&mut r).unwrap().0, Kind::Park);
        assert_eq!(read_frame(&mut r).unwrap().0, Kind::Parked);
        assert!(read_frame(&mut r).is_err());
    }

    #[test]
    fn an_oversized_frame_is_refused() {
        let mut buf = Vec::new();
        assert!(write_frame(&mut buf, Kind::Stdout, &vec![0u8; MAX_FRAME + 1]).is_err());
        let mut head = vec![Kind::Stdout as u8];
        head.extend_from_slice(&((MAX_FRAME as u32) + 1).to_be_bytes());
        assert!(read_frame(&mut head.as_slice()).is_err());
        assert!(read_frame(&mut [200u8, 0, 0, 0, 0].as_slice()).is_err());
    }
}
