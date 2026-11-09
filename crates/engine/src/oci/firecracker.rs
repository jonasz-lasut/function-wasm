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

use crate::metrics;
use crate::oci::sandbox::{Ended, Outcome, Sandbox, Snapshot, VmSpec};

/// The guest's CID; the host is always 2.
const GUEST_CID: u32 = 3;
/// The kernel command line Firecracker's guide gives, with the console on
/// the serial port the engine reads: the init's own lines come out there.
const BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 pci=off nomodules random.trust_cpu=on i8042.noaux i8042.nomux i8042.nopnp i8042.dumbkbd quiet loglevel=4";
/// Most of the serial console kept for the debug log.
const CONSOLE_CAP: usize = 64 << 10;

pub struct Firecracker {
    binary: PathBuf,
    /// The snapshots this process made, when the engine was told to keep
    /// them (`Options::snapshots`): None boots every run.
    snapshots: Option<Snapshots>,
}

/// What the sandbox keeps across runs when it snapshots: a counter for the
/// scratch directories, and the fingerprint of what a snapshot depends on
/// beyond its image and machine shape.
#[derive(Default)]
struct Snapshots {
    next: std::sync::atomic::AtomicU64,
    fingerprint: std::sync::OnceLock<String>,
}

impl Firecracker {
    pub fn new(binary: PathBuf) -> Firecracker {
        Firecracker {
            binary,
            snapshots: None,
        }
    }

    /// Firecracker restoring a snapshot per image in place of a boot
    /// (docs/one-pager-vm-snapshots.md): the first run of an image boots,
    /// parks the init and snapshots the VM through the API socket; every
    /// later run restores that snapshot into a fresh process. A stored
    /// image's snapshot is kept in the run's store, beside the compiled
    /// artifacts; any other under the engine's scratch directory.
    pub fn with_snapshots(binary: PathBuf) -> Firecracker {
        Firecracker {
            binary,
            snapshots: Some(Snapshots::default()),
        }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// The VM's configuration file, as the release binary reads it.
    fn config(spec: &VmSpec<'_>, uds: &Path) -> serde_json::Value {
        serde_json::json!({
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
        })
    }

    /// The VM's directory with the guest's port bound in it. Unix socket
    /// paths are short-limited (108 bytes): the scratch directory is the
    /// engine's, under the temp dir.
    fn workdir(spec: &VmSpec<'_>) -> Result<(tempfile::TempDir, PathBuf, UnixListener), Error> {
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
        Ok((dir, uds, listener))
    }

    /// Drives the run over the guest's connection, then stops the VM
    /// whatever happened: a run leaves nothing behind.
    fn finish(
        &self,
        mut child: Child,
        listener: &UnixListener,
        spec: &VmSpec<'_>,
        started: Instant,
        how: &str,
    ) -> Outcome {
        let ready = Instant::now();
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
        let mut connected = None;
        let result = drive(listener, &mut child, spec, &mut connected);
        let driven = Instant::now();
        let usage = process_usage(child.id());
        let _ = child.kill();
        let _ = child.wait();
        let _ = out_thread.join();
        let _ = err_thread.join();
        let console = std::mem::take(&mut *console.lock().expect("poisoned"));
        // GUESTFN_VM_CONSOLE prints every VM's serial console to stderr, for
        // a test run or a laptop debugging a boot; the engine's debug log
        // carries it otherwise.
        if std::env::var_os("GUESTFN_VM_CONSOLE").is_some() {
            eprintln!(
                "--- VM console ({}) ---\n{console}--- end ---",
                spec.description.argv.join(" ")
            );
        }
        // Where a run's time went: until the VMM had its VM (the process
        // start, and the snapshot load of a restore), until the init
        // connected, until the process exited.
        let connected = connected.unwrap_or(driven);
        let ms =
            |from: Instant, to: Instant| to.saturating_duration_since(from).as_secs_f64() * 1e3;
        tracing::debug!(
            how,
            vmm_ms = ms(started, ready),
            guest_ms = ms(ready, connected),
            run_ms = ms(connected, driven),
            "the VM's run, by phase"
        );
        if std::env::var_os("GUESTFN_VM_TIMING").is_some() {
            eprintln!(
                "vm-timing how={how} vmm_ms={:.1} guest_ms={:.1} run_ms={:.1} total_ms={:.1} vmm_rss_mib={:.1} vmm_minor_faults={}",
                ms(started, ready),
                ms(ready, connected),
                ms(connected, driven),
                ms(started, driven),
                usage.0 as f64 / (1u64 << 20) as f64,
                usage.1,
            );
        }
        let (stdout, stderr, end) = result;
        Outcome {
            stdout,
            stderr,
            end,
            console,
        }
    }

    /// A boot: the VM from its configuration file, with no API socket.
    fn boot(&self, spec: &VmSpec<'_>) -> Result<Outcome, Error> {
        let started = Instant::now();
        let (dir, uds, listener) = Self::workdir(spec)?;
        let config_path = dir.path().join("vm.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec(&Self::config(spec, &uds)).expect("json"),
        )
        .map_err(|e| Error(format!("cannot write the VM's configuration: {e}")))?;
        let child = Command::new(&self.binary)
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
        Ok(self.finish(child, &listener, spec, started, "boot"))
    }

    /// The names a stored image's snapshot goes by in the store: the root
    /// filesystem's own entry name, the machine shape and the fingerprint
    /// of what else the frozen VM depends on, so a snapshot is found only
    /// by the Firecracker, the kernel and the init that took it.
    fn names(&self, snapshots: &Snapshots, spec: &VmSpec<'_>) -> Result<(String, String), String> {
        let rootfs = spec
            .rootfs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or("the root filesystem has no file name")?;
        let fingerprint = match snapshots.fingerprint.get() {
            Some(f) => f.clone(),
            None => {
                use sha2::Digest as _;
                let mut hash = sha2::Sha256::new();
                for path in [self.binary.as_path(), spec.kernel, spec.initrd] {
                    let bytes = std::fs::read(path)
                        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                    hash.update(sha2::Sha256::digest(&bytes));
                }
                hash.update(BOOT_ARGS.as_bytes());
                hash.update(crate::protocol::VERSION.to_be_bytes());
                let f = hex::encode(&hash.finalize()[..6]);
                snapshots.fingerprint.get_or_init(|| f).clone()
            }
        };
        let stem = format!(
            "{rootfs}.vm-{}m{}c-{fingerprint}",
            spec.memory_mib, spec.vcpus
        );
        Ok((format!("{stem}.state"), format!("{stem}.mem")))
    }

    /// The snapshot for this image and machine shape: the one the loaded
    /// image already has, the one the store kept and still verifies, or a
    /// new one. To make one the VM boots with its API socket, the init
    /// connects and is parked, and the paused VM is written out; that VM
    /// never runs a process.
    fn snapshot(&self, snapshots: &Snapshots, spec: &VmSpec<'_>) -> Result<Arc<Snapshot>, String> {
        let slot = spec.snapshots.slot(spec.memory_mib, spec.vcpus);
        let mut slot = slot.lock().expect("poisoned");
        let count = |event| {
            metrics::CACHE_EVENTS
                .with_label_values(&[metrics::CACHE_SNAPSHOT, event])
                .inc()
        };
        if let Some(snapshot) = &*slot {
            if snapshot.state.is_file() && snapshot.mem.is_file() {
                if let Some(store) = spec.store {
                    store.used(&snapshot.state);
                    store.used(&snapshot.mem);
                }
                count(metrics::EVENT_HIT);
                return Ok(Arc::clone(snapshot));
            }
            // The sweep took a file from under the image: made again.
            count(metrics::EVENT_STALE);
            *slot = None;
        }
        let names = match spec.store {
            Some(_) => Some(self.names(snapshots, spec)?),
            None => None,
        };
        if let (Some(store), Some((state, mem))) = (spec.store, &names) {
            // Both or neither: a lone file is written over below.
            if let (Some(state), Some(mem)) = (store.sealed(state), store.sealed(mem)) {
                let snapshot = Arc::new(Snapshot { state, mem });
                *slot = Some(Arc::clone(&snapshot));
                count(metrics::EVENT_HIT);
                return Ok(snapshot);
            }
        }
        count(metrics::EVENT_MISS);
        let started = Instant::now();
        let (state_path, mem_path) = match (spec.store, &names) {
            (Some(store), Some((state, mem))) => (store.begin(state)?, store.begin(mem)?),
            _ => {
                let n = snapshots
                    .next
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let keep = spec.scratch.join(format!("snapshot-{n}"));
                std::fs::create_dir_all(&keep)
                    .map_err(|e| format!("cannot create {}: {e}", keep.display()))?;
                (keep.join("state"), keep.join("mem"))
            }
        };
        let (dir, uds, listener) = Self::workdir(spec).map_err(|e| e.0)?;
        let config_path = dir.path().join("vm.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec(&Self::config(spec, &uds)).expect("json"),
        )
        .map_err(|e| format!("cannot write the VM's configuration: {e}"))?;
        let api = dir.path().join("api.sock");
        let mut child = Command::new(&self.binary)
            .arg("--api-sock")
            .arg(&api)
            .arg("--config-file")
            .arg(&config_path)
            .arg("--level")
            .arg("Warning")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.binary.display()))?;
        let result = (|| -> Result<(), String> {
            let deadline = spec.deadline.min(Instant::now() + Duration::from_secs(30));
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(format!("cannot accept the guest's connection: {e}")),
                }
                if Instant::now() >= deadline {
                    return Err(GUEST_NEVER_CONNECTED.to_string());
                }
                std::thread::sleep(Duration::from_millis(2));
            };
            stream
                .set_nonblocking(false)
                .and_then(|()| stream.set_read_timeout(Some(Duration::from_secs(10))))
                .map_err(|e| format!("cannot configure the guest's connection: {e}"))?;
            let hello: Hello = read_json(&mut stream, Kind::Hello)
                .map_err(|e| format!("cannot read the init's hello: {e}"))?;
            if hello.version != crate::protocol::VERSION {
                return Err(format!(
                    "the init speaks protocol version {}, this engine version {}",
                    hello.version,
                    crate::protocol::VERSION
                ));
            }
            write_frame(&mut stream, Kind::Park, &[])
                .map_err(|e| format!("cannot park the init: {e}"))?;
            // The init answers and then waits on this connection, sending
            // nothing: the VM is still when it is paused. The connection
            // stays open until the snapshot is written, since its reset on
            // restore is what wakes the init in a clone.
            let (kind, _) =
                read_frame(&mut stream).map_err(|e| format!("the init did not park: {e}"))?;
            if kind != Kind::Parked {
                return Err(format!("expected the init's Parked, got {kind:?}"));
            }
            api_call(
                &api,
                "PATCH",
                "/vm",
                &serde_json::json!({"state": "Paused"}),
            )?;
            api_call(
                &api,
                "PUT",
                "/snapshot/create",
                &serde_json::json!({
                    "snapshot_type": "Full",
                    "snapshot_path": state_path,
                    "mem_file_path": mem_path,
                }),
            )?;
            drop(stream);
            Ok(())
        })();
        let _ = child.kill();
        let _ = child.wait();
        result?;
        let (apparent, kept) =
            sparsify(&mem_path).map_err(|e| format!("cannot rewrite the VM's memory file: {e}"))?;
        // In the store a file counts once sealed, under the store's key.
        let snapshot = Arc::new(match (spec.store, &names) {
            (Some(store), Some((state, mem))) => Snapshot {
                state: store.seal(state)?,
                mem: store.seal(mem)?,
            },
            _ => Snapshot {
                state: state_path,
                mem: mem_path,
            },
        });
        metrics::SNAPSHOT_DURATION.observe(started.elapsed().as_secs_f64());
        tracing::debug!(
            rootfs = %spec.rootfs.display(),
            memory_mib = spec.memory_mib,
            memory_file_bytes = apparent,
            memory_file_kept_bytes = kept,
            millis = started.elapsed().as_millis() as u64,
            "snapshotted the VM"
        );
        if std::env::var_os("GUESTFN_VM_TIMING").is_some() {
            eprintln!(
                "vm-timing how=snapshot total_ms={:.1} memory_mib={} mem_file_mib={:.1} mem_file_kept_mib={:.1} state_kib={:.1}",
                started.elapsed().as_secs_f64() * 1e3,
                spec.memory_mib,
                apparent as f64 / (1u64 << 20) as f64,
                kept as f64 / (1u64 << 20) as f64,
                std::fs::metadata(&snapshot.state)
                    .map(|m| m.len())
                    .unwrap_or(0) as f64
                    / 1024.0,
            );
        }
        *slot = Some(Arc::clone(&snapshot));
        Ok(snapshot)
    }

    /// A restore in place of a boot: a fresh Firecracker process loads the
    /// snapshot (its memory mapped privately from the file, so the clone's
    /// writes are its own), the parked init connects to this run's socket
    /// and the run proceeds as after a boot. The description carries the
    /// host's clock and fresh randomness for the init to set.
    fn restore(&self, spec: &VmSpec<'_>, snapshot: &Snapshot) -> Result<Outcome, String> {
        let started = Instant::now();
        let (dir, uds, listener) = Self::workdir(spec).map_err(|e| e.0)?;
        let api = dir.path().join("api.sock");
        let mut child = Command::new(&self.binary)
            .arg("--api-sock")
            .arg(&api)
            .arg("--level")
            .arg("Warning")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.binary.display()))?;
        if let Err(e) = api_call(
            &api,
            "PUT",
            "/snapshot/load",
            &serde_json::json!({
                "snapshot_path": snapshot.state,
                "mem_backend": {"backend_type": "File", "backend_path": snapshot.mem},
                "resume_vm": true,
                "vsock_override": {"uds_path": uds},
            }),
        ) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        let mut seed = [0u8; 32];
        let seeded = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut seed));
        let description = crate::protocol::Description {
            now_unix_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
            seed: if seeded.is_ok() {
                hex::encode(seed)
            } else {
                String::new()
            },
            ..spec.description.clone()
        };
        let spec = VmSpec {
            description: &description,
            ..*spec
        };
        Ok(self.finish(child, &listener, &spec, started, "restore"))
    }
}

/// Rewrites a memory file sparse, in place: Firecracker writes every page
/// of the VM's memory, and most of a just-booted guest's are zero. A hole
/// reads as zeros, so the restore maps the same content from a file that
/// costs the disk only what the guest had touched. Returns the file's
/// length and the bytes kept.
fn sparsify(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::io::{Seek, SeekFrom};
    const BLOCK: usize = 64 << 10;
    let mut source = std::fs::File::open(path)?;
    let len = source.metadata()?.len();
    // Appended, not swapped for the extension: in the store the path has
    // the marker of an unfinished write, which the name has to keep.
    let sparse_path = PathBuf::from(format!("{}.sparse", path.display()));
    let mut sparse = std::fs::File::create(&sparse_path)?;
    let mut block = vec![0u8; BLOCK];
    let mut offset = 0u64;
    let mut kept = 0u64;
    while offset < len {
        let want = BLOCK.min((len - offset) as usize);
        source.read_exact(&mut block[..want])?;
        if block[..want].iter().any(|b| *b != 0) {
            sparse.seek(SeekFrom::Start(offset))?;
            sparse.write_all(&block[..want])?;
            kept += want as u64;
        }
        offset += want as u64;
    }
    sparse.set_len(len)?;
    sparse.sync_all()?;
    std::fs::rename(&sparse_path, path)?;
    Ok((len, kept))
}

/// The VMM process's resident memory in bytes and its minor page faults,
/// from /proc: what a run cost the host, for the timing line. Zeros where
/// /proc has no such file.
fn process_usage(pid: u32) -> (u64, u64) {
    let rss = std::fs::read_to_string(format!("/proc/{pid}/statm"))
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| pages * 4096)
        .unwrap_or(0);
    let faults = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            // The fields after the command, which may hold spaces.
            let rest = s.rsplit_once(')')?.1.to_string();
            rest.split_whitespace().nth(7)?.parse::<u64>().ok()
        })
        .unwrap_or(0);
    (rss, faults)
}

/// One request to Firecracker's API socket: HTTP/1.1 over the Unix socket,
/// a JSON body, 204 on success. The socket appears a moment after the
/// process starts, so the connect is tried for a while.
fn api_call(sock: &Path, method: &str, path: &str, body: &serde_json::Value) -> Result<(), String> {
    let started = Instant::now();
    let mut stream = loop {
        match UnixStream::connect(sock) {
            Ok(s) => break s,
            Err(e) if started.elapsed() > Duration::from_secs(5) => {
                return Err(format!("cannot reach the VMM's API socket: {e}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(1)),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(30))))
        .map_err(|e| format!("cannot configure the VMM's API socket: {e}"))?;
    let body = body.to_string();
    // One write for the whole request: written piece by piece, a peer that
    // answers and closes on the first piece breaks the pipe under the rest.
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("cannot send {method} {path} to the VMM: {e}"))?;
    let mut response = Vec::new();
    let mut buf = [0u8; 1024];
    // The status line is enough for success; an error's body follows its
    // headers and fits a read or two.
    loop {
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("cannot read the VMM's answer to {method} {path}: {e}"))?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&response);
        if let Some(head_end) = text.find("\r\n\r\n") {
            let length = text[..head_end]
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if response.len() >= head_end + 4 + length {
                break;
            }
        }
    }
    let text = String::from_utf8_lossy(&response);
    let status = text.split_whitespace().nth(1).unwrap_or("");
    if status.starts_with('2') {
        return Ok(());
    }
    let detail = text.split("\r\n\r\n").nth(1).unwrap_or("").trim();
    Err(format!(
        "the VMM answered {method} {path} with {status}: {detail}"
    ))
}

impl Sandbox for Firecracker {
    fn name(&self) -> &'static str {
        "firecracker"
    }

    fn run(&self, spec: VmSpec<'_>) -> Result<Outcome, Error> {
        if let Some(snapshots) = &self.snapshots
            && !spec.snapshots.is_off()
        {
            // A snapshot that cannot be made or restored costs its run the
            // boot it was meant to save, never the run, and the image its
            // snapshots until it is loaded again: the next run does not try
            // what just failed.
            match self.snapshot(snapshots, &spec) {
                Ok(snapshot) => match self.restore(&spec, &snapshot) {
                    Ok(outcome) => return Ok(outcome),
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot restore the VM's snapshot; booting it");
                        *spec
                            .snapshots
                            .slot(spec.memory_mib, spec.vcpus)
                            .lock()
                            .expect("poisoned") = None;
                        let _ = std::fs::remove_file(&snapshot.state);
                        let _ = std::fs::remove_file(&snapshot.mem);
                        spec.snapshots.turn_off();
                    }
                },
                Err(e) => {
                    tracing::warn!(error = %e, "cannot snapshot the VM; booting it");
                    spec.snapshots.turn_off();
                }
            }
        }
        self.boot(&spec)
    }
}

/// What the host says when the guest never reaches it: the VM booted (or
/// stalled) and no init connected before the deadline. Its own sentence,
/// not the module's deadline: the process never ran (#272).
pub const GUEST_NEVER_CONNECTED: &str = "the guest did not connect before the deadline: the VM booted but its init never reached the host";

/// Waits for the init's connection, then speaks the protocol until the
/// exit, the deadline or a lost connection. Until the run is sent, time
/// running out is the sandbox's failure, not the module's: a guest that
/// never connects, or connects and sends no hello, never ran the process.
fn drive(
    listener: &UnixListener,
    child: &mut Child,
    spec: &VmSpec<'_>,
    connected: &mut Option<Instant>,
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
            return (
                stdout,
                stderr,
                Ended::Lost(GUEST_NEVER_CONNECTED.to_string()),
            );
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
        std::thread::sleep(Duration::from_millis(1));
    };
    *connected = Some(Instant::now());
    let end = match talk(stream, spec, &mut stdout, &mut stderr) {
        Ok(exit) => Ended::Exit(exit),
        Err(Talk::Deadline) => Ended::Deadline,
        Err(Talk::Lost(e)) => Ended::Lost(e),
    };
    // The stream is dropped here: the init reads the close as its cue to
    // power off, after every frame it sent has been drained.
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
    let hello: Hello = read_json(&mut stream, Kind::Hello).map_err(|e| match io_err(e, "cannot read the init's hello") {
        Talk::Deadline => Talk::Lost(
            "the guest connected but sent no hello before the deadline: its init stalled before the run"
                .to_string(),
        ),
        lost => lost,
    })?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Description;

    /// The API client reads a success from the status line and puts the
    /// VMM's own words into a failure.
    #[test]
    fn an_api_call_reports_the_vmm_s_answer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock = dir.path().join("api.sock");
        let listener = UnixListener::bind(&sock).expect("listener");
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for answer in [
                "HTTP/1.1 204 \r\nServer: Firecracker API\r\nConnection: keep-alive\r\n\r\n"
                    .to_string(),
                {
                    let body = r#"{"fault_message":"Load snapshot error: no such file"}"#;
                    format!(
                        "HTTP/1.1 400 \r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                },
            ] {
                let (mut stream, _) = listener.accept().expect("accept");
                // The whole request before the answer, as an HTTP server
                // reads it: the headers, then the body they announce.
                let mut request = Vec::new();
                let mut buf = [0u8; 2048];
                loop {
                    let n = stream.read(&mut buf).expect("request");
                    assert!(n > 0, "the request ended early");
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request);
                    let Some(head) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let length: usize = text[..head]
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .expect("a Content-Length header")
                        .parse()
                        .expect("a length");
                    if request.len() >= head + 4 + length {
                        break;
                    }
                }
                seen.push(String::from_utf8_lossy(&request).to_string());
                stream.write_all(answer.as_bytes()).expect("answer");
            }
            seen
        });
        let body = serde_json::json!({"state": "Paused"});
        assert_eq!(api_call(&sock, "PATCH", "/vm", &body), Ok(()));
        let err = api_call(&sock, "PUT", "/snapshot/load", &body).unwrap_err();
        assert_eq!(
            err,
            r#"the VMM answered PUT /snapshot/load with 400: {"fault_message":"Load snapshot error: no such file"}"#
        );
        let seen = server.join().expect("server");
        assert!(seen[0].starts_with("PATCH /vm HTTP/1.1\r\n"), "{}", seen[0]);
        assert!(seen[0].ends_with(r#"{"state":"Paused"}"#), "{}", seen[0]);
    }

    /// A sparse memory file reads as the one Firecracker wrote and keeps
    /// only its non-zero blocks.
    #[test]
    fn a_sparse_memory_file_reads_the_same() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mem");
        let mut content = vec![0u8; (1 << 20) + 4096];
        content[70_000] = 7;
        content[(1 << 20) + 100] = 9;
        std::fs::write(&path, &content).expect("write");
        let (len, kept) = sparsify(&path).expect("sparsify");
        assert_eq!(len, content.len() as u64);
        assert_eq!(kept, (64 << 10) + 4096);
        assert_eq!(std::fs::read(&path).expect("read"), content);
    }

    /// A guest that never connects is the sandbox's failure, worded as
    /// such, not the module's deadline: the process never ran.
    #[test]
    fn a_guest_that_never_connects_is_lost_not_late() {
        let dir = tempfile::tempdir().expect("tempdir");
        let listener = UnixListener::bind(dir.path().join("vsock")).expect("listener");
        listener.set_nonblocking(true).expect("nonblocking");
        // Stands in for the VMM: alive for as long as the wait lasts.
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep");
        let description = Description::default();
        let spec = VmSpec {
            kernel: dir.path(),
            initrd: dir.path(),
            rootfs: dir.path(),
            memory_mib: 64,
            vcpus: 1,
            description: &description,
            request: b"",
            deadline: Instant::now() + Duration::from_millis(50),
            scratch: dir.path(),
            snapshots: &Default::default(),
            store: None,
        };
        let (stdout, stderr, end) = drive(&listener, &mut child, &spec, &mut None);
        let _ = child.kill();
        let _ = child.wait();
        assert!(stdout.is_empty() && stderr.is_empty());
        match end {
            Ended::Lost(why) => assert_eq!(why, GUEST_NEVER_CONNECTED),
            other => panic!("expected a lost run, got {other:?}"),
        }
    }
}
