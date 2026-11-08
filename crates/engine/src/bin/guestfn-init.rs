//! The OCI runtime's guest init: the entry point of the `guestfn-init`
//! binary. The init itself is `guestfn-init/linux.rs`, Linux alone: it
//! mounts, pivots, opens a vsock and powers the VM off through libc calls
//! that exist nowhere else, and it runs as PID 1 of a microVM, never on a
//! developer's host. This shim keeps the workspace building on every host
//! (`cargo test --workspace`, clippy over every target on a Mac) and makes
//! the binary say what it is when run elsewhere.

#[cfg(target_os = "linux")]
#[path = "guestfn-init/linux.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "guestfn-init is the OCI runtime's guest init: it runs as PID 1 of a Linux microVM (docs/one-pager-oci-runtime.md) and does nothing on this platform"
    );
    std::process::exit(1);
}
