//! guestfn build: compile a guest project as a wasip1 reactor with the
//! toolchain of its language, check the result as the runtime would at
//! load, and validate the project's wasmfn.yaml, if any, as the manifest
//! guestfn push will publish beside it.

use std::path::{Path, PathBuf};

use function_wasm::manifest::Manifest;

use crate::scaffold;

/// TypeScript builds through npm (componentize-js) and Python through a
/// venv (componentize-py); neither has a scaffold template yet, so the
/// names live here, not in scaffold::LANGS.
pub(crate) const LANG_TS: &str = "ts";
pub(crate) const LANG_PYTHON: &str = "python";

#[derive(clap::Args, Debug)]
pub struct BuildCmd {
    /// Project directory.
    #[arg(long, default_value = ".")]
    dir: PathBuf,

    /// Output file, relative to the project directory unless absolute.
    #[arg(short, long, default_value = "fn.wasm")]
    output: PathBuf,

    /// Toolchain to use. auto picks rust for a Cargo.toml, zig for a
    /// build.zig (zig and c guests), ts for a package.json (without an
    /// asconfig.json), python for a requirements.txt, tinygo for a go.mod
    /// that requires vtprotobuf, go otherwise.
    #[arg(long, default_value = "auto", value_parser = ["auto", "go", "tinygo", "rust", "zig", "c", "ts", "python"])]
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
            "Built {} ({}, ABI v{}{}",
            out.display(),
            crate::human_bytes(wasm.len() as u64),
            shape.abi_version,
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
/// build.zig builds with zig (zig and c guests); a package.json without an
/// asconfig.json builds with npm (the TypeScript flavour; AssemblyScript
/// projects build with npm directly); a go.mod that requires vtprotobuf is
/// the TinyGo flavour; any other go.mod is Go.
fn detect_lang(dir: &Path) -> Result<String, String> {
    if dir.join("Cargo.toml").exists() {
        return Ok(scaffold::LANG_RUST.to_string());
    }
    if dir.join("build.zig").exists() {
        return Ok(scaffold::LANG_ZIG.to_string());
    }
    if dir.join("package.json").exists() && !dir.join("asconfig.json").exists() {
        return Ok(LANG_TS.to_string());
    }
    if dir.join("requirements.txt").exists() {
        return Ok(LANG_PYTHON.to_string());
    }
    let gomod = std::fs::read_to_string(dir.join("go.mod")).map_err(|_| {
        format!(
            "cannot tell the project's language: no Cargo.toml, build.zig, package.json, requirements.txt or go.mod in {} (use --lang)",
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
            // The scaffold emits an ABI v2 component (wasm32-wasip2, the
            // wit/ directory carries its world); a project without wit/ is
            // an ABI v1 wasip1 guest and keeps building as one.
            let target = if dir.join("wit").is_dir() {
                "wasm32-wasip2"
            } else {
                "wasm32-wasip1"
            };
            which(
                "cargo",
                &format!("install Rust from https://rustup.rs and run rustup target add {target}"),
            )?;
            crate::run_in(dir, "cargo", &["build", "--release", "--target", target])?;
            let release = dir.join(format!("target/{target}/release"));
            let mut matches: Vec<PathBuf> = std::fs::read_dir(&release)
                .map_err(|e| format!("cannot read {}: {e}", release.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "wasm"))
                .collect();
            if matches.len() != 1 {
                return Err(format!(
                    "expected one .wasm under target/{target}/release, found {}",
                    matches.len()
                ));
            }
            let wasm = std::fs::read(matches.remove(0)).map_err(|e| e.to_string())?;
            std::fs::write(out, wasm).map_err(|e| e.to_string())?;
        }
        LANG_TS => {
            which("npm", "install node from https://nodejs.org")?;
            if !dir.join("node_modules").is_dir() {
                crate::run_in(dir, "npm", &["ci", "--no-audit", "--no-fund"])?;
            }
            crate::run_in(dir, "npm", &["run", "build"])?;
            // The package's build script componentizes to fn.wasm in the
            // project directory (the jco invocation names it).
            let built = dir.join("fn.wasm");
            if built != out {
                let wasm = std::fs::read(&built)
                    .map_err(|e| format!("npm run build produced no fn.wasm: {e}"))?;
                std::fs::write(out, wasm).map_err(|e| e.to_string())?;
            }
        }
        LANG_PYTHON => {
            // The documented layout of a python guest (examples/hello-python):
            // the app module under src/, the generated codec under src/gen,
            // the world in wit/, componentize-py pinned in requirements.txt.
            which("python3", "install Python from https://www.python.org")?;
            if !dir.join(".venv").is_dir() {
                crate::run_in(dir, "python3", &["-m", "venv", ".venv"])?;
                crate::run_in(
                    dir,
                    ".venv/bin/pip",
                    &["install", "--quiet", "-r", "requirements.txt"],
                )?;
            }
            // VIRTUAL_ENV must be absolute (componentize-py resolves
            // site-packages from it); the program path resolves in the
            // child's working directory.
            let venv = dir
                .join(".venv")
                .canonicalize()
                .map_err(|e| format!("cannot resolve .venv: {e}"))?;
            let status = std::process::Command::new(".venv/bin/componentize-py")
                .args(["-d", "wit", "-w", "function", "componentize", "app"])
                .args(["-p", "src", "-p", "src/gen", "-o", &out_s])
                .current_dir(dir)
                .env("VIRTUAL_ENV", &venv)
                .status()
                .map_err(|e| format!("componentize-py failed: {e}"))?;
            if !status.success() {
                return Err(format!("componentize-py failed: {status}"));
            }
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
