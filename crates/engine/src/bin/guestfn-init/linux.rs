//! The OCI runtime's guest init (docs/one-pager-oci-runtime.md, "The
//! sandbox"): the first process of the microVM, booted from an initramfs
//! the engine packs around this one static binary. It mounts the image's
//! root filesystem (the squashfs the engine built from the image's layers,
//! on the first virtio block device) under a tmpfs overlay the VM discards
//! with its memory, puts proc, sys, dev and a fresh /tmp into it, connects
//! to the host over vsock, runs the image's process as the run's
//! description says - its user, its working directory, its environment -
//! with the request on its stdin, relays stdout and stderr as they arrive
//! and reports the exit. Then it powers the VM off. Every failure is
//! printed to the serial console, which the engine logs at debug, and ends
//! the VM the same way: the host reads a missing exit as a failed run.

use std::ffi::CString;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use guestfn_engine::protocol::{
    Description, Exit, Hello, Kind, read_frame, read_json, write_frame, write_json,
};

/// The image's root filesystem: Firecracker exposes the one drive as vda.
const ROOTFS_DEVICE: &str = "/dev/vda";
const LOWER: &str = "/lower";
const RW: &str = "/rw";
const MERGED: &str = "/merged";
/// The cgroup the process runs in, under the merged root's cgroup mount.
const CGROUP: &str = "/sys/fs/cgroup/run";

/// The init's whole life: run the description, report, power off. A
/// failure is printed to the serial console first.
pub fn main() {
    if let Err(e) = run() {
        eprintln!("guestfn-init: {e}");
    }
    poweroff();
}

fn run() -> Result<(), String> {
    mount_base()?;
    let mut sock = connect_host()?;
    write_json(
        &mut sock,
        Kind::Hello,
        &Hello {
            version: guestfn_engine::protocol::VERSION,
        },
    )
    .map_err(|e| format!("cannot greet the host: {e}"))?;
    let description: Description =
        read_json(&mut sock, Kind::Run).map_err(|e| format!("cannot read the run: {e}"))?;
    // The whole request before the process starts: it is small (a
    // RunFunctionRequest), and the process then reads it without waiting
    // on the host.
    let mut request = Vec::new();
    loop {
        let (kind, payload) =
            read_frame(&mut sock).map_err(|e| format!("cannot read the request: {e}"))?;
        match kind {
            Kind::Stdin => request.extend_from_slice(&payload),
            Kind::StdinEnd => break,
            other => {
                return Err(format!(
                    "unexpected {other:?} frame while reading the request"
                ));
            }
        }
    }
    if !description.hostname.is_empty() {
        set_hostname(&description.hostname);
    }
    // Into the image: from here the init sees what the process sees (the
    // process's PATH resolves inside it), plus /dev/kmsg for the OOM check
    // and the cgroup tree.
    chroot(MERGED)?;
    if description.memory_max > 0 {
        memory_cgroup(description.memory_max)?;
    }
    let sock = Arc::new(Mutex::new(sock));
    let exit = match spawn(&description, request, &sock) {
        Ok(exit) => exit,
        Err(error) => Exit {
            error,
            ..Default::default()
        },
    };
    let mut s = sock.lock().expect("poisoned");
    write_json(&mut *s, Kind::Exit, &exit).map_err(|e| format!("cannot report the exit: {e}"))?;
    let _ = s.shutdown(std::net::Shutdown::Write);
    // The frames sit in the vsock device's queue until the host drains
    // them; a power-off now would take them with the VM. The host closes
    // the connection once it has the exit, which the read below sees as
    // end of file; a bound keeps a host that never closes from pinning
    // the VM.
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let mut sink = [0u8; 1024];
    while let Ok(n) = s.read(&mut sink) {
        if n == 0 {
            break;
        }
    }
    Ok(())
}

/// Runs the process and relays its streams; the error is a run that never
/// started.
fn spawn(d: &Description, request: Vec<u8>, sock: &Arc<Mutex<UnixStream>>) -> Result<Exit, String> {
    let Some(program) = d.argv.first() else {
        return Err("the image has neither an entrypoint nor a command".to_string());
    };
    let mut cmd = Command::new(program);
    cmd.args(&d.argv[1..]);
    cmd.env_clear();
    for entry in &d.env {
        if let Some((k, v)) = entry.split_once('=') {
            cmd.env(k, v);
        }
    }
    cmd.current_dir(if d.cwd.is_empty() { "/" } else { &d.cwd });
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let (uid, gid, groups) = (d.uid, d.gid, d.groups.clone());
    // SAFETY: the closure makes the three identity syscalls and nothing
    // else between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            let groups: Vec<libc::gid_t> = groups.iter().map(|g| *g as libc::gid_t).collect();
            if libc::setgroups(groups.len(), groups.as_ptr()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgid(gid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setuid(uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {program}: {e}"))?;
    // Into the capped cgroup before it has allocated anything of note; a
    // failure here is the init's, not the process's.
    if d.memory_max > 0 {
        std::fs::write(format!("{CGROUP}/cgroup.procs"), child.id().to_string())
            .map_err(|e| format!("cannot put the process in its cgroup: {e}"))?;
    }
    let mut stdin = child.stdin.take().expect("piped");
    let feeder = std::thread::spawn(move || {
        // A process that exits without reading gets EPIPE here, which is
        // its business, not a failure of the run.
        let _ = stdin.write_all(&request);
        drop(stdin);
    });
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let out = relay(stdout, Kind::Stdout, Arc::clone(sock));
    let err = relay(stderr, Kind::Stderr, Arc::clone(sock));
    let status = child
        .wait()
        .map_err(|e| format!("cannot wait for {program}: {e}"))?;
    let _ = feeder.join();
    let _ = out.join();
    let _ = err.join();
    use std::os::unix::process::ExitStatusExt as _;
    let signal = status.signal();
    Ok(Exit {
        status: status.code(),
        signal,
        oom: signal == Some(libc::SIGKILL) && kmsg_mentions_oom(),
        error: String::new(),
    })
}

/// Relays a stream to the host in frames as it arrives, so a guest's log
/// lines reach the host while it runs and stdout is collected whatever its
/// size.
fn relay(
    mut from: impl Read + Send + 'static,
    kind: Kind,
    sock: Arc<Mutex<UnixStream>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 << 10];
        loop {
            let n = match from.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut s = sock.lock().expect("poisoned");
            if write_frame(&mut *s, kind, &buf[..n]).is_err() {
                return;
            }
        }
    })
}

/// proc, sys and dev for the init, the image's root filesystem read-only
/// under a tmpfs overlay, and proc, sys, dev and a fresh /tmp inside it.
fn mount_base() -> Result<(), String> {
    for dir in ["/dev", "/proc", "/sys", LOWER, RW, MERGED] {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir}: {e}"))?;
    }
    mount("devtmpfs", "/dev", "devtmpfs", 0, "")?;
    mount("proc", "/proc", "proc", 0, "")?;
    mount("sysfs", "/sys", "sysfs", 0, "")?;
    mount(ROOTFS_DEVICE, LOWER, "squashfs", libc::MS_RDONLY, "")?;
    mount("tmpfs", RW, "tmpfs", 0, "")?;
    for dir in ["/rw/upper", "/rw/work"] {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir}: {e}"))?;
    }
    mount(
        "overlay",
        MERGED,
        "overlay",
        0,
        &format!("lowerdir={LOWER},upperdir={RW}/upper,workdir={RW}/work"),
    )?;
    for dir in ["proc", "sys", "dev", "tmp"] {
        let path = format!("{MERGED}/{dir}");
        std::fs::create_dir_all(&path).map_err(|e| format!("cannot create {path}: {e}"))?;
    }
    mount("proc", &format!("{MERGED}/proc"), "proc", 0, "")?;
    mount("sysfs", &format!("{MERGED}/sys"), "sysfs", 0, "")?;
    mount(
        "/dev",
        &format!("{MERGED}/dev"),
        "",
        libc::MS_BIND | libc::MS_REC,
        "",
    )?;
    // A fresh, writable /tmp whatever the image put there: the private
    // scratch space the contract promises.
    mount("tmpfs", &format!("{MERGED}/tmp"), "tmpfs", 0, "mode=1777")?;
    // The cgroup tree, for the process's memory cap.
    let cgroup = format!("{MERGED}/sys/fs/cgroup");
    std::fs::create_dir_all(&cgroup).map_err(|e| format!("cannot create {cgroup}: {e}"))?;
    mount("cgroup2", &cgroup, "cgroup2", 0, "")?;
    Ok(())
}

/// A cgroup capped at `max` bytes, with its memory controller on and the
/// whole group killed together on an out-of-memory.
fn memory_cgroup(max: u64) -> Result<(), String> {
    std::fs::create_dir_all(CGROUP).map_err(|e| format!("cannot create {CGROUP}: {e}"))?;
    let w = |path: String, value: &str| {
        std::fs::write(&path, value).map_err(|e| format!("cannot write {value:?} to {path}: {e}"))
    };
    w(
        "/sys/fs/cgroup/cgroup.subtree_control".to_string(),
        "+memory",
    )?;
    w(format!("{CGROUP}/memory.max"), &max.to_string())?;
    // Best effort: an older kernel has no such knob.
    let _ = std::fs::write(format!("{CGROUP}/memory.oom.group"), "1");
    // No swap in the VM, but say so in case one appears.
    let _ = std::fs::write(format!("{CGROUP}/memory.swap.max"), "0");
    Ok(())
}

fn mount(
    source: &str,
    target: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: &str,
) -> Result<(), String> {
    let c = |s: &str| CString::new(s).expect("no NUL in a mount argument");
    let (source, target, fstype, data) = (c(source), c(target), c(fstype), c(data));
    // SAFETY: four valid C strings and the flags; mount reads them and
    // returns.
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            if fstype.as_bytes().is_empty() {
                std::ptr::null()
            } else {
                fstype.as_ptr()
            },
            flags,
            data.as_ptr() as *const libc::c_void,
        )
    };
    if rc != 0 {
        return Err(format!(
            "cannot mount {} on {target:?}: {}",
            if fstype.as_bytes().is_empty() {
                format!("{source:?} (bind)")
            } else {
                format!("{fstype:?} {source:?}")
            },
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn chroot(dir: &str) -> Result<(), String> {
    let c = CString::new(dir).expect("no NUL");
    // SAFETY: a valid C string.
    if unsafe { libc::chroot(c.as_ptr()) } != 0 {
        return Err(format!(
            "cannot chroot into {dir}: {}",
            std::io::Error::last_os_error()
        ));
    }
    std::env::set_current_dir("/").map_err(|e| format!("cannot enter the root: {e}"))
}

fn set_hostname(name: &str) {
    // SAFETY: the bytes and their length.
    unsafe {
        libc::sethostname(name.as_ptr() as *const libc::c_char, name.len());
    }
}

/// Connects to the host on the well-known port; the host listens before
/// the VM boots, and the retries cover the vsock driver coming up.
fn connect_host() -> Result<UnixStream, String> {
    let mut last = String::new();
    for _ in 0..200 {
        // SAFETY: socket(2) with constant arguments.
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(format!(
                "cannot open a vsock socket: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: sockaddr_vm is plain data; zeroed is a valid value.
        let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        addr.svm_cid = guestfn_engine::protocol::HOST_CID;
        addr.svm_port = guestfn_engine::protocol::PORT;
        // SAFETY: a valid fd and address of the stated length.
        let rc = unsafe {
            libc::connect(
                fd,
                &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };
        if rc == 0 {
            use std::os::unix::io::FromRawFd as _;
            // SAFETY: a connected stream socket this function owns; a
            // UnixStream is a wrapper over read, write and shutdown on it.
            return Ok(unsafe { UnixStream::from_raw_fd(fd) });
        }
        last = std::io::Error::last_os_error().to_string();
        // SAFETY: the fd is ours and not used again.
        unsafe { libc::close(fd) };
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!("cannot connect to the host over vsock: {last}"))
}

/// Whether the kernel log says the OOM killer acted: a SIGKILL from it is
/// the run's memory limit, which the host names as such.
fn kmsg_mentions_oom() -> bool {
    use std::os::unix::fs::OpenOptionsExt as _;
    let Ok(mut f) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/kmsg")
    else {
        return false;
    };
    let mut text = String::new();
    let mut buf = [0u8; 8192];
    loop {
        match f.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => text.push_str(&String::from_utf8_lossy(&buf[..n])),
        }
    }
    let lower = text.to_ascii_lowercase();
    lower.contains("out of memory") || lower.contains("oom-kill")
}

fn poweroff() {
    // SAFETY: sync and reboot take no pointers; reboot does not return.
    unsafe {
        libc::sync();
        libc::reboot(libc::LINUX_REBOOT_CMD_POWER_OFF);
    }
    // Only if the kernel refused: stay alive rather than panic PID 1.
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[allow(dead_code)]
fn exists(p: &str) -> bool {
    Path::new(p).exists()
}
