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
        // Sixteen bytes of the kernel's randomness as hex: two clones of
        // one snapshot must not print the same.
        "random" => {
            let mut bytes = [0u8; 16];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut bytes))
                .expect("/dev/urandom");
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            print!("{hex}");
        }
        // The wall clock in seconds since the epoch: a restored clone's
        // must be the host's, not the snapshot's.
        "clock" => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a clock after the epoch");
            print!("{}", now.as_secs());
        }
        // Take the asked memory in chunks, touching every page, so the
        // kernel's limit is what ends it: one reservation of the whole
        // would fail by overcommit's heuristic before any page is used.
        "grow" => {
            let mib: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(64);
            let mut held: Vec<Vec<u8>> = Vec::new();
            for _ in 0..mib.div_ceil(8) {
                let mut chunk: Vec<u8> = Vec::with_capacity(8 << 20);
                for i in 0..(8 << 20) {
                    chunk.push((i % 251) as u8);
                }
                held.push(chunk);
            }
            println!("grew {} bytes", held.iter().map(Vec::len).sum::<usize>());
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
        // Whether an earlier run left anything behind, then leave something:
        // a second run must find both paths absent again.
        "leftover" => {
            let seen = |path: &str| {
                if std::path::Path::new(path).exists() {
                    "present"
                } else {
                    "absent"
                }
            };
            print!("tmp={} root={}", seen("/tmp/probe"), seen("/probe"));
            std::fs::write("/tmp/probe", b"scratch").expect("write /tmp/probe");
            std::fs::write("/probe", b"overlay").expect("write /probe");
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
