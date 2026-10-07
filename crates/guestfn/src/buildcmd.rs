//! guestfn build: compile a guest project with the toolchain of its
//! language, wrap a core module that carries wit-bindgen's component-type
//! section into a component (embedding the world first for a Go project,
//! whose toolchain emits none), check the result as the runtime would at
//! load, and validate the project's wasmfn.yaml, if any, as the manifest
//! guestfn push will publish beside it.

use std::path::{Path, PathBuf};

use function_wasm::manifest::Manifest;
use function_wasm_engine::componentize;

use crate::scaffold;

/// The world a Go project's wit/ declares (the go scaffold's
/// wit/world.wit): what is embedded into the core module go build emits.
const GO_WORLD: &str = "function";

#[derive(clap::Args, Debug)]
pub struct BuildCmd {
    /// Project directory.
    #[arg(long, default_value = ".")]
    dir: PathBuf,

    /// Output file, relative to the project directory unless absolute.
    #[arg(short, long, default_value = "fn.wasm")]
    output: PathBuf,

    /// Toolchain to use. auto picks rust for a Cargo.toml, zig for a
    /// build.zig (zig and c guests), ts for a package.json, python for a
    /// requirements.txt, go for a go.mod.
    #[arg(long, default_value = "auto", value_parser = ["auto", "go", "rust", "zig", "c", "ts", "python"])]
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
        let wasm = match componentize_if_needed(wasm)
            .map_err(|e| format!("built {}, but {e}", out.display()))?
        {
            Output::AsBuilt(wasm) => wasm,
            Output::Componentized { wasm, adapter } => {
                std::fs::write(&out, &wasm)
                    .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
                let how = if adapter {
                    "wasip1 adapter linked"
                } else {
                    "no wasip1 imports, no adapter"
                };
                println!("Componentized {} ({how})", out.display());
                wasm
            }
        };
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
        // The runtime's words on every load of such a module.
        if shape.abi_version == 1 {
            println!("warning: {}", function_wasm_engine::ABI_V1_DEPRECATION);
        }
        if let Some(m) = &m {
            warn_example_config(&self.dir, m);
        }
        Ok(())
    }
}

/// The build output as the runtime will see it.
#[derive(Debug, PartialEq)]
enum Output {
    /// A component or a plain core module (an ABI v1 guest): the toolchain's
    /// bytes, untouched.
    AsBuilt(Vec<u8>),
    /// The toolchain left a core module carrying wit-bindgen's
    /// component-type section (what the c and zig flavours link in, and what
    /// the go build embeds from the project's wit/); the engine wrapped it,
    /// with the wasip1 adapter when the module imported
    /// wasi_snapshot_preview1.
    Componentized { wasm: Vec<u8>, adapter: bool },
}

/// Wraps a core module that carries wit-bindgen's component-type section
/// into a component; anything else passes through. The wrap is the engine's,
/// so the adapter linked in is the one the runtime's wasmtime-wasi serves -
/// a guest needs no wasm-tools and no adapter download.
fn componentize_if_needed(wasm: Vec<u8>) -> Result<Output, String> {
    if !componentize::carries_component_type(&wasm) {
        return Ok(Output::AsBuilt(wasm));
    }
    let c = componentize::componentize(&wasm).map_err(|e| e.to_string())?;
    Ok(Output::Componentized {
        wasm: c.wasm,
        adapter: c.adapter,
    })
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
/// build.zig builds with zig (zig and c guests); a package.json builds with
/// npm (the TypeScript flavour); a go.mod is Go.
fn detect_lang(dir: &Path) -> Result<String, String> {
    if dir.join("Cargo.toml").exists() {
        return Ok(scaffold::LANG_RUST.to_string());
    }
    if dir.join("build.zig").exists() {
        return Ok(scaffold::LANG_ZIG.to_string());
    }
    if dir.join("package.json").exists() {
        return Ok(scaffold::LANG_TS.to_string());
    }
    if dir.join("requirements.txt").exists() {
        return Ok(scaffold::LANG_PYTHON.to_string());
    }
    if dir.join("go.mod").exists() {
        return Ok(scaffold::LANG_GO.to_string());
    }
    Err(format!(
        "cannot tell the project's language: no Cargo.toml, build.zig, package.json, requirements.txt or go.mod in {} (use --lang)",
        dir.display()
    ))
}

/// Runs the language's compiler and leaves the module at out.
fn build_guest(lang: &str, dir: &Path, out: &Path) -> Result<(), String> {
    let out_s = out.to_string_lossy().into_owned();
    match lang {
        scaffold::LANG_GO => {
            // go build leaves an output whose build ID matches alone, and
            // it reads that ID from the module's go.buildid section, which
            // the component wrapped around the module below still carries:
            // the previous build goes first, or the embed would find a
            // component where it expects the core module.
            if out.exists() {
                std::fs::remove_file(out)
                    .map_err(|e| format!("cannot remove {}: {e}", out.display()))?;
            }
            // Mainline Go has no wasip2 port: the guest is a wasip1 reactor.
            // -checklinkname=0 admits the bindings runtime's linkname to
            // runtime.sbrk (go.bytecodealliance.org/pkg, what
            // componentize-go passes too); the linker refuses it otherwise.
            let status = std::process::Command::new("go")
                .args([
                    "build",
                    "-buildmode=c-shared",
                    "-trimpath",
                    "-ldflags=-s -w -checklinkname=0",
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
            // The scaffold is an ABI v2 guest over wit-bindgen's Go
            // bindings, whose core module carries no component type (the
            // generator emits Go source only): its world is embedded here
            // from the project's wit/, and the wrap below links the wasip1
            // adapter. A project without wit/ is an ABI v1 wasip1 guest and
            // keeps building as one.
            let wit = dir.join("wit");
            if wit.is_dir() {
                let core = std::fs::read(out)
                    .map_err(|e| format!("cannot read {}: {e}", out.display()))?;
                let embedded = componentize::embed_world(&core, &wit, GO_WORLD)
                    .map_err(|e| format!("built {}, but {e}", out.display()))?;
                std::fs::write(out, embedded).map_err(|e| e.to_string())?;
            }
        }
        scaffold::LANG_RUST => {
            // The scaffold emits an ABI v2 component (wasm32-wasip3, the
            // wit/ directory carries its world); a project without wit/ is
            // an ABI v1 wasip1 guest and keeps building as one.
            let target = if dir.join("wit").is_dir() {
                "wasm32-wasip3"
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
        scaffold::LANG_TS => {
            which("npm", "install node from https://nodejs.org")?;
            if !dir.join("node_modules").is_dir() {
                // A lockfile pins the install; a fresh project has none, so
                // npm install resolves the package.json ranges and writes it.
                let install = if dir.join("package-lock.json").is_file() {
                    "ci"
                } else {
                    "install"
                };
                crate::run_in(dir, "npm", &[install, "--no-audit", "--no-fund"])?;
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
        scaffold::LANG_PYTHON => {
            // The layout the python scaffold writes (examples/team-tags):
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

#[cfg(test)]
mod tests {
    use super::*;

    const ABI_V1_WAT: &str = r#"(module
      (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#;

    /// Today's outputs pass through untouched: a core module without the
    /// section is an ABI v1 guest, a component is already wrapped (the
    /// rust, ts and python toolchains componentize themselves). The wrap of
    /// a module that carries the section is proven in the engine's tests.
    #[test]
    fn passes_through_what_needs_no_wrap() {
        for wat in [ABI_V1_WAT, "(component)"] {
            let wasm = wat::parse_str(wat).expect("wat");
            assert_eq!(
                componentize_if_needed(wasm.clone()),
                Ok(Output::AsBuilt(wasm)),
                "{wat}"
            );
        }
    }

    /// A section the wrap cannot read is the engine's refusal, which the
    /// build line prefixes - never a silent fall-through to the ABI v1
    /// verdict.
    #[test]
    fn a_failed_wrap_is_an_error() {
        let wasm = wat::parse_str(r#"(module (@custom "component-type:x" "nope"))"#).expect("wat");
        let err = componentize_if_needed(wasm).expect_err("malformed section");
        assert!(err.starts_with("cannot componentize module: "), "{err}");
    }
}
