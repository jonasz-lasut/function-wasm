//! The process the OCI engine's tests run inside a VM: a static binary,
//! put into a one-layer image by the test, whose first argument says how
//! to behave. Each mode exercises one clause of the process contract.

use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("echo");
    match mode {
        // The contract's happy path: the response is the request.
        "echo" => echo(),
        "exit" => {
            eprintln!("exiting as asked");
            std::process::exit(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3));
        }
        // A plain line and a JSON line on stderr, then the echo.
        "stderr" => {
            eprintln!("a plain line from the guest");
            eprintln!(r#"{{"level":"warn","msg":"a structured line","answer":42}}"#);
            echo();
        }
        "spin" => loop {
            std::hint::spin_loop();
        },
        // Touch every page of the asked memory, so the limit is met.
        "grow" => {
            let mib: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(64);
            let mut v: Vec<u8> = Vec::with_capacity(mib << 20);
            for i in 0..(mib << 20) {
                v.push((i % 251) as u8);
            }
            println!("grew {} bytes", v.len());
        }
        "env" => {
            let mut vars: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
            vars.sort();
            println!("{}", vars.join("\n"));
        }
        "cwd" => println!(
            "{}",
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ),
        "id" => {
            // SAFETY: the three calls take no arguments and always succeed.
            let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
            let mut groups = vec![0 as libc::gid_t; 64];
            // SAFETY: the buffer is as long as stated.
            let n = unsafe { libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr()) };
            groups.truncate(n.max(0) as usize);
            groups.sort();
            let groups: Vec<String> = groups.iter().map(|g| g.to_string()).collect();
            println!("uid={uid} gid={gid} groups={}", groups.join(","));
        }
        "tmp" => {
            std::fs::write("/tmp/probe", b"scratch").expect("write /tmp/probe");
            let back = std::fs::read("/tmp/probe").expect("read /tmp/probe");
            let root = std::fs::write("/probe", b"overlay")
                .map(|_| "writable")
                .unwrap_or("read-only");
            println!("tmp={} root={root}", String::from_utf8_lossy(&back));
        }
        "hostname" => {
            let name = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
            println!("{}", name.trim());
        }
        // No reply at all: the host's deadline ends the run, or the exit
        // without a response is the fatal.
        "silent" => {}
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}

fn echo() {
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .expect("read stdin");
    std::io::stdout().write_all(&input).expect("write stdout");
}
