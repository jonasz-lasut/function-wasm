//! What the kernel lets the process do, for the oci privileges suite: the
//! OCI runtime's init starts an image's process as a non-root user with
//! every capability set emptied and no_new_privs set, so each operation
//! below, which a capability would allow, is denied, and a setuid-root
//! binary in the image runs as the same user. A WebAssembly guest has no
//! such operations to try, so the probe exists in the process build alone.

use crate::PrivilegesStatus;

/// The setuid-root copy of this binary the suite's setup puts in the image.
pub const SETUID_COPY: &str = "/e2e-guest-setuid";
/// The argument that makes the binary print its effective user and exit.
pub const EUID_ARGUMENT: &str = "euid";

#[cfg(not(all(feature = "process", target_os = "linux")))]
pub fn probe() -> Option<PrivilegesStatus> {
    None
}

#[cfg(all(feature = "process", target_os = "linux"))]
pub fn probe() -> Option<PrivilegesStatus> {
    Some(linux::probe())
}

#[cfg(all(feature = "process", target_os = "linux"))]
mod linux {
    use std::collections::BTreeMap;
    use std::ffi::CString;
    use std::io::{Error, Result};
    use std::os::unix::fs::MetadataExt as _;

    use super::{EUID_ARGUMENT, SETUID_COPY};
    use crate::{PrivilegesStatus, SetuidStatus};

    /// The run's cgroup, where the init wrote the memory limit.
    const MEMORY_LIMIT: &str = "/sys/fs/cgroup/run/memory.max";
    /// The image's root filesystem, as the VM's block device.
    const BLOCK_DEVICE: &str = "/dev/vda";

    pub fn probe() -> PrivilegesStatus {
        // SAFETY: the call takes no argument and always succeeds.
        let uid = unsafe { libc::getuid() };
        PrivilegesStatus {
            uid,
            capabilities: capabilities(),
            attempts: attempts(),
            setuid: setuid(),
        }
    }

    /// The capability sets and the no-new-privileges bit as
    /// /proc/self/status prints them.
    fn capabilities() -> BTreeMap<String, String> {
        std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once(':'))
            .filter(|(key, _)| key.starts_with("Cap") || *key == "NoNewPrivs")
            .map(|(key, value)| (key.to_string(), value.trim().to_string()))
            .collect()
    }

    /// Each operation by the capability that would allow it, and how the
    /// kernel answered: `denied` for EPERM or EACCES, `allowed`, or the
    /// error when it failed another way.
    fn attempts() -> BTreeMap<String, String> {
        type Operation = (&'static str, fn() -> Result<()>);
        let operations: [Operation; 14] = [
            ("CAP_SYS_ADMIN mount", mount),
            ("CAP_SYS_ADMIN sethostname", sethostname),
            ("CAP_SYS_CHROOT chroot", chroot),
            ("CAP_SETUID setuid", || check(unsafe { libc::setuid(0) })),
            ("CAP_SETGID setgid", || check(unsafe { libc::setgid(0) })),
            ("CAP_KILL signal the init", || {
                check(unsafe { libc::kill(1, 0) })
            }),
            ("CAP_SYS_PTRACE trace the init", ptrace),
            ("CAP_NET_BIND_SERVICE bind port 80", || {
                std::net::TcpListener::bind("127.0.0.1:80").map(drop)
            }),
            ("CAP_NET_ADMIN mark a socket", mark_socket),
            ("CAP_NET_RAW raw socket", raw_socket),
            ("CAP_MKNOD mknod", mknod),
            ("CAP_CHOWN chown to root", chown),
            ("CAP_SYS_TIME set the clock", set_clock),
            ("CAP_DAC_OVERRIDE write the memory limit", || {
                std::fs::write(MEMORY_LIMIT, "max")
            }),
        ];
        let mut out: BTreeMap<String, String> = operations
            .iter()
            .map(|(name, operation)| (name.to_string(), outcome(operation())))
            .collect();
        out.insert(
            "CAP_DAC_READ_SEARCH read the block device".to_string(),
            outcome(std::fs::File::open(BLOCK_DEVICE).map(drop)),
        );
        out
    }

    fn outcome(result: Result<()>) -> String {
        match result {
            Ok(()) => "allowed".to_string(),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => "denied".to_string(),
            Err(e) => format!("failed: {e}"),
        }
    }

    fn check(returned: libc::c_int) -> Result<()> {
        if returned == 0 {
            Ok(())
        } else {
            Err(Error::last_os_error())
        }
    }

    fn c(text: &str) -> CString {
        CString::new(text).expect("no NUL")
    }

    fn mount() -> Result<()> {
        let (source, target) = (c("tmpfs"), c("/tmp"));
        // SAFETY: valid C strings, no data argument.
        check(unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                source.as_ptr(),
                0,
                std::ptr::null(),
            )
        })
    }

    fn sethostname() -> Result<()> {
        let name = b"escaped";
        // SAFETY: the buffer is as long as stated.
        check(unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) })
    }

    fn chroot() -> Result<()> {
        let dir = c("/tmp");
        // SAFETY: a valid C string.
        check(unsafe { libc::chroot(dir.as_ptr()) })
    }

    fn ptrace() -> Result<()> {
        // SAFETY: the request takes a pid and two ignored arguments.
        let returned = unsafe {
            libc::ptrace(
                libc::PTRACE_ATTACH,
                1 as libc::pid_t,
                std::ptr::null_mut::<libc::c_void>(),
                std::ptr::null_mut::<libc::c_void>(),
            )
        };
        if returned == -1 {
            Err(Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn mark_socket() -> Result<()> {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let mark: libc::c_int = 1;
        // SAFETY: the option value is an int of the stated size.
        check(unsafe {
            libc::setsockopt(
                std::os::fd::AsRawFd::as_raw_fd(&socket),
                libc::SOL_SOCKET,
                libc::SO_MARK,
                (&raw const mark).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        })
    }

    fn raw_socket() -> Result<()> {
        // SAFETY: socket takes three integers.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_RAW, libc::IPPROTO_ICMP) };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        // SAFETY: the descriptor was just opened here.
        unsafe { libc::close(fd) };
        Ok(())
    }

    fn mknod() -> Result<()> {
        let path = c("/tmp/e2e-device");
        // The null device's numbers.
        let device = libc::makedev(1, 3);
        // SAFETY: a valid C string and integers.
        check(unsafe { libc::mknod(path.as_ptr(), libc::S_IFCHR | 0o600, device) })
    }

    fn chown() -> Result<()> {
        let path = "/tmp/e2e-owned";
        std::fs::write(path, b"")?;
        let path = c(path);
        // SAFETY: a valid C string and integers.
        check(unsafe { libc::chown(path.as_ptr(), 0, 0) })
    }

    fn set_clock() -> Result<()> {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: the kernel writes one timespec, then reads it back.
        unsafe {
            check(libc::clock_gettime(libc::CLOCK_REALTIME, &raw mut now))?;
            check(libc::clock_settime(libc::CLOCK_REALTIME, &raw const now))
        }
    }

    /// Runs the setuid-root copy of this binary and reads the effective
    /// user it prints.
    fn setuid() -> SetuidStatus {
        let mut status = SetuidStatus::default();
        match std::fs::metadata(SETUID_COPY) {
            Ok(m) => {
                status.owner = Some(m.uid());
                status.bit = m.mode() & 0o4000 != 0;
            }
            Err(e) => {
                status.error = Some(format!("stat {SETUID_COPY}: {e}"));
                return status;
            }
        }
        match std::process::Command::new(SETUID_COPY)
            .arg(EUID_ARGUMENT)
            .output()
        {
            Ok(output) => match String::from_utf8_lossy(&output.stdout).trim().parse() {
                Ok(euid) => status.euid = Some(euid),
                Err(_) => {
                    status.error = Some(format!(
                        "{SETUID_COPY} printed {:?} and {:?}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    ));
                }
            },
            Err(e) => status.error = Some(format!("run {SETUID_COPY}: {e}")),
        }
        status
    }
}
