//! The function the OCI engine's tests run inside a VM: a static binary on
//! function-sdk-rust, started the way a user's image is, put into a
//! one-layer image by the test. Its positional arguments say how to
//! behave, and each mode exercises one clause of the contract. What a mode
//! reports goes back in the response's meta tag.

use std::io::Read;
use std::process::ExitCode;

use clap::Parser;
use function_sdk_rust::proto::v1::function_runner_service_server::FunctionRunnerService;
use function_sdk_rust::proto::v1::{ResponseMeta, RunFunctionRequest, RunFunctionResponse};
use function_sdk_rust::{Args, serve};
use tonic::{Request, Response, Status};

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    sdk: Args,
    /// How to behave.
    #[arg(default_value = "echo")]
    mode: String,
    /// The mode's own value: the exit status, the memory to grow by.
    value: Option<String>,
}

struct Fixture {
    mode: String,
    value: Option<String>,
}

fn answer(tag: String) -> Result<Response<RunFunctionResponse>, Status> {
    Ok(Response::new(RunFunctionResponse {
        meta: Some(ResponseMeta { tag, ttl: None }),
        ..Default::default()
    }))
}

#[tonic::async_trait]
impl FunctionRunnerService for Fixture {
    async fn run_function(
        &self,
        request: Request<RunFunctionRequest>,
    ) -> Result<Response<RunFunctionResponse>, Status> {
        let tag = request
            .into_inner()
            .meta
            .map(|meta| meta.tag)
            .unwrap_or_default();
        match self.mode.as_str() {
            // The contract's happy path: the answer is what was asked.
            "echo" => answer(tag),
            // A plain line and a JSON line, then the echo.
            "stderr" => {
                eprintln!("a plain line from the guest");
                eprintln!(r#"{{"level":"warn","msg":"a structured line","answer":42}}"#);
                answer(tag)
            }
            // A function that answers with an error.
            "refuse" => Err(Status::failed_precondition("refused as asked")),
            "spin" => loop {
                std::hint::spin_loop();
            },
            // Sixteen bytes of the kernel's randomness as hex: two clones
            // of one snapshot must not draw the same.
            "random" => {
                let mut bytes = [0u8; 16];
                std::fs::File::open("/dev/urandom")
                    .and_then(|mut f| f.read_exact(&mut bytes))
                    .expect("/dev/urandom");
                answer(bytes.iter().map(|b| format!("{b:02x}")).collect())
            }
            // The wall clock in seconds since the epoch: a restored
            // clone's must be the host's, not the snapshot's.
            "clock" => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("a clock after the epoch");
                answer(now.as_secs().to_string())
            }
            // Take the asked memory in chunks, touching every page, so the
            // kernel's limit is what ends it: one reservation of the whole
            // would fail by overcommit's heuristic before any page is used.
            "grow" => {
                let mib: usize = self
                    .value
                    .as_deref()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(64);
                let mut held: Vec<Vec<u8>> = Vec::new();
                for _ in 0..mib.div_ceil(8) {
                    let mut chunk: Vec<u8> = Vec::with_capacity(8 << 20);
                    for i in 0..(8 << 20) {
                        chunk.push((i % 251) as u8);
                    }
                    held.push(chunk);
                }
                answer(format!(
                    "grew {} bytes",
                    held.iter().map(Vec::len).sum::<usize>()
                ))
            }
            "env" => {
                let mut vars: Vec<String> =
                    std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
                vars.sort();
                answer(vars.join("\n"))
            }
            "cwd" => answer(
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            "id" => {
                // SAFETY: the calls take no arguments and always succeed.
                let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
                let mut groups = vec![0 as libc::gid_t; 64];
                // SAFETY: the buffer is as long as stated.
                let n =
                    unsafe { libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr()) };
                groups.truncate(n.max(0) as usize);
                groups.sort();
                let groups: Vec<String> = groups.iter().map(|g| g.to_string()).collect();
                answer(format!("uid={uid} gid={gid} groups={}", groups.join(",")))
            }
            "tmp" => {
                std::fs::write("/tmp/probe", b"scratch").expect("write /tmp/probe");
                let back = std::fs::read("/tmp/probe").expect("read /tmp/probe");
                let root = std::fs::write("/probe", b"overlay")
                    .map(|_| "writable")
                    .unwrap_or("read-only");
                answer(format!(
                    "tmp={} root={root}",
                    String::from_utf8_lossy(&back)
                ))
            }
            // Whether an earlier run left anything behind, then leave
            // something: a second run must find both paths absent again.
            "leftover" => {
                let seen = |path: &str| {
                    if std::path::Path::new(path).exists() {
                        "present"
                    } else {
                        "absent"
                    }
                };
                let report = format!("tmp={} root={}", seen("/tmp/probe"), seen("/probe"));
                std::fs::write("/tmp/probe", b"scratch").expect("write /tmp/probe");
                std::fs::write("/probe", b"overlay").expect("write /probe");
                answer(report)
            }
            "rng" => answer(
                std::fs::read_to_string("/sys/class/misc/hw_random/rng_current")
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            ),
            "hostname" => answer(
                std::fs::read_to_string("/proc/sys/kernel/hostname")
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            ),
            other => Err(Status::invalid_argument(format!("unknown mode {other}"))),
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.mode.as_str() {
        // Ends before serving: the exit is the run's verdict.
        "exit" => {
            eprintln!("exiting as asked");
            return ExitCode::from(cli.value.and_then(|s| s.parse().ok()).unwrap_or(3));
        }
        "silent" => return ExitCode::SUCCESS,
        _ => {}
    }
    let fixture = Fixture {
        mode: cli.mode,
        value: cli.value,
    };
    if let Err(e) = serve(fixture, &cli.sdk).await {
        eprintln!("cannot serve: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
