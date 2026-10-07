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

/// The world a Go or MoonBit project's wit/ declares (the go scaffold's
/// wit/world.wit, and the moonbit example's): what is embedded into the
/// core module go build or moon build emits.
const GUEST_WORLD: &str = "function";

/// The MoonBit toolchain, detected by its module file - moon.mod (what moon
/// writes today) or moon.mod.json (what wit-bindgen 0.62 still writes, and
/// moon still reads): an example here, not a scaffold yet (#155).
const LANG_MOONBIT: &str = "moonbit";

/// moon's build directory, passed explicitly (--target-dir) because its
/// default moved from target/ to _build/ between moon releases.
const MOON_TARGET_DIR: &str = "_build";

/// Where `moon build --target wasm --release` leaves the linked module of
/// the package that carries wit-bindgen's `link` section (the generated
/// `gen` package): under the build directory, named after the package.
const MOON_OUTPUT: &str = "_build/wasm/release/build/gen/gen.wasm";

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
    /// requirements.txt, go for a go.mod, moonbit for a moon.mod or
    /// moon.mod.json.
    #[arg(long, default_value = "auto", value_parser = ["auto", "go", "rust", "zig", "c", "ts", "python", "moonbit"])]
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
            "Built {} ({}, ABI v2{}",
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

/// The build output as the runtime will see it.
#[derive(Debug, PartialEq)]
enum Output {
    /// A component, or a plain core module the runtime will refuse: the
    /// toolchain's bytes, untouched.
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
    match example_step(&path) {
        Err(e) => println!("warning: cannot read {}: {e}", path.display()),
        Ok(None) => {}
        Ok(Some(step)) => {
            // The runtime's words on every request carrying such a step.
            if step.api_version == function_wasm::input::API_VERSION_V1BETA1 {
                println!(
                    "warning: {}: {}",
                    path.display(),
                    function_wasm::input::V1BETA1_DEPRECATION
                );
            }
            if let Err(e) = m.validate_config(step.config.as_ref()) {
                println!("warning: {}: {e}", path.display());
            }
        }
    }
}

/// The first function-wasm step of a Composition file, as the runtime
/// receives it.
struct ExampleStep {
    api_version: String,
    config: Option<serde_json::Value>,
}

/// None without a function-wasm step (the current apiVersion or the
/// deprecated one, as the runtime accepts).
fn example_step(path: &Path) -> Result<Option<ExampleStep>, String> {
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let doc: serde_json::Value = serde_yaml::from_slice(&raw).map_err(|e| e.to_string())?;
    let steps = doc
        .pointer("/spec/pipeline")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let str_of = |v: &serde_json::Value, key: &str| -> String {
        v.get(key)
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string()
    };
    for step in steps {
        let input = step.get("input").cloned().unwrap_or_default();
        let api_version = str_of(&input, "apiVersion");
        if !function_wasm::input::is_input(&api_version, &str_of(&input, "kind")) {
            continue;
        }
        return Ok(Some(ExampleStep {
            api_version,
            config: input.get("config").cloned(),
        }));
    }
    Ok(None)
}

/// Tells the language of a project from its files: a Cargo.toml is Rust; a
/// build.zig builds with zig (zig and c guests); a package.json builds with
/// npm (the TypeScript flavour); a go.mod is Go; a moon.mod.json is MoonBit.
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
    if dir.join("moon.mod").exists() || dir.join("moon.mod.json").exists() {
        return Ok(LANG_MOONBIT.to_string());
    }
    Err(format!(
        "cannot tell the project's language: no Cargo.toml, build.zig, package.json, requirements.txt, go.mod or moon.mod in {} (use --lang)",
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
            // The guest runs over wit-bindgen's Go bindings, whose core
            // module carries no component type (the generator emits Go
            // source only): its world is embedded here from the project's
            // wit/ (the scaffold writes one), and the wrap below links the
            // wasip1 adapter.
            let core =
                std::fs::read(out).map_err(|e| format!("cannot read {}: {e}", out.display()))?;
            let embedded = componentize::embed_world(
                &core,
                &dir.join("wit"),
                GUEST_WORLD,
                componentize::StringEncoding::Utf8,
            )
            .map_err(|e| format!("built {}, but {e}", out.display()))?;
            std::fs::write(out, embedded).map_err(|e| e.to_string())?;
        }
        scaffold::LANG_RUST => {
            // The scaffold emits a component (wasm32-wasip3, the wit/
            // directory carries its world).
            let target = "wasm32-wasip3";
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
        LANG_MOONBIT => {
            // moon's wasm target is a core module over linear memory (wasm-gc
            // has no canonical ABI); wit-bindgen's MoonBit bindings carry no
            // component type, so the world is embedded from the project's
            // wit/ as for Go - stated UTF-16, how MoonBit stores strings.
            // Nothing is imported from wasi_snapshot_preview1, so the wrap
            // below links no adapter.
            which(
                "moon",
                "install MoonBit from https://www.moonbitlang.com/download",
            )?;
            crate::run_in(
                dir,
                "moon",
                &[
                    "build",
                    "--target",
                    "wasm",
                    "--release",
                    "--target-dir",
                    MOON_TARGET_DIR,
                ],
            )?;
            let built = dir.join(MOON_OUTPUT);
            let core = std::fs::read(&built)
                .map_err(|e| format!("moon build produced no {MOON_OUTPUT}: {e}"))?;
            let embedded = componentize::embed_world(
                &core,
                &dir.join("wit"),
                GUEST_WORLD,
                componentize::StringEncoding::Utf16,
            )
            .map_err(|e| format!("built {}, but {e}", built.display()))?;
            std::fs::write(out, embedded).map_err(|e| e.to_string())?;
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

    const CORE_MODULE_WAT: &str = r#"(module
      (memory (export "memory") 1)
      (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
      (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#;

    /// Outputs that need no wrap pass through untouched: a core module
    /// without the section (which the runtime's check then refuses) and a
    /// component (the rust, ts and python toolchains componentize
    /// themselves). The wrap of a module that carries the section is proven
    /// in the engine's tests.
    #[test]
    fn passes_through_what_needs_no_wrap() {
        for wat in [CORE_MODULE_WAT, "(component)"] {
            let wasm = wat::parse_str(wat).expect("wat");
            assert_eq!(
                componentize_if_needed(wasm.clone()),
                Ok(Output::AsBuilt(wasm)),
                "{wat}"
            );
        }
    }

    /// A section the wrap cannot read is the engine's refusal, which the
    /// build line prefixes - never a silent fall-through to the runtime's
    /// core-module refusal.
    #[test]
    fn a_failed_wrap_is_an_error() {
        let wasm = wat::parse_str(r#"(module (@custom "component-type:x" "nope"))"#).expect("wat");
        let err = componentize_if_needed(wasm).expect_err("malformed section");
        assert!(err.starts_with("cannot componentize module: "), "{err}");
    }

    /// The toolchain is told from the project's marker file, one per
    /// language: MoonBit by either spelling of its module file (moon.mod is
    /// what moon writes today, moon.mod.json what wit-bindgen 0.62 still
    /// writes), and a project with none is refused naming every marker.
    #[test]
    fn the_language_is_told_from_the_project() {
        let cases: [(&str, &str); 7] = [
            ("Cargo.toml", scaffold::LANG_RUST),
            ("build.zig", scaffold::LANG_ZIG),
            ("package.json", scaffold::LANG_TS),
            ("requirements.txt", scaffold::LANG_PYTHON),
            ("go.mod", scaffold::LANG_GO),
            ("moon.mod", LANG_MOONBIT),
            ("moon.mod.json", LANG_MOONBIT),
        ];
        for (marker, want) in cases {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join(marker), b"").expect("write");
            assert_eq!(detect_lang(dir.path()).as_deref(), Ok(want), "{marker}");
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let err = detect_lang(dir.path()).expect_err("no marker");
        assert!(
            err.starts_with("cannot tell the project's language: no Cargo.toml, build.zig, package.json, requirements.txt, go.mod or moon.mod in "),
            "{err}"
        );
    }

    /// The example-config check finds the step under the current apiVersion
    /// and under the deprecated one the runtime still serves, and reads
    /// which it was so the build line can say so.
    #[test]
    fn the_example_step_is_found_under_either_api_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("composition.yaml");
        for (api_version, want) in [
            (function_wasm::input::API_VERSION, true),
            (function_wasm::input::API_VERSION_V1BETA1, true),
            ("pt.fn.crossplane.io/v1beta1", false),
        ] {
            std::fs::write(
                &path,
                format!(
                    "apiVersion: apiextensions.crossplane.io/v1\nkind: Composition\nspec:\n  pipeline:\n  - step: s\n    input:\n      apiVersion: {api_version}\n      kind: Input\n      config: {{greeting: hi}}\n"
                ),
            )
            .expect("write");
            let step = example_step(&path).expect("read");
            assert_eq!(step.is_some(), want, "{api_version}");
            if let Some(step) = step {
                assert_eq!(step.api_version, api_version);
                assert_eq!(step.config, Some(serde_json::json!({"greeting": "hi"})));
            }
        }
    }
}
