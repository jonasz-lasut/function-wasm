//! The runtime's log filter: `--debug` means the runtime's own debug lines.
//!
//! function-sdk-rust's `logging::configure` raises every crate in the binary
//! to DEBUG, so under `--debug` h2 logged every frame, wasmtime every section
//! it skipped and cranelift every compile pass (2.2 GB for an 80 MB module,
//! and timings skewed by the writes - #135). The filter here keeps `--debug`
//! to the function_wasm crates, everything else at INFO, and lets a `RUST_LOG`
//! that is set replace it in full, the tracing convention (the SDK's setup
//! never read it). The formats stay the SDK's: JSON lines for the pod,
//! human-readable under `--debug`, file and line on both.

use tracing_subscriber::EnvFilter;

/// The environment variable whose directives, when set, replace the flag's.
pub const RUST_LOG: &str = "RUST_LOG";

/// The crates whose DEBUG lines `--debug` turns on, by tracing target: the
/// `function` binary, the `function_wasm` library and the engine.
const OWN_CRATES: [&str; 3] = ["function", "function_wasm", "function_wasm_engine"];

/// The filter directives for the process: `rust_log` verbatim when it is set
/// and not blank (an operator asking for another crate's lines, cranelift's
/// say, gets exactly that), else INFO everywhere with the runtime's own
/// crates at DEBUG when `debug` is on.
pub fn directives(debug: bool, rust_log: Option<&str>) -> String {
    if let Some(set) = rust_log.map(str::trim).filter(|s| !s.is_empty()) {
        return set.to_string();
    }
    let mut directives = String::from("info");
    if debug {
        for krate in OWN_CRATES {
            directives.push_str(&format!(",{krate}=debug"));
        }
    }
    directives
}

/// Configures process-wide logging. Call once, before serving.
pub fn configure(debug: bool) {
    let rust_log = std::env::var(RUST_LOG).ok();
    // parse_lossy keeps a malformed RUST_LOG from taking the process down:
    // the bad directive is reported on stderr and the rest apply.
    let filter = EnvFilter::builder().parse_lossy(directives(debug, rust_log.as_deref()));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_file(true)
        .with_line_number(true);
    if debug {
        builder.init();
    } else {
        builder.json().init();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_everywhere_by_default() {
        assert_eq!(directives(false, None), "info");
        assert_eq!(directives(false, Some("")), "info");
        assert_eq!(directives(false, Some("  ")), "info");
    }

    #[test]
    fn debug_names_the_runtimes_crates_only() {
        assert_eq!(
            directives(true, None),
            "info,function=debug,function_wasm=debug,function_wasm_engine=debug"
        );
        let filter = EnvFilter::try_new(directives(true, None)).expect("valid directives");
        assert_eq!(
            filter.to_string().matches("=debug").count(),
            OWN_CRATES.len()
        );
    }

    #[test]
    fn rust_log_replaces_the_flag() {
        assert_eq!(
            directives(true, Some("info,cranelift_codegen=debug")),
            "info,cranelift_codegen=debug"
        );
        assert_eq!(directives(false, Some("trace")), "trace");
    }
}
