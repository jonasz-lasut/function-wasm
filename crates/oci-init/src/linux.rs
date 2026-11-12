//! The OCI runtime's guest init (docs/one-pager-oci-runtime.md, "The
//! sandbox"): the first process of the microVM, booted from an initramfs
//! the engine packs around this one static binary. It mounts the image's
//! root filesystem (the squashfs the engine built from the image's layers,
//! on the first virtio block device) under a tmpfs overlay the VM discards
//! with its memory, puts proc, sys, dev and a fresh /tmp into it, connects
//! to the host over vsock, runs the image's process as the run's
//! description says - its user, its working directory, its environment -
//! and relays its stdout and stderr as they arrive. Once the function's
//! server accepts connections it bridges a second vsock connection to it,
//! over which the host calls the function, and it reports the process's
//! exit. Then it powers the VM off. Every failure is
//! printed to the serial console, which the engine logs at debug, and ends
//! the VM the same way: the host reads a missing exit as a failed run.

use std::ffi::CString;
use std::io::Read;
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use guestfn_oci_protocol::{
    Description, Exit, GRPC_PORT, Hello, Kind, PORT, SERVER_PORT, read_frame, read_json,
    write_frame, write_json,
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
    // Three lines on the serial console, which the host keeps with the run
    // (GUESTFN_VM_CONSOLE prints it in the engine's tests): where a stalled
    // VM got to is otherwise invisible from outside (#272).
    eprintln!("guestfn-init: up");
    if let Err(e) = run() {
        eprintln!("guestfn-init: {e}");
    }
    poweroff();
}

fn run() -> Result<(), String> {
    mount_base()?;
    loopback_up()?;
    let mut sock = connect_host()?;
    write_json(&mut sock, Kind::Hello, &Hello {})
        .map_err(|e| format!("cannot greet the host: {e}"))?;
    // The host answers with the run, or parks the init first: it is about
    // to snapshot the VM. A parked init waits on the connection it has,
    // sending nothing, so the VM is still when it is frozen. A restore
    // resets every vsock connection, which ends that wait in the clone,
    // after the guest's driver has handled the reset: the init then
    // connects to the clone's host as after a boot. The clone's clock and
    // random state are the snapshot's until the run's description
    // refreshes them.
    let (kind, payload) = read_frame(&mut sock).map_err(|e| format!("cannot read the run: {e}"))?;
    let description: Description = match kind {
        Kind::Run => {
            serde_json::from_slice(&payload).map_err(|e| format!("cannot read the run: {e}"))?
        }
        Kind::Park => {
            eprintln!("guestfn-init: parked");
            write_frame(&mut sock, Kind::Parked, &[])
                .map_err(|e| format!("cannot answer the park: {e}"))?;
            // Whatever ends the wait, an error or a close, the connection
            // is gone and the next host is the one to greet.
            let _ = read_frame(&mut sock);
            drop(sock);
            sock = connect_vsock(PORT, 2_000, Duration::from_millis(1))?;
            write_json(&mut sock, Kind::Hello, &Hello {})
                .map_err(|e| format!("cannot greet the host: {e}"))?;
            let description: Description =
                read_json(&mut sock, Kind::Run).map_err(|e| format!("cannot read the run: {e}"))?;
            refresh(&description);
            eprintln!("guestfn-init: resumed");
            description
        }
        other => return Err(format!("unexpected {other:?} frame in place of the run")),
    };
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
    eprintln!(
        "guestfn-init: running {}",
        description.argv.first().map(String::as_str).unwrap_or("")
    );
    let sock = Arc::new(Mutex::new(sock));
    let exit = match spawn(&description, &sock) {
        Ok(exit) => exit,
        Err(error) => Exit {
            error,
            ..Default::default()
        },
    };
    eprintln!(
        "guestfn-init: exited status={:?} signal={:?} oom={}",
        exit.status, exit.signal, exit.oom
    );
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
fn spawn(d: &Description, sock: &Arc<Mutex<UnixStream>>) -> Result<Exit, String> {
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
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // The engine refuses an image that runs as root at load; the init is
    // where the rule is enforced, whatever described the run.
    if d.uid == 0 {
        return Err("the run describes a root process, which the init does not start".to_string());
    }
    let (uid, gid) = (d.uid, d.gid);
    let groups: Vec<libc::gid_t> = d.groups.iter().map(|g| *g as libc::gid_t).collect();
    // SAFETY: the closure makes the identity and capability syscalls and
    // nothing else between fork and exec.
    unsafe {
        cmd.pre_exec(move || drop_privileges(uid, gid, &groups));
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
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let out = relay(stdout, Kind::Stdout, Arc::clone(sock));
    let err = relay(stderr, Kind::Stderr, Arc::clone(sock));
    let ended = Arc::new(AtomicBool::new(false));
    bridge_when_serving(Arc::clone(&ended));
    stop_on_finish(sock, child.id(), Arc::clone(&ended))?;
    let status = child
        .wait()
        .map_err(|e| format!("cannot wait for {program}: {e}"))?;
    ended.store(true, Ordering::SeqCst);
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

/// Waits for the function's server to accept a connection, then bridges it
/// to the host, which calls the function over it. A process that never
/// serves leaves the host to its deadline, or to the exit the init reports.
fn bridge_when_serving(ended: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let server = loop {
            if ended.load(Ordering::SeqCst) {
                return;
            }
            match TcpStream::connect(("127.0.0.1", SERVER_PORT)) {
                Ok(s) => break s,
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        };
        let _ = server.set_nodelay(true);
        let host = match connect_vsock(GRPC_PORT, 200, Duration::from_millis(5)) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("guestfn-init: cannot bridge the function: {e}");
                return;
            }
        };
        let (Ok(mut server_read), Ok(mut host_write)) = (server.try_clone(), host.try_clone())
        else {
            eprintln!("guestfn-init: cannot share the function's connections");
            return;
        };
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut server_read, &mut host_write);
            let _ = host_write.shutdown(std::net::Shutdown::Write);
        });
        let (mut host_read, mut server_write) = (host, server);
        let _ = std::io::copy(&mut host_read, &mut server_write);
        let _ = server_write.shutdown(std::net::Shutdown::Write);
    });
}

/// Stops the process when the host says the call is over: a server would
/// otherwise run until the VM is killed, and what it logged with it.
fn stop_on_finish(
    sock: &Arc<Mutex<UnixStream>>,
    pid: u32,
    ended: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut reader = sock
        .lock()
        .expect("poisoned")
        .try_clone()
        .map_err(|e| format!("cannot share the host's connection: {e}"))?;
    std::thread::spawn(move || {
        while let Ok((kind, _)) = read_frame(&mut reader) {
            if kind == Kind::Finish {
                if !ended.load(Ordering::SeqCst) {
                    // SAFETY: a signal to the child this process has not
                    // reaped yet.
                    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                }
                return;
            }
        }
    });
    Ok(())
}

/// Relays a stream to the host in frames as it arrives, so a guest's log
/// lines reach the host while it runs.
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
    connect_vsock(PORT, 200, Duration::from_millis(25))
}

/// The capability header and one of its two data words, as capset(2)
/// takes them at version 3 (64 capability bits).
#[repr(C)]
struct CapHeader {
    version: u32,
    pid: libc::c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

const CAPABILITY_VERSION_3: u32 = 0x2008_0522;

/// Makes the calling process the described user with no privilege and no
/// way back to one, between fork and exec: the bounding set emptied while
/// the init's capabilities still allow it, the identity, then the ambient,
/// permitted, effective and inheritable sets cleared and no_new_privs set,
/// so neither a setuid binary nor a file capability in the image raises
/// what the process may do. The guest's root is then the init alone, which
/// is what lets the init hold what the process must not reach.
fn drop_privileges(
    uid: libc::uid_t,
    gid: libc::gid_t,
    groups: &[libc::gid_t],
) -> std::io::Result<()> {
    let failed = std::io::Error::last_os_error;
    let zero: libc::c_ulong = 0;
    // The kernel answers EINVAL for the first number past its last
    // capability; 64 is what the capability sets can hold.
    for capability in 0..64 as libc::c_ulong {
        // SAFETY: prctl reads its integer arguments and nothing else.
        if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, capability, zero, zero, zero) } != 0 {
            let error = failed();
            if error.raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(error);
        }
    }
    // SAFETY: the slice is as long as stated, and the calls read their
    // arguments alone.
    unsafe {
        if libc::setgroups(groups.len(), groups.as_ptr()) != 0 {
            return Err(failed());
        }
        if libc::setgid(gid) != 0 {
            return Err(failed());
        }
        if libc::setuid(uid) != 0 {
            return Err(failed());
        }
        if libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong,
            zero,
            zero,
            zero,
        ) != 0
        {
            return Err(failed());
        }
    }
    let header = CapHeader {
        version: CAPABILITY_VERSION_3,
        pid: 0,
    };
    let none = [CapData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    // SAFETY: the header and the two data words are the layout version 3
    // of the call reads, alive for the call.
    if unsafe { libc::syscall(libc::SYS_capset, &header, none.as_ptr()) } != 0 {
        return Err(failed());
    }
    let one: libc::c_ulong = 1;
    // SAFETY: prctl reads its integer arguments and nothing else.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, one, zero, zero, zero) } != 0 {
        return Err(failed());
    }
    Ok(())
}

/// Brings up the loopback interface, which the kernel leaves down: the
/// function's server listens on it. It's the VM's one interface.
fn loopback_up() -> Result<(), String> {
    // SAFETY: socket(2) with constant arguments.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!(
            "cannot open a socket to configure loopback: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: ifreq is plain data; zeroed is a valid value.
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request.ifr_name.iter_mut().zip(b"lo") {
        *slot = *byte as libc::c_char;
    }
    // SAFETY: a valid fd and an ifreq naming the interface; the union's
    // flags are what these two requests read and write.
    let rc = unsafe {
        let mut rc = libc::ioctl(fd, libc::SIOCGIFFLAGS as _, &mut request);
        if rc == 0 {
            request.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
            rc = libc::ioctl(fd, libc::SIOCSIFFLAGS as _, &request);
        }
        rc
    };
    let error = std::io::Error::last_os_error();
    // SAFETY: the fd is ours and not used again.
    unsafe { libc::close(fd) };
    if rc != 0 {
        return Err(format!("cannot bring up loopback: {error}"));
    }
    Ok(())
}

/// What a restored clone must not share with its siblings or keep from the
/// snapshot's moment: the wall clock, set to the host's, and the kernel's
/// random state, reseeded with the host's fresh bytes. Best effort: a
/// failure leaves the guest as the snapshot had it, which the console says.
fn refresh(d: &Description) {
    if d.now_unix_ns != 0 {
        let ts = libc::timespec {
            tv_sec: (d.now_unix_ns / 1_000_000_000) as _,
            tv_nsec: (d.now_unix_ns % 1_000_000_000) as _,
        };
        // SAFETY: a valid timespec; PID 1 may set the clock.
        if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } != 0 {
            eprintln!(
                "guestfn-init: cannot set the clock: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    let seed: Vec<u8> = (0..d.seed.len() / 2)
        .filter_map(|i| u8::from_str_radix(d.seed.get(2 * i..2 * i + 2)?, 16).ok())
        .take(64)
        .collect();
    if seed.is_empty() {
        return;
    }
    // RNDADDENTROPY credits the bytes to the input pool and RNDRESEEDCRNG
    // makes the generator take them now rather than at its next interval.
    const RNDADDENTROPY: u64 = 0x4008_5203;
    const RNDRESEEDCRNG: u64 = 0x5207;
    #[repr(C)]
    struct Pool {
        entropy_count: libc::c_int,
        buf_size: libc::c_int,
        buf: [u8; 64],
    }
    let mut pool = Pool {
        entropy_count: (seed.len() * 8) as libc::c_int,
        buf_size: seed.len() as libc::c_int,
        buf: [0; 64],
    };
    pool.buf[..seed.len()].copy_from_slice(&seed);
    match std::fs::OpenOptions::new().write(true).open("/dev/urandom") {
        Ok(f) => {
            use std::os::unix::io::AsRawFd as _;
            // SAFETY: a valid fd and a rand_pool_info of the stated size.
            let added = unsafe { libc::ioctl(f.as_raw_fd(), RNDADDENTROPY as _, &pool) };
            // SAFETY: a valid fd; the request takes no argument.
            let reseeded = unsafe { libc::ioctl(f.as_raw_fd(), RNDRESEEDCRNG as _, 0) };
            if added != 0 || reseeded != 0 {
                eprintln!(
                    "guestfn-init: cannot reseed the kernel's randomness: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        Err(e) => eprintln!("guestfn-init: cannot open /dev/urandom to reseed: {e}"),
    }
}

/// Connects to a port of the host, trying again after each refusal.
fn connect_vsock(port: u32, tries: u32, pause: Duration) -> Result<UnixStream, String> {
    let mut last = String::new();
    for _ in 0..tries {
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
        addr.svm_cid = guestfn_oci_protocol::HOST_CID;
        addr.svm_port = port;
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
        std::thread::sleep(pause);
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
