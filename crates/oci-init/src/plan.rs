//! What the init decides without touching the kernel: which of the host's
//! frames starts the run, the command a description amounts to, whether
//! the kernel's log says the OOM killer acted, the bytes of a seed. The
//! init itself (`linux.rs`) runs only as PID 1 of a microVM, so these are
//! kept apart from it to be tested on any host: the VM suite takes none of
//! their refusing branches, the engine having refused first.

use std::process::{Command, Stdio};

use guestfn_oci_protocol::{Description, Kind};

/// What the host's first frame asks of the init.
#[derive(Debug, PartialEq, Eq)]
pub enum First {
    /// Run this description now.
    Run(Description),
    /// Park first: the host is about to snapshot the VM, and the run comes
    /// over a new connection once a clone is restored.
    Park,
}

/// Reads the frame the host answers the init's greeting with.
pub fn first(kind: Kind, payload: &[u8]) -> Result<First, String> {
    match kind {
        Kind::Run => serde_json::from_slice(payload)
            .map(First::Run)
            .map_err(|e| format!("cannot read the run: {e}")),
        Kind::Park => Ok(First::Park),
        other => Err(format!("unexpected {other:?} frame in place of the run")),
    }
}

/// The process as the description states it, short of its identity (the
/// init drops privileges between fork and exec): the program and its
/// arguments, exactly the described environment, the working directory,
/// no stdin and both output streams piped. A description with no program,
/// or of a root process, is refused.
pub fn command(d: &Description) -> Result<Command, String> {
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
    Ok(cmd)
}

/// Whether the kernel log says the OOM killer acted: a SIGKILL from it is
/// the run's memory limit, which the host names as such.
pub fn mentions_oom(kernel_log: &str) -> bool {
    let lower = kernel_log.to_ascii_lowercase();
    lower.contains("out of memory") || lower.contains("oom-kill")
}

/// The bytes of a hex seed, at most the 64 the kernel's pool takes at
/// once; a pair that is not hex is left out.
pub fn seed(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok())
        .take(64)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use guestfn_oci_protocol::{Hello, read_frame, write_frame, write_json};

    use super::*;

    fn description() -> Description {
        Description {
            argv: vec!["/fn".to_string(), "--insecure".to_string()],
            env: vec!["A=1".to_string(), "B=x=y".to_string()],
            cwd: "/work".to_string(),
            uid: 1000,
            gid: 1000,
            ..Default::default()
        }
    }

    /// The frame as the host writes it and the init reads it.
    fn framed(write: impl FnOnce(&mut Vec<u8>)) -> (Kind, Vec<u8>) {
        let mut wire = Vec::new();
        write(&mut wire);
        read_frame(&mut wire.as_slice()).expect("a frame")
    }

    #[test]
    fn a_run_frame_is_the_run() {
        let want = description();
        let (kind, payload) = framed(|w| write_json(w, Kind::Run, &want).expect("write the run"));
        assert_eq!(first(kind, &payload), Ok(First::Run(want)));
    }

    #[test]
    fn a_park_frame_parks_the_init() {
        let (kind, payload) = framed(|w| write_frame(w, Kind::Park, &[]).expect("write"));
        assert_eq!(first(kind, &payload), Ok(First::Park));
    }

    #[test]
    fn a_run_that_is_not_a_description_is_refused() {
        let (kind, payload) = framed(|w| write_frame(w, Kind::Run, b"{").expect("write"));
        let refusal = first(kind, &payload).expect_err("a refusal");
        assert!(refusal.starts_with("cannot read the run: "), "{refusal}");
    }

    #[test]
    fn another_frame_in_place_of_the_run_is_refused() {
        let frames: [(Kind, Vec<u8>); 3] = [
            framed(|w| write_json(w, Kind::Hello, &Hello {}).expect("write")),
            framed(|w| write_frame(w, Kind::Finish, &[]).expect("write")),
            framed(|w| write_frame(w, Kind::Stdout, b"x").expect("write")),
        ];
        for (kind, payload) in frames {
            assert_eq!(
                first(kind, &payload),
                Err(format!("unexpected {kind:?} frame in place of the run"))
            );
        }
    }

    #[test]
    fn the_command_is_the_description_s() {
        let cmd = command(&description()).expect("a command");
        assert_eq!(cmd.get_program(), "/fn");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["--insecure"]);
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/work")));
        // Exactly the described variables, the value kept past its first
        // `=`: nothing of the init's own environment is inherited.
        assert_eq!(
            cmd.get_envs().collect::<Vec<_>>(),
            [
                (OsStr::new("A"), Some(OsStr::new("1"))),
                (OsStr::new("B"), Some(OsStr::new("x=y"))),
            ]
        );
    }

    #[test]
    fn an_entry_that_is_no_assignment_is_left_out_of_the_environment() {
        let mut d = description();
        d.env = vec!["NOT_AN_ASSIGNMENT".to_string(), "A=1".to_string()];
        let cmd = command(&d).expect("a command");
        assert_eq!(
            cmd.get_envs().collect::<Vec<_>>(),
            [(OsStr::new("A"), Some(OsStr::new("1")))]
        );
    }

    #[test]
    fn no_working_directory_is_the_root() {
        let mut d = description();
        d.cwd.clear();
        let cmd = command(&d).expect("a command");
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/")));
    }

    #[test]
    fn a_description_with_no_program_is_refused() {
        let mut d = description();
        d.argv.clear();
        assert_eq!(
            command(&d).expect_err("a refusal"),
            "the image has neither an entrypoint nor a command"
        );
    }

    /// The engine refuses such an image at load, so no VM test reaches the
    /// init's own refusal: it is the second line, held here.
    #[test]
    fn a_root_process_is_refused() {
        let mut d = description();
        d.uid = 0;
        assert_eq!(
            command(&d).expect_err("a refusal"),
            "the run describes a root process, which the init does not start"
        );
        // A root group alone is not a root process.
        let mut d = description();
        d.gid = 0;
        assert!(command(&d).is_ok());
    }

    #[test]
    fn the_kernel_s_oom_lines_are_recognised() {
        // /dev/kmsg records as Linux 6.1 writes them for a cgroup's limit
        // and for the whole machine's.
        let cgroup = "4,512,3411522,-;fixture invoked oom-killer: gfp_mask=0xcc0(GFP_KERNEL), order=0, oom_score_adj=0\n\
            6,530,3411980,-;oom-kill:constraint=CONSTRAINT_MEMCG,nodemask=(null),cpuset=/,mems_allowed=0,oom_memcg=/run,task_memcg=/run,task=fixture,pid=312,uid=1000\n\
            3,531,3412010,-;Memory cgroup out of memory: Killed process 312 (fixture) total-vm:212000kB, anon-rss:97000kB\n";
        let machine =
            "3,640,5120033,-;Out of memory: Killed process 312 (fixture) total-vm:212000kB\n";
        for log in [cgroup, machine] {
            assert!(mentions_oom(log), "{log}");
            for line in log.lines() {
                assert!(mentions_oom(line), "{line}");
            }
        }
    }

    #[test]
    fn a_log_with_no_oom_kill_is_not_one() {
        let boot = "6,0,0,-;Linux version 6.1.141 (gcc 13.3.0)\n\
            6,210,812000,-;Memory: 148232K/163840K available (10240K kernel code)\n\
            6,455,1502113,-;Freeing unused kernel memory: 1408K\n\
            6,470,1620441,-;room for 16 more\n";
        assert!(!mentions_oom(boot));
        assert!(!mentions_oom(""));
    }

    #[test]
    fn a_seed_is_its_hex_bytes() {
        assert_eq!(seed("00ff10"), [0x00, 0xff, 0x10]);
        assert_eq!(seed(""), Vec::<u8>::new());
        // A trailing half pair is dropped and a pair that is not hex is
        // left out.
        assert_eq!(seed("0a0"), [0x0a]);
        assert_eq!(seed("0azz0b"), [0x0a, 0x0b]);
        // The kernel's pool takes 64 bytes at once.
        assert_eq!(seed(&"ab".repeat(100)), vec![0xab; 64]);
    }
}
