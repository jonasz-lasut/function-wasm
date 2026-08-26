//! Operational endpoints and warm-up - the Rust port of the Go runtime's
//! health listener and cmd/function/warm.go. The plain-HTTP health server
//! answers /livez always and /readyz once the caches are open and
//! --warm-modules are loaded (kubelet's gRPC probe cannot speak the
//! function port's mTLS); warm-up loads every entry through the request
//! path's own resolve and cache, logging failures and never holding
//! readiness back - an entry is a hint, the request path is the truth.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cache::ModuleCache;
use crate::input::{ModuleSource, OciSource};
use crate::resolver::Resolver;

/// The readiness flag the health server reads and warm-up flips.
#[derive(Clone, Default)]
pub struct Readiness(Arc<AtomicBool>);

impl Readiness {
    pub fn ready(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Serves metrics on address at /metrics - the port function-sdk-go serves
/// on :8080 for the Go runtime. OpenMetrics 1.0 is the main exposition
/// format, served unless the scraper's Accept header asks for the classic
/// Prometheus text format and does not accept OpenMetrics; the two carry
/// identical series either way.
pub async fn serve_metrics(address: &str) -> Result<(), std::io::Error> {
    let address = if address.starts_with(':') {
        format!("0.0.0.0{address}")
    } else {
        address.to_string()
    };
    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(address = %address, "serving metrics");
    loop {
        let Ok((mut conn, _)) = listener.accept().await else {
            continue;
        };
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let n = conn.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or_default();
            let response = if path == "/metrics" {
                let (body, content_type) = if wants_classic_text(&head) {
                    (
                        function_wasm_engine::metrics::render(),
                        "text/plain; version=0.0.4",
                    )
                } else {
                    (
                        function_wasm_engine::metrics::render_openmetrics(),
                        function_wasm_engine::metrics::OPENMETRICS_CONTENT_TYPE,
                    )
                };
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string()
            };
            let _ = conn.write_all(response.as_bytes()).await;
        });
    }
}

/// Whether the request's Accept header asks for the classic Prometheus text
/// format without accepting OpenMetrics. OpenMetrics is the main format:
/// it is served when the header names it (whatever the q-values - a
/// scraper that lists it can read it), when there is no Accept header, and
/// for catch-alls like */*; only an explicit text/plain-and-not-OpenMetrics
/// preference gets the classic rendering.
fn wants_classic_text(request_head: &str) -> bool {
    let Some(accept) = request_head.lines().find_map(|l| {
        let (name, value) = l.split_once(':')?;
        name.trim().eq_ignore_ascii_case("accept").then_some(value)
    }) else {
        return false;
    };
    !accept.contains("application/openmetrics-text") && accept.contains("text/plain")
}

pub async fn serve_health(address: &str, readiness: Readiness) -> Result<(), std::io::Error> {
    // The Go flag's ":8081" shorthand means every interface.
    let address = if address.starts_with(':') {
        format!("0.0.0.0{address}")
    } else {
        address.to_string()
    };
    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(address = %address, "serving health endpoints");
    loop {
        let Ok((mut conn, _)) = listener.accept().await else {
            continue;
        };
        let readiness = readiness.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let n = conn.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or_default();
            let status = match path {
                "/livez" => "200 OK",
                "/readyz" if readiness.is_ready() => "200 OK",
                "/readyz" => "503 Service Unavailable",
                _ => "404 Not Found",
            };
            let response =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = conn.write_all(response.as_bytes()).await;
        });
    }
}

/// Loads every --warm-modules entry - an OCI reference pinned to its digest,
/// or path:<file> under --module-dir - through the request path's own
/// resolve and cache, so a warmed module is exactly what a request would
/// have loaded. Failures are logged per entry and never fatal; the caller
/// flips readiness when this returns.
pub async fn warm(entries: &[String], resolver: &Arc<Resolver>, cache: &ModuleCache) {
    if entries.is_empty() {
        return;
    }
    tracing::info!(count = entries.len(), "Warming modules");
    for entry in entries {
        let source = warm_source(entry);
        let resolved = match resolver.resolve(&source, None) {
            Ok(resolved) => resolved,
            Err(e) => {
                tracing::warn!(entry = %entry, error = %e, "Cannot warm module");
                continue;
            }
        };
        let fetch_resolver = Arc::clone(resolver);
        let target = resolved.clone();
        let loaded = cache
            .get(&resolved.digest, move || {
                fetch_resolver
                    .fetch(&target)
                    .map_err(|e| format!("cannot fetch module: {e}"))
            })
            .await;
        match loaded {
            Ok(_) => tracing::info!(entry = %entry, digest = %resolved.digest, "Warmed module"),
            Err(e) => tracing::warn!(entry = %entry, error = %e, "Cannot warm module"),
        }
    }
    tracing::info!("Warmed modules");
}

/// An entry is stated like a Composition states a module, never resolved:
/// path:<file> under --module-dir, or an OCI reference pinned to its
/// manifest digest.
fn warm_source(entry: &str) -> ModuleSource {
    if let Some(rel) = entry.strip_prefix("path:") {
        return ModuleSource {
            r#type: "Path".to_string(),
            path: rel.to_string(),
            ..Default::default()
        };
    }
    ModuleSource {
        r#type: "OCI".to_string(),
        oci: Some(OciSource {
            r#ref: entry.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheOptions;
    use function_wasm_engine::{Config, Engine};

    #[tokio::test(flavor = "multi_thread")]
    async fn warms_path_entries_into_the_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let wasm = wat::parse_str(
            r#"(module (memory (export "memory") 1)
              (func (export "wasmfn_alloc") (param i32) (result i32) i32.const 8)
              (func (export "wasmfn_run") (param i32 i32) (result i64) i64.const 0))"#,
        )
        .expect("wat");
        std::fs::write(dir.path().join("fn.wasm"), &wasm).expect("write");
        let engine = Arc::new(Engine::new(Config::default()).expect("engine"));
        let resolver = Arc::new(Resolver::new(Some(dir.path().to_owned()), 128 << 20, None));
        let cache = ModuleCache::new(Arc::clone(&engine), CacheOptions::default());

        // A bad entry is logged, not fatal; the good one lands in the cache.
        warm(
            &["path:missing.wasm".to_string(), "path:fn.wasm".to_string()],
            &resolver,
            &cache,
        )
        .await;

        let resolved = resolver
            .resolve(
                &ModuleSource {
                    r#type: "Path".to_string(),
                    path: "fn.wasm".to_string(),
                    ..Default::default()
                },
                None,
            )
            .expect("resolve");
        cache
            .get(&resolved.digest, || {
                panic!("the warm-up should have cached this")
            })
            .await
            .expect("warmed");
    }

    #[test]
    fn openmetrics_is_the_main_format() {
        // No Accept header, or one that names OpenMetrics or anything at
        // all: OpenMetrics.
        assert!(!wants_classic_text("GET /metrics HTTP/1.1\r\nHost: x\r\n"));
        assert!(!wants_classic_text(
            "GET /metrics HTTP/1.1\r\nAccept: */*\r\n"
        ));
        assert!(!wants_classic_text(
            "GET /metrics HTTP/1.1\r\nAccept: application/openmetrics-text;version=1.0.0;q=0.9,text/plain;version=0.0.4;q=0.5\r\n"
        ));
        // Only an explicit classic-and-nothing-OpenMetrics preference gets
        // the classic format.
        assert!(wants_classic_text(
            "GET /metrics HTTP/1.1\r\nAccept: text/plain; version=0.0.4\r\n"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_metrics_endpoint_negotiates_the_format() {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = probe.local_addr().expect("addr").to_string();
        drop(probe);
        let server_addr = addr.clone();
        tokio::spawn(async move {
            let _ = serve_metrics(&server_addr).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let get = |accept: &'static str| {
            let addr = addr.clone();
            async move {
                let mut conn = tokio::net::TcpStream::connect(&addr)
                    .await
                    .expect("connect");
                let header = if accept.is_empty() {
                    String::new()
                } else {
                    format!("Accept: {accept}\r\n")
                };
                conn.write_all(
                    format!("GET /metrics HTTP/1.1\r\nHost: x\r\n{header}\r\n").as_bytes(),
                )
                .await
                .expect("write");
                let mut out = Vec::new();
                let _ = conn.read_to_end(&mut out).await;
                String::from_utf8_lossy(&out).into_owned()
            }
        };
        let main = get("").await;
        assert!(
            main.contains("application/openmetrics-text; version=1.0.0; charset=utf-8"),
            "OpenMetrics is the main format: {main}"
        );
        assert!(
            main.trim_end().ends_with("# EOF"),
            "OpenMetrics body ends with EOF"
        );
        let classic = get("text/plain; version=0.0.4").await;
        assert!(
            classic.contains("text/plain; version=0.0.4"),
            "classic on request: {classic}"
        );
        assert!(!classic.contains("# EOF"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn health_endpoints_answer() {
        let readiness = Readiness::default();
        let r = readiness.clone();
        // Bind on an ephemeral port by racing: pick a port via a throwaway
        // listener, then serve on it.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = probe.local_addr().expect("addr").to_string();
        drop(probe);
        let server_addr = addr.clone();
        tokio::spawn(async move {
            let _ = serve_health(&server_addr, r).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let get = |path: &'static str| {
            let addr = addr.clone();
            async move {
                let mut conn = tokio::net::TcpStream::connect(&addr)
                    .await
                    .expect("connect");
                conn.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .await
                    .expect("write");
                let mut out = Vec::new();
                let _ = conn.read_to_end(&mut out).await;
                String::from_utf8_lossy(&out).into_owned()
            }
        };
        assert!(get("/livez").await.starts_with("HTTP/1.1 200"));
        assert!(get("/readyz").await.starts_with("HTTP/1.1 503"));
        readiness.ready();
        assert!(get("/readyz").await.starts_with("HTTP/1.1 200"));
    }
}
