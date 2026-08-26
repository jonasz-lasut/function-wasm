//! Renders a new guest project in one of five flavours: Go with
//! function-sdk-go (its ABI glue vendored in internal/wasmfn), TinyGo over
//! generated protobuf messages, Rust with prost, Zig with zig-protobuf, or C
//! with nanopb (built by zig cc). Each template set is the matching example
//! guest of this repository (examples/hello-go, hello-tinygo, hello-rust,
//! hello-zig, hello-c) with the module path and name parameterised; tests
//! keep them identical.

use std::collections::BTreeMap;
use std::path::Path;

use include_dir::{Dir, include_dir};

static TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

pub const LANG_GO: &str = "go";
pub const LANG_TINYGO: &str = "tinygo";
pub const LANG_RUST: &str = "rust";
pub const LANG_ZIG: &str = "zig";
pub const LANG_C: &str = "c";

/// The scaffoldable languages, default first.
pub const LANGS: [&str; 5] = [LANG_GO, LANG_TINYGO, LANG_RUST, LANG_ZIG, LANG_C];

/// Options parameterise a scaffold.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// The template set; empty means go.
    pub lang: String,
    /// The Go module path of the guest, e.g. github.com/me/my-fn. Required
    /// for Go and TinyGo; Rust, Zig and C projects have no module path.
    pub module: String,
    /// The guest's short name: the crate name for Rust, and what docs and
    /// the example Composition call the guest. Empty derives it from the
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
    if o.module.is_empty() && (o.lang == LANG_GO || o.lang == LANG_TINYGO) {
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
/// sets use: the four fields, one conditional block, and the two zig
/// helpers.
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
        ("[[ zigid .Name ]]", zig_ident(&o.name)),
        ("[[ zigfp .Name ]]", zig_fingerprint(&o.name)),
    ] {
        out = out.replace(token, &value);
    }
    if out.contains("[[") {
        let at = out.find("[[").expect("just found");
        return Err(format!(
            "template {name} uses an unsupported expression near {:?}",
            &out[at..out.len().min(at + 40)]
        ));
    }
    Ok(out)
}

/// Turns a project name into a valid Zig identifier (build.zig.zon's .name
/// is an enum literal, so kebab-case dashes and other non-identifier bytes
/// become underscores).
fn zig_ident(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The build.zig.zon fingerprint, whose high 32 bits Zig requires to be the
/// CRC-32 of the identifier (the low 32 are a package id, fixed here since a
/// wasm guest is never published as a Zig package).
fn zig_fingerprint(s: &str) -> String {
    let crc = crc32fast::hash(zig_ident(s).as_bytes());
    format!("0x{:016x}", (u64::from(crc)) << 32 | 0x8f36eb28)
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

    /// Each language's example guest is its scaffold rendered for itself.
    /// Only go.mod may differ (the Go examples replace the SDK with the
    /// checkout and carry tidy's indirect requirements); the examples'
    /// Makefile, Cargo.lock and generated build artefacts are extra.
    #[test]
    fn render_matches_the_examples() {
        let examples: [(&str, Options); 5] = [
            (
                LANG_GO,
                Options {
                    lang: LANG_GO.into(),
                    module: "github.com/jonasz-lasut/function-wasm/examples/hello-go".into(),
                    go_version: "1.26.6".into(),
                    ..Default::default()
                },
            ),
            (
                LANG_TINYGO,
                Options {
                    lang: LANG_TINYGO.into(),
                    module: "github.com/jonasz-lasut/function-wasm/examples/hello-tinygo".into(),
                    go_version: "1.26.6".into(),
                    ..Default::default()
                },
            ),
            (
                LANG_RUST,
                Options {
                    lang: LANG_RUST.into(),
                    name: "hello-rust".into(),
                    ..Default::default()
                },
            ),
            (
                LANG_ZIG,
                Options {
                    lang: LANG_ZIG.into(),
                    name: "hello-zig".into(),
                    ..Default::default()
                },
            ),
            (
                LANG_C,
                Options {
                    lang: LANG_C.into(),
                    name: "hello-c".into(),
                    ..Default::default()
                },
            ),
        ];
        for (lang, o) in examples {
            let files = render(o).expect("render");
            let example = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples")
                .join(format!("hello-{lang}"));
            for (name, rendered) in &files {
                if name == "go.mod" {
                    continue;
                }
                let want = std::fs::read(example.join(name)).unwrap_or_else(|e| {
                    panic!(
                        "examples/hello-{lang}/{name}: {e} (the scaffold renders it; copy it there)"
                    )
                });
                assert_eq!(
                    String::from_utf8_lossy(&want),
                    String::from_utf8_lossy(rendered),
                    "examples/hello-{lang}/{name} differs from the scaffold template"
                );
            }
        }
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
            "unsupported language \"cobol\"; one of go, tinygo, rust, zig, c"
        );

        // The C flavour builds with zig, so build.zig.zon gets the project's
        // identifier and fingerprint.
        let files = render(Options {
            lang: LANG_C.into(),
            name: "my-fn".into(),
            ..Default::default()
        })
        .expect("render");
        let zon = String::from_utf8_lossy(&files["build.zig.zon"]).into_owned();
        assert!(zon.contains(".name = .my_fn,"), "{zon}");
        assert!(zon.contains(".fingerprint = 0x"), "{zon}");

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

    /// The four polyglot template sets vendor the same run_function.proto:
    /// one canonical wire contract, four copies kept in lockstep (the
    /// render-matches-the-examples test extends the lockstep to the
    /// examples and their generated codecs).
    #[test]
    fn vendored_protos_are_identical() {
        // The first two lines are the per-language vendoring header; the
        // wire contract below them must be one and the same.
        let read = |lang: &str| {
            let raw = TEMPLATES
                .get_file(format!("{lang}/proto/run_function.proto"))
                .unwrap_or_else(|| panic!("{lang} template vendors no proto"))
                .contents();
            String::from_utf8_lossy(raw)
                .lines()
                .skip(2)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let reference = read(LANG_TINYGO);
        for lang in [LANG_RUST, LANG_ZIG, LANG_C] {
            assert_eq!(
                reference,
                read(lang),
                "{lang}'s vendored proto differs from tinygo's"
            );
        }
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
