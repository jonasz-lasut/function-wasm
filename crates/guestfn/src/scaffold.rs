//! Renders a new guest project in one of four flavours, the languages with
//! an official function-sdk under the crossplane organisation (#264): Go
//! with function-sdk-go (its ABI glue vendored in internal/wasmfn over
//! wit-bindgen's Go bindings, the core module componentized by guestfn
//! build), Rust with prost, TypeScript with protobuf-es (componentized by
//! jco), or Python with protobuf (componentized by componentize-py). Each
//! template set is a minimal greeting project; the example guests of this
//! repository solve their own use cases on the same plumbing (the vendored
//! glue, codecs, proto and WIT), which tests keep identical. Zig, C and
//! the other languages under examples/ are examples alone: guestfn build
//! still builds them, guestfn init does not scaffold them.

use std::collections::BTreeMap;
use std::path::Path;

use include_dir::{Dir, include_dir};

static TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

pub const LANG_GO: &str = "go";
pub const LANG_RUST: &str = "rust";
pub const LANG_TS: &str = "ts";
pub const LANG_PYTHON: &str = "python";

/// The scaffoldable languages, default first.
pub const LANGS: [&str; 4] = [LANG_GO, LANG_RUST, LANG_TS, LANG_PYTHON];

/// Options parameterise a scaffold.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// The template set; empty means go.
    pub lang: String,
    /// The Go module path of the guest, e.g. github.com/me/my-fn. Required
    /// for Go; Rust, TypeScript and Python projects have no module path.
    pub module: String,
    /// The guest's short name: the crate name for Rust, the package name
    /// for TypeScript, and what docs and the example Composition call the
    /// guest. Empty derives it from the
    /// last element of module.
    pub name: String,
    /// The go directive of the generated go.mod, e.g. 1.26.6.
    pub go_version: String,
    /// The function-sdk-go version to require.
    pub sdk_version: String,
    /// Whether go.mod carries the require block at all; when false the
    /// caller is expected to run go get, which is what guestfn init does
    /// unless asked to stay offline.
    pub requires: bool,
}

/// Renders the files of the scaffold keyed by their path relative to the
/// project root.
pub fn render(mut o: Options) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if o.lang.is_empty() {
        o.lang = LANG_GO.to_string();
    }
    if !LANGS.contains(&o.lang.as_str()) {
        return Err(format!(
            "unsupported language {:?}; one of {}",
            o.lang,
            LANGS.join(", ")
        ));
    }
    if o.module.is_empty() && o.lang == LANG_GO {
        return Err("a module path is required".to_string());
    }
    if o.name.is_empty() {
        if o.module.is_empty() {
            return Err("a name is required".to_string());
        }
        o.name = o.module.rsplit('/').next().unwrap_or(&o.module).to_string();
    }
    let root = TEMPLATES
        .get_dir(&o.lang)
        .ok_or_else(|| format!("no template set for {:?}", o.lang))?;
    let mut files = BTreeMap::new();
    collect(root, &o.lang, &o, &mut files)?;
    Ok(files)
}

fn collect(
    dir: &Dir<'_>,
    root: &str,
    o: &Options,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(d) => collect(d, root, o, files)?,
            include_dir::DirEntry::File(f) => {
                let rel = f
                    .path()
                    .strip_prefix(root)
                    .expect("under the language root")
                    .to_string_lossy()
                    .into_owned();
                if let Some(bare) = rel.strip_suffix(".tmpl") {
                    let text = std::str::from_utf8(f.contents())
                        .map_err(|_| format!("template {rel} is not UTF-8"))?;
                    files.insert(bare.to_string(), template(&rel, text, o)?.into_bytes());
                } else {
                    files.insert(rel, f.contents().to_vec());
                }
            }
        }
    }
    Ok(())
}

/// Executes a template with [[ ]] delimiters, so source code in the
/// templates keeps its braces. The surface is exactly what the template
/// sets use: the four fields and one conditional block.
fn template(name: &str, text: &str, o: &Options) -> Result<String, String> {
    let mut out = text.to_string();
    // The one conditional: go.mod's require block for offline scaffolds.
    while let Some(start) = out.find("[[ if .Requires ]]") {
        let after = start + "[[ if .Requires ]]".len();
        let Some(end) = out[after..].find("[[ end ]]") else {
            return Err(format!("template {name}: [[ if ]] without [[ end ]]"));
        };
        let body = out[after..after + end].to_string();
        let replacement = if o.requires { body } else { String::new() };
        out.replace_range(start..after + end + "[[ end ]]".len(), &replacement);
    }
    for (token, value) in [
        ("[[ .Module ]]", o.module.clone()),
        ("[[ .Name ]]", o.name.clone()),
        ("[[ .GoVersion ]]", o.go_version.clone()),
        ("[[ .SDKVersion ]]", o.sdk_version.clone()),
    ] {
        out = out.replace(token, &value);
    }
    // A delimiter is `[[ ` with its space: a bare `[[` is source (Go's
    // generic `Result[[]byte, string]` in the go template's bindings).
    if out.contains("[[ ") {
        let at = out.find("[[ ").expect("just found");
        return Err(format!(
            "template {name} uses an unsupported expression near {:?}",
            &out[at..out.len().min(at + 40)]
        ));
    }
    Ok(out)
}

/// Writes rendered files under dir, creating it. It refuses to overwrite
/// existing files so a typo cannot clobber a project.
pub fn write(dir: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    for rel in files.keys() {
        let full = dir.join(rel);
        if full.exists() {
            return Err(format!("{} already exists", full.display()));
        }
    }
    for (rel, content) in files {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&full, content)
            .map_err(|e| format!("cannot write {}: {e}", full.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The options the golden scaffolds are rendered with, per language.
    fn golden(lang: &str) -> Options {
        Options {
            lang: lang.to_string(),
            module: "github.com/example/my-fn".to_string(),
            name: "my-fn".to_string(),
            go_version: "1.26.6".to_string(),
            sdk_version: "v0.7.1".to_string(),
            requires: true,
        }
    }

    fn read_tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).expect("read_dir") {
                let entry = entry.expect("entry");
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, files);
                } else {
                    let rel = path
                        .strip_prefix(root)
                        .expect("under root")
                        .to_string_lossy()
                        .into_owned();
                    files.insert(rel, std::fs::read(&path).expect("read"));
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(dir, dir, &mut files);
        files
    }

    /// The golden scaffolds under testdata/<lang>; UPDATE_GOLDENS=1
    /// regenerates them.
    #[test]
    fn render_matches_the_goldens() {
        for lang in LANGS {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata")
                .join(lang);
            let files = render(golden(lang)).expect("render");
            if std::env::var_os("UPDATE_GOLDENS").is_some() {
                let _ = std::fs::remove_dir_all(&dir);
                write(&dir, &files).expect("write");
            }
            let want = read_tree(&dir);
            assert_eq!(
                want.keys().collect::<Vec<_>>(),
                files.keys().collect::<Vec<_>>(),
                "{lang}: scaffold files differ (run UPDATE_GOLDENS=1 cargo test to refresh)"
            );
            for (name, content) in &want {
                assert_eq!(
                    String::from_utf8_lossy(content),
                    String::from_utf8_lossy(&files[name]),
                    "{lang}/{name} differs (run UPDATE_GOLDENS=1 cargo test to refresh)"
                );
            }
        }
    }

    /// The examples solve their own use cases, but each one that pairs with
    /// a scaffold builds on that scaffold's plumbing - the vendored ABI glue,
    /// the generated codecs, the proto and the WIT - and must carry it byte
    /// for byte, so a plumbing fix lands in the template and the example
    /// together. Everything else (the function itself, guestfn.yaml, the
    /// README, the example manifests) is the example's own. Four pairs, one
    /// per scaffold: go, rust, ts and python. The other examples (zig, c,
    /// cpp, odin, moonbit, csharp) have no scaffold and own their plumbing
    /// (the zig and c templates retired in #264): their proto copies are
    /// held by the root Makefile's glob and Renovate's header rule under
    /// their own headers, their generated codecs and bindings by their
    /// build jobs' drift checks.
    #[test]
    fn examples_share_the_scaffold_plumbing() {
        let examples: [(&str, &[&str], Options); 4] = [
            (
                "function-wasm-go",
                &[
                    "internal/wasmfn/",
                    "internal/bindings/",
                    "wit/",
                    "guestfn.lock",
                    ".golangci.yml",
                ],
                Options {
                    lang: LANG_GO.into(),
                    module: "github.com/jonasz-lasut/guestfn/examples/function-wasm-go".into(),
                    go_version: "1.26.6".into(),
                    ..Default::default()
                },
            ),
            (
                "function-wasm-rust",
                &["proto/", "guestfn.lock", "build.rs", "rust-toolchain.toml"],
                Options {
                    lang: LANG_RUST.into(),
                    name: "function-wasm-rust".into(),
                    ..Default::default()
                },
            ),
            (
                "function-wasm-ts",
                &["proto/", "src/gen/", "src/log.d.ts", "guestfn.lock"],
                Options {
                    lang: LANG_TS.into(),
                    name: "function-wasm-ts".into(),
                    ..Default::default()
                },
            ),
            (
                "function-wasm-python",
                &["proto/", "src/gen/", "guestfn.lock"],
                Options {
                    lang: LANG_PYTHON.into(),
                    name: "function-wasm-python".into(),
                    ..Default::default()
                },
            ),
        ];
        for (example, plumbing, o) in examples {
            let files = render(o).expect("render");
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples")
                .join(example);
            let shared: Vec<_> = files
                .iter()
                .filter(|(name, _)| plumbing.iter().any(|p| name.starts_with(p)))
                .collect();
            assert!(!shared.is_empty(), "{example}: no plumbing rendered");
            for (name, rendered) in shared {
                let want = std::fs::read(dir.join(name)).unwrap_or_else(|e| {
                    panic!(
                        "examples/{example}/{name}: {e} (the scaffold renders it; copy it there)"
                    )
                });
                assert_eq!(
                    String::from_utf8_lossy(&want),
                    String::from_utf8_lossy(rendered),
                    "examples/{example}/{name} differs from the scaffold template"
                );
            }
        }
    }

    /// Every guest pins the package the engine compiles
    /// (wit/guestfn-function.wit), at its version and by one digest: the
    /// four scaffolds through what they render, and the examples with no
    /// scaffold read here.
    #[test]
    fn render_matches_the_runtime_world() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let runtime = std::fs::read_to_string(repo.join("wit/guestfn-function.wit"))
            .expect("read the runtime's world");
        let version = runtime
            .lines()
            .find_map(|line| {
                line.strip_prefix("package guestfn:function@")?
                    .strip_suffix(';')
            })
            .expect("the runtime's world declares its version");
        let pin_of = |origin: &str, lock: &str| -> String {
            let (_, entry) = lock
                .split_once("- name: guestfn:function\n")
                .unwrap_or_else(|| panic!("{origin} does not pin guestfn:function"));
            let mut fields = entry.lines().map(str::trim);
            assert_eq!(
                fields.next(),
                Some(format!("version: {version}").as_str()),
                "{origin} pins another version of the runtime's world (guestfn wit update moves it)"
            );
            fields
                .find_map(|field| field.strip_prefix("digest: "))
                .unwrap_or_else(|| panic!("{origin} pins no digest"))
                .to_string()
        };
        let mut digests = std::collections::BTreeSet::new();
        for lang in [LANG_GO, LANG_PYTHON, LANG_RUST, LANG_TS] {
            let files = render(golden(lang)).expect("render");
            let lock = String::from_utf8_lossy(&files["guestfn.lock"]).into_owned();
            digests.insert(pin_of(lang, &lock));
        }
        for example in ["c", "cpp", "csharp", "moonbit", "odin", "zig"] {
            let origin = format!("examples/function-wasm-{example}/guestfn.lock");
            let lock = std::fs::read_to_string(repo.join(&origin))
                .unwrap_or_else(|e| panic!("{origin}: {e}"));
            digests.insert(pin_of(&origin, &lock));
        }
        assert_eq!(
            digests.len(),
            1,
            "the guests pin different artifacts: {digests:?}"
        );
    }

    #[test]
    fn render_options() {
        let err = render(Options::default()).expect_err("no module");
        assert_eq!(err, "a module path is required");
        let err = render(Options {
            lang: LANG_RUST.into(),
            ..Default::default()
        })
        .expect_err("rust needs a name");
        assert_eq!(err, "a name is required");
        let err = render(Options {
            lang: "cobol".into(),
            name: "x".into(),
            ..Default::default()
        })
        .expect_err("unknown language");
        assert_eq!(
            err,
            "unsupported language \"cobol\"; one of go, rust, ts, python"
        );

        // The name defaults to the module's last element.
        let files = render(Options {
            module: "github.com/me/greeter".into(),
            ..Default::default()
        })
        .expect("render");
        assert!(String::from_utf8_lossy(&files["README.md"]).contains("# greeter"));

        // Offline scaffolds carry the require block.
        let files = render(Options {
            module: "github.com/me/greeter".into(),
            go_version: "1.26.6".into(),
            sdk_version: "v0.7.1".into(),
            requires: true,
            ..Default::default()
        })
        .expect("render");
        let gomod = String::from_utf8_lossy(&files["go.mod"]).into_owned();
        assert!(gomod.contains("go 1.26.6"), "{gomod}");
        assert!(
            gomod.contains("require github.com/crossplane/function-sdk-go v0.7.1"),
            "{gomod}"
        );

        // The Go scaffold vendors its glue and imports it by module path.
        let files = render(Options {
            module: "github.com/me/greeter".into(),
            go_version: "1.26.6".into(),
            ..Default::default()
        })
        .expect("render");
        assert!(
            String::from_utf8_lossy(&files["main.go"])
                .contains("import \"github.com/me/greeter/internal/wasmfn\"")
        );
    }

    #[test]
    fn write_refuses_overwrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("fn.go"), b"mine").expect("write");
        let mut files = BTreeMap::new();
        files.insert("fn.go".to_string(), b"theirs".to_vec());
        files.insert("main.go".to_string(), b"x".to_vec());
        write(dir.path(), &files).expect_err("must refuse");
        assert_eq!(
            std::fs::read(dir.path().join("fn.go")).expect("read"),
            b"mine"
        );
        assert!(!dir.path().join("main.go").exists());
    }
}
