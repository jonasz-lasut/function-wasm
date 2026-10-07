//! Wraps a core module into an ABI v2 component, for guestfn build.
//!
//! wit-bindgen's C generator (the c and zig scaffolds) does not emit a
//! component: it links a `component-type*` custom section into the core
//! module - the guest's world, encoded - and leaves the wrap to wasm-tools.
//! This module is that wrap, with wit-component: the world is read from the
//! section, and when the core module imports `wasi_snapshot_preview1` the
//! wasip1 reactor adapter is linked in so those imports become the WASI
//! 0.2 interfaces the runtime serves. It lives in the engine, not in
//! guestfn, because the adapter must be the one the engine's wasmtime-wasi
//! was released with: the provider crate is published at wasmtime's version
//! and pinned beside it, so a guest built by guestfn can never carry an
//! adapter the runtime does not serve.
//!
//! wit-bindgen's Go generator (the go scaffold) emits no section either:
//! mainline Go's wasip1 build knows nothing of components, and
//! componentize-go embeds the world itself before wrapping. `embed_world`
//! is that embed, over the guest's own wit/ directory, so a Go guest needs
//! neither componentize-go nor wasm-tools. wit-bindgen's MoonBit generator
//! is the same case with one difference: MoonBit strings are UTF-16 in
//! linear memory, so the embed states that encoding, or the host would
//! read every string the guest lowers (a log line, `run`'s error) as UTF-8.

use std::path::Path;

use crate::Error;

/// How a guest lays its strings out in linear memory, stated in the
/// embedded world so the canonical ABI transcodes them: what wit-bindgen's
/// generators embed for their language (`wasm-tools component embed
/// --encoding`). Every C-family toolchain and Go use UTF-8; MoonBit stores
/// UTF-16.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StringEncoding {
    #[default]
    Utf8,
    Utf16,
}

impl From<StringEncoding> for wit_component::StringEncoding {
    fn from(encoding: StringEncoding) -> Self {
        match encoding {
            StringEncoding::Utf8 => wit_component::StringEncoding::UTF8,
            StringEncoding::Utf16 => wit_component::StringEncoding::UTF16,
        }
    }
}

/// The WASI preview 1 import module a wasip1 toolchain's core module
/// imports: what the adapter translates to WASI 0.2 in the wrap.
const WASI_PREVIEW1_MODULE: &str = "wasi_snapshot_preview1";

/// The custom section wit-bindgen embeds, matched the way wit-component
/// matches it: by prefix, so `component-type` (older generators) and
/// `component-type:<world>` both count.
const COMPONENT_TYPE_SECTION: &str = "component-type";

/// What the wrap produced: the component, and whether the wasip1 adapter
/// was linked into it (only when the core module imported from
/// `wasi_snapshot_preview1`).
#[derive(Debug)]
pub struct Componentized {
    pub wasm: Vec<u8>,
    pub adapter: bool,
}

/// True when the bytes are a core module carrying a `component-type*`
/// custom section - what componentize consumes. A component is already
/// wrapped and a core module without the section is no guest at all (the
/// runtime refuses it by name); both are left to the runtime's own
/// verdict. Malformed bytes are false too: wasmtime refuses them with its
/// own words.
pub fn carries_component_type(wasm: &[u8]) -> bool {
    matches!(
        scan(wasm),
        Ok(Scan {
            component: false,
            component_type: true,
            ..
        })
    )
}

/// Embeds the world named `world` of the WIT package under `wit_dir` (the
/// guest's wit/ directory: its world and its deps) into a core module as
/// wit-bindgen's generators embed it, the `component-type` custom section
/// componentize consumes. For a core module that carries none: what
/// mainline Go's `go build` leaves behind, whose wit-bindgen bindings are
/// Go source only - or MoonBit's `moon build`, whose bindings are MoonBit
/// source only too. `encoding` is how the guest stores its strings. A
/// module that already carries a section, or is already a component, is
/// refused rather than embedded twice.
pub fn embed_world(
    core: &[u8],
    wit_dir: &Path,
    world: &str,
    encoding: StringEncoding,
) -> Result<Vec<u8>, Error> {
    let scan = scan(core)?;
    if scan.component {
        return Err(Error(
            "cannot embed the world into module: it is already a component".to_string(),
        ));
    }
    if scan.component_type {
        return Err(Error(format!(
            "cannot embed the world into module: it already carries a {COMPONENT_TYPE_SECTION} custom section"
        )));
    }
    let mut resolve = wit_parser::Resolve::default();
    let (pkg, _) = resolve.push_path(wit_dir).map_err(|e| {
        Error(format!(
            "cannot embed the world into module: cannot read {}: {e:#}",
            wit_dir.display()
        ))
    })?;
    let world = resolve
        .select_world(&[pkg], Some(world))
        .map_err(|e| Error(format!("cannot embed the world into module: {e:#}")))?;
    let mut wasm = core.to_vec();
    wit_component::embed_component_metadata(&mut wasm, &resolve, world, encoding.into())
        .map_err(|e| Error(format!("cannot embed the world into module: {e:#}")))?;
    Ok(wasm)
}

/// Wraps a core module carrying wit-bindgen's `component-type*` section
/// into a component, linking the wasip1 reactor adapter when the module
/// imports `wasi_snapshot_preview1`. The result is what wasm-tools
/// component new would write; the runtime's world typecheck (inspect,
/// compile) judges it afterwards like any other component.
pub fn componentize(core: &[u8]) -> Result<Componentized, Error> {
    let scan = scan(core)?;
    if scan.component {
        return Err(Error(
            "cannot componentize module: it is already a component".to_string(),
        ));
    }
    if !scan.component_type {
        return Err(Error(format!(
            "cannot componentize module: it carries no {COMPONENT_TYPE_SECTION} custom section (wit-bindgen embeds one; the runtime refuses a core module without it)"
        )));
    }
    let mut encoder = wit_component::ComponentEncoder::default();
    encoder
        .module(core)
        .map_err(|e| Error(format!("cannot componentize module: {e:#}")))?;
    if scan.preview1 {
        encoder
            .adapter(
                wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_ADAPTER_NAME,
                wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_REACTOR_ADAPTER,
            )
            .map_err(|e| Error(format!("cannot componentize module: {e:#}")))?;
    }
    let wasm = encoder
        .encode()
        .map_err(|e| Error(format!("cannot componentize module: {e:#}")))?;
    Ok(Componentized {
        wasm,
        adapter: scan.preview1,
    })
}

/// What one pass over the binary's sections tells: the format, whether the
/// section is there, whether anything is imported from preview1.
struct Scan {
    component: bool,
    component_type: bool,
    preview1: bool,
}

fn scan(wasm: &[u8]) -> Result<Scan, Error> {
    let mut scan = Scan {
        component: false,
        component_type: false,
        preview1: false,
    };
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload =
            payload.map_err(|e| Error(format!("cannot componentize module: {}", e.message())))?;
        match payload {
            wasmparser::Payload::Version { encoding, .. } => {
                if encoding == wasmparser::Encoding::Component {
                    // The component's own sections say nothing about a
                    // component-type section nested in its core modules.
                    scan.component = true;
                    break;
                }
            }
            wasmparser::Payload::CustomSection(cs)
                if cs.name().starts_with(COMPONENT_TYPE_SECTION) =>
            {
                scan.component_type = true;
            }
            wasmparser::Payload::ImportSection(reader) => {
                for import in reader.into_imports() {
                    let import = import.map_err(|e| {
                        Error(format!("cannot componentize module: {}", e.message()))
                    })?;
                    if import.module == WASI_PREVIEW1_MODULE {
                        scan.preview1 = true;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(scan)
}
