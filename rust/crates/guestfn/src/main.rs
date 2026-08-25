//! guestfn scaffolds, builds, inspects and publishes guest modules for
//! function-wasm. It checks modules with the runtime's own engine
//! (wasmtime), so its verdicts are the runtime's.

mod buildcmd;
mod composition;
mod inspect;
mod manifestcmd;
mod push;
mod scaffold;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// The Input's identity in a Composition step.
pub(crate) const INPUT_API_VERSION: &str = "wasm.fn.crossplane.io/v1beta1";
pub(crate) const INPUT_KIND: &str = "Input";

/// Used when go is not on PATH to say better, so a scaffold is never left
/// without a version.
const FALLBACK_SDK_VERSION: &str = "v0.7.1";
const FALLBACK_GO_VERSION: &str = "1.26";

#[derive(Parser, Debug)]
#[command(
    name = "guestfn",
    version,
    about = "Scaffold, build and publish WebAssembly guest functions for function-wasm."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Scaffold a new guest project.
    Init(InitCmd),
    /// Compile a guest project to a wasip1 module, check its ABI and its
    /// manifest (wasmfn.yaml).
    Build(buildcmd::BuildCmd),
    /// Push a module to an OCI registry as a wasm artifact; a module the
    /// runtime would refuse is not pushed.
    Push(push::PushCmd),
    /// Show what a module (or an artifact in a registry) is made of, as the
    /// runtime sees it: size, ABI verdict, exports, imports, memories,
    /// manifest.
    Inspect(inspect::InspectCmd),
    /// Validate a module manifest (wasmfn.yaml) or print the one an
    /// artifact carries.
    #[command(subcommand)]
    Manifest(manifestcmd::ManifestCmd),
    /// Print a Composition step (or a Composition) for a module, from its
    /// manifest.
    #[command(subcommand)]
    Scaffold(ScaffoldCmd),
}

#[derive(clap::Subcommand, Debug)]
enum ScaffoldCmd {
    /// Print a Composition step for a module - its module source, the
    /// sandbox its manifest requires, a config skeleton from the manifest's
    /// schema - or a whole Composition with --full.
    Composition(composition::CompositionCmd),
}

#[derive(clap::Args, Debug)]
struct InitCmd {
    /// Directory to create the project in.
    dir: PathBuf,

    /// Language of the project: go (function-sdk-go), tinygo (raw protobuf
    /// messages, ~1 MB modules), rust (prost), zig (zig-protobuf, ~95 KB)
    /// or c (nanopb, built by zig cc, ~70 KB).
    #[arg(long, default_value = "go", value_parser = ["go", "tinygo", "rust", "zig", "c"])]
    lang: String,

    /// Go module path of the project (go, tinygo). Defaults to the
    /// directory's base name.
    #[arg(long)]
    module: Option<String>,

    /// Short name used in docs and the example Composition, and the crate
    /// name for rust (the project name for zig and c). Defaults to the
    /// module's last element or the directory's base name.
    #[arg(long)]
    name: Option<String>,

    /// function-sdk-go version to require (go).
    #[arg(long, default_value = FALLBACK_SDK_VERSION)]
    sdk_version: String,

    /// Do not run go get / go mod tidy; go writes go.mod from the given
    /// versions.
    #[arg(long)]
    offline: bool,
}

impl InitCmd {
    fn run(&self) -> Result<(), String> {
        let base = self
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (module, name) = match self.lang.as_str() {
            // No Go module path: the crate or project is named after the
            // directory.
            scaffold::LANG_RUST | scaffold::LANG_ZIG | scaffold::LANG_C => (
                String::new(),
                self.name.clone().unwrap_or_else(|| base.clone()),
            ),
            _ => (
                self.module.clone().unwrap_or_else(|| base.clone()),
                self.name.clone().unwrap_or_default(),
            ),
        };
        let files = scaffold::render(scaffold::Options {
            lang: self.lang.clone(),
            module: module.clone(),
            name,
            go_version: go_version(),
            sdk_version: self.sdk_version.clone(),
            requires: self.offline,
        })?;
        scaffold::write(&self.dir, &files)?;
        let dir = self.dir.display();
        match self.lang.as_str() {
            scaffold::LANG_RUST => println!(
                "Created {dir} (crate {})",
                self.name.clone().unwrap_or(base.clone())
            ),
            scaffold::LANG_ZIG | scaffold::LANG_C => println!(
                "Created {dir} (project {})",
                self.name.clone().unwrap_or(base.clone())
            ),
            _ => println!("Created {dir} (module {module})"),
        }

        // Only the Go-toolchain flavours carry a go.mod to resolve; rust,
        // zig and c vendor or fetch their dependencies through their own
        // build systems.
        if !self.offline && (self.lang == scaffold::LANG_GO || self.lang == scaffold::LANG_TINYGO) {
            if self.lang == scaffold::LANG_GO {
                run_in(
                    &self.dir,
                    "go",
                    &[
                        "get",
                        &format!("github.com/crossplane/function-sdk-go@{}", self.sdk_version),
                    ],
                )?;
            }
            run_in(&self.dir, "go", &["mod", "tidy"])?;
        }
        let test = match self.lang.as_str() {
            scaffold::LANG_RUST => "cargo test           # edit src/lib.rs, keep the tests passing",
            scaffold::LANG_ZIG => {
                "zig build test       # edit src/main.zig, keep the tests passing"
            }
            scaffold::LANG_C => "zig build test       # edit src/fn.c, keep the tests passing",
            _ => "go test ./...        # edit fn.go, keep the tests passing",
        };
        println!(
            "\nNext:\n  cd {dir}\n  {test}\n  guestfn build        # fn.wasm\n  guestfn push <ref>   # publish, then reference the digest from a Composition"
        );
        Ok(())
    }
}

/// Runs a toolchain command in dir, inheriting stdout and stderr.
pub(crate) fn run_in(dir: &std::path::Path, name: &str, args: &[&str]) -> Result<(), String> {
    let status = std::process::Command::new(name)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("{name} {} failed: {e}", args.join(" ")))?;
    if !status.success() {
        return Err(format!("{name} {} failed: {status}", args.join(" ")));
    }
    Ok(())
}

/// The go directive for scaffolds: the toolchain on PATH, e.g. 1.26.6.
fn go_version() -> String {
    let out = std::process::Command::new("go")
        .args(["env", "GOVERSION"])
        .output();
    if let Ok(out) = out
        && out.status.success()
    {
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(v) = v.strip_prefix("go") {
            let v = v.split([' ', '-']).next().unwrap_or(v).to_string();
            if !v.is_empty() {
                return v;
            }
        }
    }
    FALLBACK_GO_VERSION.to_string()
}

/// Compiles wasm with a throwaway engine and reports its shape, ABI verdict
/// included - the runtime's own view.
pub(crate) fn inspect_module(wasm: &[u8]) -> Result<function_wasm_engine::Inspection, String> {
    let engine = function_wasm_engine::Engine::new(function_wasm_engine::Config::default())
        .map_err(|e| e.to_string())?;
    engine.inspect(wasm).map_err(|e| e.to_string())
}

/// The runtime's own load-time check: the shape, or the refusal the runtime
/// would give at load.
pub(crate) fn check_module(wasm: &[u8]) -> Result<function_wasm_engine::Inspection, String> {
    let shape = inspect_module(wasm)?;
    if let Some(e) = &shape.abi_error {
        return Err(e.clone());
    }
    Ok(shape)
}

/// Renders a size for text output.
pub(crate) fn human_bytes(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / f64::from(1u32 << 20))
    } else if n >= 1 << 10 {
        format!("{:.1} KB", n as f64 / f64::from(1u32 << 10))
    } else {
        format!("{n} B")
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init(cmd) => cmd.run(),
        Command::Build(cmd) => cmd.run(),
        Command::Push(cmd) => cmd.run(),
        Command::Inspect(cmd) => cmd.run(),
        Command::Manifest(cmd) => cmd.run(),
        Command::Scaffold(ScaffoldCmd::Composition(cmd)) => cmd.run(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("guestfn: error: {e}");
            ExitCode::FAILURE
        }
    }
}
