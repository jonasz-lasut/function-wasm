//! guestfn build: compile a guest project as a wasip1 reactor with the
//! toolchain of its language, check the result as the runtime would at
//! load, and validate the project's wasmfn.yaml, if any, as the manifest
//! guestfn push will publish beside it.

use std::path::{Path, PathBuf};

use function_wasm::manifest::Manifest;

use crate::scaffold;

#[derive(clap::Args, Debug)]
pub struct BuildCmd {
    /// Project directory.
    #[arg(long, default_value = ".")]
    dir: PathBuf,

    /// Output file, relative to the project directory unless absolute.
    #[arg(short, long, default_value = "fn.wasm")]
    output: PathBuf,

    /// Toolchain to use. auto picks rust for a Cargo.toml, zig for a
    /// build.zig (zig and c guests), tinygo for a go.mod that requires
    /// vtprotobuf, go otherwise.
    #[arg(long, default_value = "auto", value_parser = ["auto", "go", "tinygo", "rust", "zig", "c"])]
    lang: String,

    /// Run wasm-opt -Oz on the result (binaryen must be on PATH).
    #[arg(long)]
    wasm_opt: bool,
}

impl BuildCmd {
    pub fn run(&self) -> Result<(), String> {
        let out = if self.output.is_absolute() {
            self.output.clone()
        } else {
            self.dir.join(&self.output)
        };
        let lang = if self.lang.is_empty() || self.lang == "auto" {
            detect_lang(&self.dir)?
        } else {
            self.lang.clone()
        };
        build_guest(&lang, &self.dir, &out)?;
        if self.wasm_opt {
            let out_s = out.to_string_lossy().into_owned();
            crate::run_in(&self.dir, "wasm-opt", &["-Oz", "-o", &out_s, &out_s])?;
        }
        let wasm =
            std::fs::read(&out).map_err(|e| format!("cannot read {}: {e}", out.display()))?;
        // The manifest is checked with the build so a broken wasmfn.yaml
        // fails here rather than at push time.
        let manifest_path = self.dir.join(function_wasm::manifest::FILE_NAME);
        let m = if manifest_path.is_file() {
            Some(Manifest::load(&manifest_path)?)
        } else {
            None
        };
        // The runtime's own load-time check, so a module it would refuse is
        // refused here, with the same words, before it is pushed anywhere.
        let shape = crate::check_module(&wasm).map_err(|e| {
            format!(
                "built {}, but the runtime would refuse it: {e}",
                out.display()
            )
        })?;
        let mut line = format!(
            "Built {} ({}, ABI v1{}",
            out.display(),
            crate::human_bytes(wasm.len() as u64),
            imports_suffix(&shape)
        );
        if let Some(m) = &m {
            let summary = m.summary();
            if !summary.is_empty() {
                line += &format!("; manifest: {summary}");
            }
        }
        println!("{line})");
        if let Some(m) = &m {
            warn_example_config(&self.dir, m);
        }
        Ok(())
    }
}

/// Holds the scaffold's example Composition config against the manifest's
/// schema, when both exist: a mismatch is a warning, not a failed build -
/// the example is documentation, the schema is the contract.
fn warn_example_config(dir: &Path, m: &Manifest) {
    let path = dir.join("example/composition.yaml");
    if !path.is_file() || m.config.as_ref().is_none_or(|c| c.schema.is_none()) {
        return;
    }
    match example_config(&path) {
        Err(e) => println!("warning: cannot read {}: {e}", path.display()),
        Ok(None) => {}
        Ok(Some(config)) => {
            if let Err(e) = m.validate_config(config.as_ref()) {
                println!("warning: {}: {e}", path.display());
            }
        }
    }
}

/// The config block of the first function-wasm step of a Composition file,
/// as the runtime receives it; None without such a step.
#[allow(clippy::type_complexity)]
fn example_config(path: &Path) -> Result<Option<Option<serde_json::Value>>, String> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let doc: serde_json::Value = serde_yaml::from_slice(&raw).map_err(|e| e.to_string())?;
    let steps = doc
        .pointer("/spec/pipeline")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    for step in steps {
        let input = step.get("input").cloned().unwrap_or_default();
        if input.get("apiVersion").and_then(|v| v.as_str()) != Some(crate::INPUT_API_VERSION)
            || input.get("kind").and_then(|v| v.as_str()) != Some(crate::INPUT_KIND)
        {
            continue;
        }
        return Ok(Some(input.get("config").cloned()));
    }
    Ok(None)
}

/// Tells the language of a project from its files: a Cargo.toml is Rust; a
/// build.zig builds with zig (zig and c guests); a go.mod that requires
/// vtprotobuf is the TinyGo flavour; any other go.mod is Go.
fn detect_lang(dir: &Path) -> Result<String, String> {
    if dir.join("Cargo.toml").exists() {
        return Ok(scaffold::LANG_RUST.to_string());
    }
    if dir.join("build.zig").exists() {
        return Ok(scaffold::LANG_ZIG.to_string());
    }
    let gomod = std::fs::read_to_string(dir.join("go.mod")).map_err(|_| {
        format!(
            "cannot tell the project's language: no Cargo.toml, build.zig or go.mod in {} (use --lang)",
            dir.display()
        )
    })?;
    if gomod.contains("github.com/planetscale/vtprotobuf") {
        return Ok(scaffold::LANG_TINYGO.to_string());
    }
    Ok(scaffold::LANG_GO.to_string())
}

/// Runs the language's compiler and leaves the module at out.
fn build_guest(lang: &str, dir: &Path, out: &Path) -> Result<(), String> {
    let out_s = out.to_string_lossy().into_owned();
    match lang {
        scaffold::LANG_GO => {
            let status = std::process::Command::new("go")
                .args([
                    "build",
                    "-buildmode=c-shared",
                    "-trimpath",
                    "-ldflags=-s -w",
                    "-o",
                    &out_s,
                    ".",
                ])
                .current_dir(dir)
                .env("GOOS", "wasip1")
                .env("GOARCH", "wasm")
                .status()
                .map_err(|e| format!("go build failed: {e}"))?;
            if !status.success() {
                return Err(format!("go build failed: {status}"));
            }
        }
        scaffold::LANG_TINYGO => {
            which(
                "tinygo",
                "install it from https://tinygo.org/getting-started/install/",
            )?;
            crate::run_in(
                dir,
                "tinygo",
                &[
                    "build",
                    "-target=wasip1",
                    "-buildmode=c-shared",
                    "-no-debug",
                    "-o",
                    &out_s,
                    ".",
                ],
            )?;
        }
        scaffold::LANG_RUST => {
            which(
                "cargo",
                "install Rust from https://rustup.rs and run rustup target add wasm32-wasip1",
            )?;
            crate::run_in(
                dir,
                "cargo",
                &["build", "--release", "--target", "wasm32-wasip1"],
            )?;
            let release = dir.join("target/wasm32-wasip1/release");
            let mut matches: Vec<PathBuf> = std::fs::read_dir(&release)
                .map_err(|e| format!("cannot read {}: {e}", release.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "wasm"))
                .collect();
            if matches.len() != 1 {
                return Err(format!(
                    "expected one .wasm under target/wasm32-wasip1/release, found {}",
                    matches.len()
                ));
            }
            let wasm = std::fs::read(matches.remove(0)).map_err(|e| e.to_string())?;
            std::fs::write(out, wasm).map_err(|e| e.to_string())?;
        }
        scaffold::LANG_ZIG | scaffold::LANG_C => {
            which(
                "zig",
                "install it from https://ziglang.org/download/ (it builds both the zig and c guests)",
            )?;
            crate::run_in(dir, "zig", &["build", "-Doptimize=ReleaseSmall"])?;
            let built = dir.join("zig-out/bin/fn.wasm");
            let wasm = std::fs::read(&built)
                .map_err(|e| format!("zig build produced no zig-out/bin/fn.wasm: {e}"))?;
            std::fs::write(out, wasm).map_err(|e| e.to_string())?;
        }
        _ => return Err(format!("unsupported language {lang:?}")),
    }
    Ok(())
}

fn which(name: &str, hint: &str) -> Result<(), String> {
    let found = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(name).is_file()));
    if !found {
        return Err(format!("{name} not found on PATH: {hint}"));
    }
    Ok(())
}

/// Names the host imports a module uses, for the build line.
pub(crate) fn imports_suffix(shape: &function_wasm_engine::Inspection) -> String {
    if shape.host_imports.is_empty() {
        return String::new();
    }
    format!(", imports {}", shape.host_imports.join(" "))
}
