//! Firecracker behind the `Sandbox` seam, the evaluation's first prototype
//! (docs/one-pager-oci-runtime.md, "Sandbox evaluation"): one `firecracker`
//! process per run, configured from a file and started with no API
//! socket, booting the kernel with the init's initramfs, the root
//! filesystem as a read-only block device, a vsock device and no network
//! device. The init connects back to the host port, which Firecracker
//! maps onto a Unix socket next to its own; the run is driven through the
//! protocol over that connection, and the VM is killed when the run ends
//! or the deadline passes, whichever comes first.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::Error;
use crate::protocol::{
    Exit, Hello, Kind, read_frame, read_json, write_chunked, write_frame, write_json,
};

use crate::oci::sandbox::{Ended, Outcome, Sandbox, VmSpec};

/// The guest's CID; the host is always 2.
const GUEST_CID: u32 = 3;
/// The kernel command line Firecracker's guide gives, with the console on
/// the serial port the engine reads: the init's own lines come out there.
const BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 pci=off nomodules random.trust_cpu=on i8042.noaux i8042.nomux i8042.nopnp i8042.dumbkbd quiet loglevel=4";
/// Most of the serial console kept for the debug log.
const CONSOLE_CAP: usize = 64 << 10;

pub struct Firecracker {
    binary: PathBuf,
}

impl Firecracker {
    pub fn new(binary: PathBuf) -> Firecracker {
        Firecracker { binary }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

impl Sandbox for Firecracker {
    fn name(&self) -> &'static str {
        "firecracker"
    }

    fn run(&self, spec: VmSpec<'_>) -> Result<Outcome, Error> {
        // Unix socket paths are short-limited (108 bytes): the scratch
        // directory is the engine's, under the temp dir.
        let dir = tempfile::Builder::new()
            .prefix("vm-")
            .tempdir_in(spec.scratch)
            .map_err(|e| Error(format!("cannot create the VM's directory: {e}")))?;
        let uds = dir.path().join("v.sock");
        let host_port = dir.path().join(format!("v.sock_{}", crate::protocol::PORT));
        let listener = UnixListener::bind(&host_port).map_err(|e| {
            Error(format!(
                "cannot listen for the guest on {}: {e}",
                host_port.display()
            ))
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|e| Error(format!("cannot configure the guest listener: {e}")))?;
        let config = serde_json::json!({
            "boot-source": {
                "kernel_image_path": spec.kernel,
                "initrd_path": spec.initrd,
                "boot_args": BOOT_ARGS,
            },
            "drives": [{
                "drive_id": "rootfs",
                "path_on_host": spec.rootfs,
                "is_root_device": false,
                "is_read_only": true,
            }],
            "machine-config": {
                "vcpu_count": spec.vcpus,
                "mem_size_mib": spec.memory_mib,
                "smt": false,
            },
            "vsock": {
                "guest_cid": GUEST_CID,
                "uds_path": uds,
            },
        });
        let config_path = dir.path().join("vm.json");
        std::fs::write(&config_path, serde_json::to_vec(&config).expect("json"))
            .map_err(|e| Error(format!("cannot write the VM's configuration: {e}")))?;
        let mut child = Command::new(&self.binary)
            .arg("--no-api")
            .arg("--config-file")
            .arg(&config_path)
            .arg("--level")
            .arg("Warning")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error(format!("cannot start {}: {e}", self.binary.display())))?;
        let console = Arc::new(Mutex::new(String::new()));
        let drain = |stream: Option<Box<dyn Read + Send>>, sink: Arc<Mutex<String>>| {
            std::thread::spawn(move || {
                let Some(mut stream) = stream else { return };
                let mut buf = [0u8; 4096];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut s = sink.lock().expect("poisoned");
                    if s.len() < CONSOLE_CAP {
                        s.push_str(&String::from_utf8_lossy(&buf[..n]));
                    }
                }
            })
        };
        let out_thread = drain(
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            Arc::clone(&console),
        );
        let err_thread = drain(
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            Arc::clone(&console),
        );
        let result = drive(&listener, &mut child, &spec);
        // Whatever happened, the VM goes: a run leaves nothing behind.
        let _ = child.kill();
        let _ = child.wait();
        let _ = out_thread.join();
        let _ = err_thread.join();
        let console = std::mem::take(&mut *console.lock().expect("poisoned"));
        let (stdout, stderr, end) = result;
        Ok(Outcome {
            stdout,
            stderr,
            end,
            console,
        })
    }
}

/// Waits for the init's connection, then speaks the protocol until the
/// exit, the deadline or a lost connection.
fn drive(
    listener: &UnixListener,
    child: &mut Child,
    spec: &VmSpec<'_>,
) -> (Vec<u8>, Vec<u8>, Ended) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let stream = loop {
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                return (
                    stdout,
                    stderr,
                    Ended::Lost(format!("cannot accept the guest's connection: {e}")),
                );
            }
        }
        if Instant::now() >= spec.deadline {
            return (stdout, stderr, Ended::Deadline);
        }
        if let Ok(Some(status)) = child.try_wait() {
            return (
                stdout,
                stderr,
                Ended::Lost(format!(
                    "the VMM exited before the guest connected ({status})"
                )),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let end = match talk(stream, spec, &mut stdout, &mut stderr) {
        Ok(exit) => Ended::Exit(exit),
        Err(Talk::Deadline) => Ended::Deadline,
        Err(Talk::Lost(e)) => Ended::Lost(e),
    };
    (stdout, stderr, end)
}

enum Talk {
    Deadline,
    Lost(String),
}

fn remaining(deadline: Instant) -> Result<Duration, Talk> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Talk::Deadline);
    }
    Ok(left)
}

fn io_err(e: std::io::Error, what: &str) -> Talk {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => Talk::Deadline,
        _ => Talk::Lost(format!("{what}: {e}")),
    }
}

fn talk(
    mut stream: UnixStream,
    spec: &VmSpec<'_>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
) -> Result<Exit, Talk> {
    stream
        .set_nonblocking(false)
        .map_err(|e| Talk::Lost(format!("cannot configure the guest's connection: {e}")))?;
    let arm = |stream: &UnixStream, deadline: Instant| -> Result<(), Talk> {
        let left = remaining(deadline)?;
        stream
            .set_read_timeout(Some(left))
            .and_then(|()| stream.set_write_timeout(Some(left)))
            .map_err(|e| Talk::Lost(format!("cannot configure the guest's connection: {e}")))
    };
    arm(&stream, spec.deadline)?;
    let hello: Hello = read_json(&mut stream, Kind::Hello)
        .map_err(|e| io_err(e, "cannot read the init's hello"))?;
    if hello.version != crate::protocol::VERSION {
        return Err(Talk::Lost(format!(
            "the init speaks protocol version {}, this engine version {}",
            hello.version,
            crate::protocol::VERSION
        )));
    }
    write_json(&mut stream, Kind::Run, spec.description)
        .map_err(|e| io_err(e, "cannot send the run"))?;
    write_chunked(&mut stream, Kind::Stdin, spec.request)
        .map_err(|e| io_err(e, "cannot send the request"))?;
    write_frame(&mut stream, Kind::StdinEnd, &[])
        .map_err(|e| io_err(e, "cannot send the request"))?;
    loop {
        arm(&stream, spec.deadline)?;
        let (kind, payload) =
            read_frame(&mut stream).map_err(|e| io_err(e, "the guest's connection ended"))?;
        match kind {
            Kind::Stdout => stdout.extend_from_slice(&payload),
            Kind::Stderr => stderr.extend_from_slice(&payload),
            Kind::Exit => {
                let exit: Exit = serde_json::from_slice(&payload)
                    .map_err(|e| Talk::Lost(format!("cannot read the exit: {e}")))?;
                let _ = stream.flush();
                return Ok(exit);
            }
            other => {
                return Err(Talk::Lost(format!(
                    "unexpected {other:?} frame from the init"
                )));
            }
        }
    }
}
