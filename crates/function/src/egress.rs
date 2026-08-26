//! HTTP egress through the host - the Rust port of `internal/egress`'s
//! ceiling, grant and per-run client: the host side of the wasmfn.http
//! import. The guest never opens a socket: the host resolves the name,
//! judges every resolved address against the block list (the operator's
//! Cedar dialAddress rules compiled in), dials only addresses it checked,
//! terminates TLS with its own roots, applies the module's admitted rules
//! on the first request and every redirect hop, enforces the fixed per-run
//! budgets and the process-wide rate limit, and writes one audit line per
//! request. A refusal is an in-band error the guest reads, never a trap.
//! Refusal strings match the Go runtime's; transport-level error text is
//! the library's own (noted in rust/README.md).

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use function_wasm_engine::wire;

use crate::authz::{IpPrefix, IpRules};
use crate::egress_rules::{self, HttpRule};

/// The fixed per-run egress budgets: the declarative parts of the ceiling
/// (hosts, CIDR rules) are Cedar's, and the budgets are not configurable in
/// this release.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_MAX_REQUESTS: i64 = 16;
pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 4 << 20;
pub const DEFAULT_MAX_REDIRECTS: usize = 5;

/// Refused whatever the grant unless an operator allow rule makes an
/// exception: loopback, link-local (the cloud metadata endpoint), RFC 1918,
/// carrier-grade NAT, IPv6 unique-local, the NAT64 and IPv4-compatible
/// well-known prefixes, and the unspecified, multicast and reserved ranges.
pub const DEFAULT_BLOCKED_CIDRS: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "224.0.0.0/4",
    "240.0.0.0/4",
    "::/96",
    "64:ff9b::/96",
    "fc00::/7",
    "fe80::/10",
    "ff00::/8",
];

/// The compiled ceiling: which addresses are never dialled and the budgets
/// of every run. The operator's host allowlist is Cedar's (grantEgress),
/// applied at admission, not here. One per runtime.
pub struct Egress {
    blocked: Vec<IpPrefix>,
    allowed: Vec<IpPrefix>,
    /// Operator forbid rules, which allowed never override.
    explicit: Vec<IpPrefix>,
    budget: Budget,
    rate_limits: Option<RateLimiters>,
    /// The one HTTP client of the ceiling, like the Go transport: its
    /// resolver judges every address, it follows no redirect itself (each
    /// hop is the client's own checked request), and it speaks no proxy -
    /// the host must see the destination address to judge it.
    http: std::sync::OnceLock<reqwest::blocking::Client>,
}

#[derive(Clone, Copy)]
struct Budget {
    timeout: Duration,
    max_requests: i64,
    max_response_bytes: u64,
    max_redirects: usize,
}

impl Egress {
    /// Compiles the ceiling with the fixed budgets and default block list,
    /// the operator's Cedar-authored CIDR rules and the rate-limit flags.
    pub fn new(rules: IpRules, rate_per_minute: f64, burst: i64) -> Egress {
        let blocked = DEFAULT_BLOCKED_CIDRS
            .iter()
            .map(|c| IpPrefix::parse(c).expect("a default CIDR parses"))
            .collect();
        Egress {
            blocked,
            allowed: rules.allowed,
            explicit: rules.blocked,
            budget: Budget {
                timeout: DEFAULT_TIMEOUT,
                max_requests: DEFAULT_MAX_REQUESTS,
                max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
                max_redirects: DEFAULT_MAX_REDIRECTS,
            },
            rate_limits: (rate_per_minute > 0.0).then(|| {
                let burst = if burst <= 0 {
                    rate_per_minute.max(1.0) as i64
                } else {
                    burst
                };
                RateLimiters::new(rate_per_minute, burst)
            }),
            http: std::sync::OnceLock::new(),
        }
    }

    fn http_client(self: &Arc<Self>) -> &reqwest::blocking::Client {
        self.http.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .dns_resolver(Arc::new(JudgedResolver {
                    egress: Arc::clone(self),
                }))
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("the egress client's configuration is static")
        })
    }

    /// Compiles a module's admitted requires.egress.http rules into what one
    /// run's requests are checked against. The rules are shape-valid (the
    /// manifest ran validate_rules); the host gate is the policy layers'
    /// grantEgress, decided at admission.
    pub fn grant(self: &Arc<Self>, rules: &[HttpRule]) -> Result<Grant, String> {
        let mut compiled = Vec::with_capacity(rules.len());
        for (i, r) in rules.iter().enumerate() {
            if !r.path_prefix.is_empty() && !crate::location::normalized_path(&r.path_prefix) {
                return Err(format!(
                    "requires.egress.http[{i}].pathPrefix {:?} must be normalized (no . or .. segments, no empty segments)",
                    r.path_prefix
                ));
            }
            let mut rule = CompiledRule {
                host: String::new(),
                suffix: String::new(),
                methods: r.methods.iter().map(|m| m.to_uppercase()).collect(),
                path_prefix: r.path_prefix.clone(),
            };
            if !r.host.is_empty() {
                if !egress_rules::valid_host(&r.host) {
                    return Err(format!(
                        "requires.egress.http[{i}].host {:?} must be a bare host name (no scheme, port, path or zone)",
                        r.host
                    ));
                }
                rule.host = egress_rules::normalize_host(&r.host);
            } else {
                let Some(suffix) = pattern_suffix(&r.host_pattern) else {
                    return Err(format!(
                        "requires.egress.http[{i}].hostPattern {:?} must be a host name with one leading wildcard label",
                        r.host_pattern
                    ));
                };
                rule.suffix = suffix;
            }
            compiled.push(rule);
        }
        Ok(Grant {
            egress: Arc::clone(self),
            rules: compiled,
        })
    }

    /// Removes rate-limit entries for modules not seen for ten minutes, so a
    /// module that stops being served does not leak its bucket. Called by
    /// the same periodic sweep that trims the caches.
    pub fn sweep_rate_limiters(&self) {
        if let Some(rl) = &self.rate_limits {
            let cutoff = Instant::now() - Duration::from_secs(10 * 60);
            rl.entries
                .lock()
                .expect("poisoned")
                .retain(|_, b| b.last >= cutoff);
        }
    }

    /// Names the block-list entry that refuses ip, or None when ip may be
    /// dialled: an operator forbid wins over an operator permit, which wins
    /// over the defaults; an IPv4-mapped IPv6 address is judged as the IPv4
    /// it carries.
    fn blocked_by(&self, ip: IpAddr) -> Option<String> {
        let ip = unmap(ip);
        if let Some(p) = self.explicit.iter().find(|p| p.contains(ip)) {
            return Some(p.to_string());
        }
        if self.allowed.iter().any(|p| p.contains(ip)) {
            return None;
        }
        self.blocked
            .iter()
            .find(|p| p.contains(ip))
            .map(|p| p.to_string())
    }
}

/// A module's compiled egress rules - built per request after the manifest
/// is read and its rules admitted by the policy layers.
pub struct Grant {
    egress: Arc<Egress>,
    rules: Vec<CompiledRule>,
}

struct CompiledRule {
    host: String,
    suffix: String,
    methods: Vec<String>,
    path_prefix: String,
}

impl Grant {
    /// Checks one request - the first or a redirect hop - against the
    /// grant. The error is what the guest sees.
    fn admit(&self, method: &str, u: &Url) -> Result<(), String> {
        if u.scheme != "http" && u.scheme != "https" {
            return Err(format!(
                "sandbox.egress: only http and https URLs are allowed, not {:?}",
                u.scheme
            ));
        }
        let host = egress_rules::normalize_host(&u.hostname);
        if host.is_empty() {
            return Err("sandbox.egress: the URL has no host".to_string());
        }
        if !crate::location::normalized_path(&u.path) {
            return Err(format!(
                "sandbox.egress: the URL path {:?} must be normalized (no . or .. segments, no empty segments)",
                u.path
            ));
        }
        let method = method.to_uppercase();
        let mut host_matched = false;
        for r in &self.rules {
            if !r.host.is_empty() && r.host != host {
                continue;
            }
            if !r.suffix.is_empty() && !(host.len() > r.suffix.len() && host.ends_with(&r.suffix)) {
                continue;
            }
            host_matched = true;
            if !r.methods.contains(&method) {
                continue;
            }
            if !r.path_prefix.is_empty() && !path_or_root(&u.path).starts_with(&r.path_prefix) {
                continue;
            }
            return Ok(());
        }
        if !host_matched {
            return Err(format!("sandbox.egress: no rule admits host {host:?}"));
        }
        Err(format!(
            "sandbox.egress: no rule for host {host:?} admits {method} {}",
            path_or_root(&u.path)
        ))
    }

    /// The per-run client for this grant. digest identifies the module for
    /// process-wide rate limiting; module and digest label the audit lines.
    pub fn client(self, module: String, digest: String) -> Client {
        Client {
            grant: self,
            module,
            digest,
            requests: AtomicI64::new(0),
            over_budget: AtomicBool::new(false),
        }
    }
}

/// Performs one run's requests within its grant and the ceiling's budgets,
/// and writes the audit line for each. One per run.
pub struct Client {
    grant: Grant,
    module: String,
    digest: String,
    requests: AtomicI64,
    // The first over-budget refusal is an info line; a guest that keeps
    // asking does not flood the log.
    over_budget: AtomicBool,
}

impl function_wasm_engine::HttpRequester for Client {
    fn do_request(&self, req: &wire::Request, deadline: Instant) -> wire::Response {
        let start = Instant::now();
        let method = method_of(req);
        let (rsp, outcome, url, detail) = self.perform(req, deadline);
        function_wasm_engine::metrics::HTTP_REQUESTS
            .with_label_values(&[outcome])
            .inc();
        // The audit line: method, host and path (never the query, the
        // headers or the body), the status, the byte count and the outcome.
        // What the guest is told is in error; what only the operator should
        // see - the resolved address, the block-list entry - in detail.
        let (host, path) = url
            .as_ref()
            .map(|u| (u.hostname.clone(), path_or_root(&u.path).to_string()))
            .unwrap_or_default();
        let duration = format!("{:?}", start.elapsed());
        let quiet = outcome == "budget"
            && rsp.error.contains("maxRequests")
            && self.over_budget.swap(true, Ordering::Relaxed);
        if quiet {
            tracing::debug!(module = %self.module, digest = %self.digest, method, outcome, duration, host, path, error = %rsp.error, detail, "Module HTTP request");
        } else if rsp.error.is_empty() {
            tracing::info!(module = %self.module, digest = %self.digest, method, outcome, duration, host, path, status = rsp.status, bytes = rsp.body.len(), "Module HTTP request");
        } else {
            tracing::info!(module = %self.module, digest = %self.digest, method, outcome, duration, host, path, error = %rsp.error, detail, "Module HTTP request");
        }
        rsp
    }
}

impl Client {
    fn perform(
        &self,
        req: &wire::Request,
        deadline: Instant,
    ) -> (wire::Response, &'static str, Option<Url>, String) {
        let budget = self.grant.egress.budget;
        if self.requests.fetch_add(1, Ordering::Relaxed) + 1 > budget.max_requests {
            return (
                wire::Response::refusal(format!(
                    "sandbox.egress: this run already made {} requests (maxRequests)",
                    budget.max_requests
                )),
                "budget",
                None,
                String::new(),
            );
        }
        if let Some(rl) = &self.grant.egress.rate_limits
            && !rl.allow(&self.digest)
        {
            return (
                wire::Response::refusal(
                    "sandbox.egress: the module's request rate exceeds the egress policy's rateLimit",
                ),
                "budget",
                None,
                String::new(),
            );
        }
        let url = match Url::parse(&req.url) {
            Ok(u) => u,
            Err(e) => {
                return (
                    wire::Response::refusal(format!("sandbox.egress: cannot parse URL: {e}")),
                    "error",
                    None,
                    String::new(),
                );
            }
        };
        let method = method_of(req);
        if let Err(e) = self.grant.admit(&method, &url) {
            return (
                wire::Response::refusal(e),
                "refused",
                Some(url),
                String::new(),
            );
        }
        let body = match base64::engine::general_purpose::STANDARD.decode(&req.body) {
            Ok(b) => b,
            Err(e) => {
                return (
                    wire::Response::refusal(format!(
                        "sandbox.egress: cannot decode the request: {e}"
                    )),
                    "error",
                    Some(url),
                    String::new(),
                );
            }
        };

        // The policy's timeout applies on top of the run's remaining
        // deadline; the refusal names whichever was the shorter.
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (timeout, limit) = if remaining < budget.timeout {
            (remaining, "the run's remaining deadline".to_string())
        } else {
            (
                budget.timeout,
                format!(
                    "its {} timeout",
                    function_wasm_engine::duration::format(budget.timeout)
                ),
            )
        };
        let overall = Instant::now() + timeout;

        match self.round_trips(&method, url.clone(), &req.headers, body, overall, &limit) {
            Ok((rsp, final_url)) => (rsp, "ok", Some(final_url), String::new()),
            Err(e) => {
                let outcome = e.outcome;
                let detail = e.detail;
                ((wire::Response::refusal(e.msg)), outcome, Some(url), detail)
            }
        }
    }

    /// The request and its redirect hops, every hop re-checked against the
    /// grant and audited, sensitive headers surviving only same-host hops -
    /// net/http's redirect semantics, hand-rolled so the checks and messages
    /// are the runtime's own.
    fn round_trips(
        &self,
        method: &str,
        mut url: Url,
        headers: &BTreeMap<String, Vec<String>>,
        body: Vec<u8>,
        overall: Instant,
        limit: &str,
    ) -> Result<(wire::Response, Url), Refusal> {
        let egress = Arc::clone(&self.grant.egress);
        let client = egress.http_client();
        let origin_host = url.hostname.clone();
        let mut method = method.to_string();
        let mut body = Some(body);
        let mut hops = 0usize;
        loop {
            // An IP-literal host never reaches the resolver (reqwest dials
            // a literal directly), so the block list is applied here - on
            // the first request and on every redirect hop. The guest learns
            // only that the policy refused; the address and the block-list
            // entry stay operator-side, exactly as for a resolved name.
            let bare_host = url.hostname.trim_start_matches('[').trim_end_matches(']');
            if let Ok(ip) = bare_host.parse::<IpAddr>() {
                let ip = unmap(ip);
                let detail = if ip.is_unspecified() {
                    Some(format!("{} is the unspecified address {ip}", url.hostname))
                } else {
                    self.grant
                        .egress
                        .blocked_by(ip)
                        .map(|by| format!("{} is {ip}, blocked by {by}", url.hostname))
                };
                if let Some(detail) = detail {
                    return Err(Refusal::refused(
                        format!(
                            "sandbox.egress: {} resolves to an address the egress policy blocks",
                            url.hostname
                        ),
                        detail,
                    ));
                }
            }
            let remaining = overall.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Refusal::budget(format!(
                    "sandbox.egress: the request exceeded {limit}"
                )));
            }
            let mut hreq = client
                .request(
                    reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| {
                        Refusal::error(format!("sandbox.egress: cannot build request: {e}"))
                    })?,
                    url.raw.clone(),
                )
                .timeout(remaining);
            for (k, vs) in headers {
                if is_reserved_header(k) {
                    continue;
                }
                // As in Go's net/http, Authorization and Cookie survive a
                // redirect to the same host and are dropped elsewhere.
                if hops > 0
                    && is_sensitive_header(k)
                    && !url.hostname.eq_ignore_ascii_case(&origin_host)
                {
                    continue;
                }
                for v in vs {
                    hreq = hreq.header(k, v);
                }
            }
            if let Some(b) = &body {
                hreq = hreq.body(b.clone());
            }
            let hrsp = hreq.send().map_err(|e| classify(e, limit))?;
            let status = hrsp.status().as_u16();
            if let Some(location) = redirect_location(&hrsp) {
                hops += 1;
                if hops > egress.budget.max_redirects {
                    return Err(Refusal::budget(format!(
                        "sandbox.egress: stopped after {} redirects (maxRedirects)",
                        egress.budget.max_redirects
                    )));
                }
                let next = url.join(&location).map_err(|e| {
                    Refusal::error(format!("sandbox.egress: cannot parse URL: {e}"))
                })?;
                // net/http's method rewrite: 301/302/303 turn a body-carrying
                // method into GET; 307/308 repeat the method and body.
                if matches!(status, 301..=303) && method != "GET" && method != "HEAD" {
                    method = "GET".to_string();
                    body = None;
                }
                self.grant.admit(&method, &next).map_err(|e| {
                    Refusal::refused(
                        format!("redirect to {} refused: {e}", next.raw),
                        String::new(),
                    )
                })?;
                tracing::info!(module = %self.module, digest = %self.digest, method, host = %next.hostname, path = %path_or_root(&next.path), hop = hops, "Module HTTP redirect");
                url = next;
                continue;
            }
            // The body budget, with one extra byte to detect the overflow.
            let mut limited = Vec::new();
            let max = egress.budget.max_response_bytes;
            let mut reader = hrsp;
            let headers = canonical_headers(reader.headers());
            std::io::Read::read_to_end(
                &mut std::io::Read::take(&mut reader, max + 1),
                &mut limited,
            )
            .map_err(|e| Refusal::error(format!("sandbox.egress: {e}")))?;
            if limited.len() as u64 > max {
                return Err(Refusal::budget(format!(
                    "sandbox.egress: the response body exceeds {max} bytes (maxResponseBytes)"
                )));
            }
            let rsp = wire::Response {
                status: i32::from(status),
                headers,
                body: base64::engine::general_purpose::STANDARD.encode(&limited),
                error: String::new(),
            };
            return Ok((rsp, url));
        }
    }
}

/// Resolves a name, judges every resolved address against the block list,
/// and returns only vetted addresses - reqwest dials exactly what the
/// resolver returned, never a name a second time, so a name cannot rebind
/// between the check and the connection.
struct JudgedResolver {
    egress: Arc<Egress>,
}

impl reqwest::dns::Resolve for JudgedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let egress = Arc::clone(&self.egress);
        Box::pin(async move {
            let host = name.as_str().to_string();
            let addrs = tokio::task::spawn_blocking(move || {
                (host.as_str(), 0u16)
                    .to_socket_addrs()
                    .map(|a| a.collect::<Vec<_>>())
                    .map(|a| (host, a))
            })
            .await
            .map_err(|e| into_io(e.to_string()))?
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            let (host, addrs) = addrs;
            if addrs.is_empty() {
                return Err(into_io(format!("no addresses for {host}")));
            }
            // Every address is judged, not just the first: a name that
            // resolves to a public and a private address is refused
            // outright. The guest is told only that the policy refused;
            // which address and which entry stay operator-side.
            for a in &addrs {
                let ip = unmap(a.ip());
                if ip.is_unspecified() {
                    return Err(refusal_io(
                        format!(
                            "sandbox.egress: {host} resolves to an address the egress policy blocks"
                        ),
                        format!("{host} resolves to the unspecified address {ip}"),
                    ));
                }
                if let Some(by) = egress.blocked_by(ip) {
                    return Err(refusal_io(
                        format!(
                            "sandbox.egress: {host} resolves to an address the egress policy blocks"
                        ),
                        format!("{host} resolves to {ip}, blocked by {by}"),
                    ));
                }
            }
            let vetted: Vec<SocketAddr> = addrs
                .into_iter()
                .map(|a| SocketAddr::new(unmap(a.ip()), a.port()))
                .collect();
            Ok(Box::new(vetted.into_iter()) as Box<dyn Iterator<Item = SocketAddr> + Send>)
        })
    }
}

/// A refusal travels out of the resolver through reqwest's error chain as a
/// specially framed io::Error; classify unpacks it so the guest's message
/// and the operator's detail arrive separately.
const REFUSAL_FRAME: &str = "\u{1}refusal\u{1}";

fn refusal_io(msg: String, detail: String) -> Box<dyn std::error::Error + Send + Sync> {
    into_io(format!("{REFUSAL_FRAME}{msg}{REFUSAL_FRAME}{detail}"))
}

fn into_io(msg: String) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(msg))
}

struct Refusal {
    msg: String,
    detail: String,
    outcome: &'static str,
}

impl Refusal {
    fn refused(msg: String, detail: String) -> Refusal {
        Refusal {
            msg,
            detail,
            outcome: "refused",
        }
    }
    fn budget(msg: String) -> Refusal {
        Refusal {
            msg,
            detail: String::new(),
            outcome: "budget",
        }
    }
    fn error(msg: String) -> Refusal {
        Refusal {
            msg,
            detail: String::new(),
            outcome: "error",
        }
    }
}

/// Renders a transport error for the guest and picks its outcome: a
/// resolver refusal keeps its message (and its detail stays with the
/// operator), a timeout names the limit that applied, and anything else is
/// prefixed so a guest can tell the host's refusal from its own bug.
fn classify(e: reqwest::Error, limit: &str) -> Refusal {
    if e.is_timeout() {
        return Refusal::budget(format!("sandbox.egress: the request exceeded {limit}"));
    }
    // Walk the source chain for the framed refusal from the resolver.
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(s) = source {
        let text = s.to_string();
        if let Some(rest) = text.split_once(REFUSAL_FRAME).map(|(_, r)| r)
            && let Some((msg, detail)) = rest.split_once(REFUSAL_FRAME)
        {
            return Refusal::refused(msg.to_string(), detail.to_string());
        }
        source = s.source();
    }
    let mut text = e.to_string();
    if let Some(inner) = std::error::Error::source(&e) {
        text = format!("{text}: {inner}");
    }
    Refusal::error(format!("sandbox.egress: {text}"))
}

/// A minimal URL split, enough for the grant checks and the audit line;
/// reqwest parses the URL in full for the request itself.
#[derive(Clone, Debug)]
struct Url {
    raw: String,
    scheme: String,
    hostname: String,
    path: String,
}

impl Url {
    fn parse(raw: &str) -> Result<Url, String> {
        let parsed: reqwest::Url = raw.parse().map_err(|e| format!("{e}"))?;
        Ok(Url {
            raw: raw.to_string(),
            scheme: parsed.scheme().to_string(),
            hostname: parsed.host_str().unwrap_or_default().to_string(),
            path: parsed.path().to_string(),
        })
    }

    fn join(&self, location: &str) -> Result<Url, String> {
        let base: reqwest::Url = self.raw.parse().map_err(|e| format!("{e}"))?;
        let joined = base.join(location).map_err(|e| format!("{e}"))?;
        Url::parse(joined.as_str())
    }
}

fn redirect_location(rsp: &reqwest::blocking::Response) -> Option<String> {
    if !matches!(rsp.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    rsp.headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Response headers, canonicalised as Go's net/http does (Content-Type), so
/// a guest sees the same names from both runtimes.
fn canonical_headers(h: &reqwest::header::HeaderMap) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, v) in h {
        let Ok(v) = v.to_str() else { continue };
        out.entry(canonical_header_key(k.as_str()))
            .or_default()
            .push(v.to_string());
    }
    out
}

fn canonical_header_key(k: &str) -> String {
    k.split('-')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// Headers a guest may not set: the host owns the connection and framing,
/// and a Host header would let a request name one host and reach another.
fn is_reserved_header(k: &str) -> bool {
    [
        "host",
        "content-length",
        "connection",
        "transfer-encoding",
        "upgrade",
        "keep-alive",
        "proxy-connection",
        "te",
        "trailer",
    ]
    .contains(&k.to_ascii_lowercase().as_str())
}

fn is_sensitive_header(k: &str) -> bool {
    ["authorization", "cookie", "www-authenticate", "cookie2"]
        .contains(&k.to_ascii_lowercase().as_str())
}

fn method_of(req: &wire::Request) -> String {
    if req.method.is_empty() {
        "GET".to_string()
    } else {
        req.method.to_uppercase()
    }
}

fn path_or_root(p: &str) -> &str {
    if p.is_empty() { "/" } else { p }
}

/// Turns "*.example.com" into ".example.com" (shape-checked upstream).
fn pattern_suffix(pattern: &str) -> Option<String> {
    let p = egress_rules::normalize_host(pattern);
    p.strip_prefix('*')
        .filter(|rest| rest.starts_with('.'))
        .map(str::to_string)
}

fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// A process-wide map of module digest to token bucket, with idle expiry.
struct RateLimiters {
    per_minute: f64,
    burst: i64,
    entries: Mutex<HashMap<String, Bucket>>,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiters {
    fn new(per_minute: f64, burst: i64) -> RateLimiters {
        RateLimiters {
            per_minute,
            burst,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Whether the module identified by digest may make one request right
    /// now. Never blocks: a denied request is a budget error, not a queue.
    fn allow(&self, digest: &str) -> bool {
        let mut entries = self.entries.lock().expect("poisoned");
        let now = Instant::now();
        let bucket = entries.entry(digest.to_string()).or_insert_with(|| Bucket {
            tokens: self.burst as f64,
            last: now,
        });
        let rate_per_sec = self.per_minute / 60.0;
        bucket.tokens = (bucket.tokens
            + now.duration_since(bucket.last).as_secs_f64() * rate_per_sec)
            .min(self.burst as f64);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use function_wasm_engine::HttpRequester;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    /// A one-thread HTTP server answering each connection with the next
    /// canned response; enough of HTTP/1.1 for the client under test.
    fn serve(responses: Vec<String>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut conn, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(response.as_bytes());
            }
        });
        addr
    }

    fn ok_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn loopback_egress() -> Arc<Egress> {
        // The tests' server lives on loopback, which the default block list
        // refuses: punch the operator-style hole a test cluster would.
        let rules = IpRules {
            allowed: vec![IpPrefix::parse("127.0.0.0/8").expect("cidr")],
            ..Default::default()
        };
        Arc::new(Egress::new(rules, 0.0, 0))
    }

    fn client_for(egress: &Arc<Egress>, host: &str, methods: &[&str]) -> Client {
        client_for_hosts(egress, &[host], methods)
    }

    fn client_for_hosts(egress: &Arc<Egress>, hosts: &[&str], methods: &[&str]) -> Client {
        let rules: Vec<HttpRule> = hosts
            .iter()
            .map(|host| HttpRule {
                host: host.to_string(),
                methods: methods.iter().map(|m| m.to_string()).collect(),
                ..Default::default()
            })
            .collect();
        egress
            .grant(&rules)
            .expect("grant")
            .client("module file fn.wasm".to_string(), "sha256:m".to_string())
    }

    fn request(url: String) -> wire::Request {
        wire::Request {
            url,
            ..Default::default()
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn performs_an_admitted_request() {
        let addr = serve(vec![ok_response("hello")]);
        let egress = loopback_egress();
        let c = client_for(&egress, "127.0.0.1", &["GET"]);
        let rsp = c.do_request(
            &request(format!("http://127.0.0.1:{}/greet", addr.port())),
            deadline(),
        );
        assert_eq!(rsp.error, "", "unexpected error: {}", rsp.error);
        assert_eq!(rsp.status, 200);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&rsp.body)
                .expect("base64"),
            b"hello"
        );
        assert_eq!(
            rsp.headers.get("Content-Type"),
            Some(&vec!["text/plain".to_string()])
        );
    }

    #[test]
    fn refuses_hosts_and_methods_outside_the_grant() {
        let egress = loopback_egress();
        let c = client_for(&egress, "api.example.com", &["GET"]);
        let rsp = c.do_request(
            &request("http://other.example.com/x".to_string()),
            deadline(),
        );
        assert_eq!(
            rsp.error,
            r#"sandbox.egress: no rule admits host "other.example.com""#
        );
        let rsp = c.do_request(
            &wire::Request {
                url: "http://api.example.com/x".to_string(),
                method: "POST".to_string(),
                ..Default::default()
            },
            deadline(),
        );
        assert_eq!(
            rsp.error,
            r#"sandbox.egress: no rule for host "api.example.com" admits POST /x"#
        );
    }

    #[test]
    fn blocks_addresses_on_the_default_list() {
        // No loopback hole here: the grant admits the host, the address is
        // judged and refused, and the guest learns only that the policy did.
        let egress = Arc::new(Egress::new(IpRules::default(), 0.0, 0));
        let c = client_for(&egress, "localhost", &["GET"]);
        let rsp = c.do_request(&request("http://localhost/x".to_string()), deadline());
        assert_eq!(
            rsp.error,
            "sandbox.egress: localhost resolves to an address the egress policy blocks"
        );
    }

    #[test]
    fn judges_an_ip_literal_host_against_the_block_list() {
        // No loopback hole: a granted IP-literal host never reaches the
        // resolver, so the hop loop itself must judge it.
        let egress = Arc::new(Egress::new(IpRules::default(), 0.0, 0));
        let c = client_for(&egress, "127.0.0.1", &["GET"]);
        let rsp = c.do_request(&request("http://127.0.0.1/x".to_string()), deadline());
        assert_eq!(
            rsp.error,
            "sandbox.egress: 127.0.0.1 resolves to an address the egress policy blocks"
        );
        let c = client_for(&egress, "169.254.169.254", &["GET"]);
        let rsp = c.do_request(
            &request("http://169.254.169.254/token".to_string()),
            deadline(),
        );
        assert_eq!(
            rsp.error,
            "sandbox.egress: 169.254.169.254 resolves to an address the egress policy blocks"
        );
    }

    #[test]
    fn judges_a_redirect_to_an_ip_literal() {
        // The first hop is admitted and allowed (loopback hole); the
        // redirect names a metadata-service literal outside the hole, and
        // the hop loop refuses it even though the grant admits the host.
        let redirect = "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/token\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_string();
        let addr = serve(vec![redirect]);
        let egress = loopback_egress();
        let c = client_for_hosts(&egress, &["127.0.0.1", "169.254.169.254"], &["GET"]);
        let rsp = c.do_request(
            &request(format!("http://127.0.0.1:{}/moved", addr.port())),
            deadline(),
        );
        assert_eq!(
            rsp.error,
            "sandbox.egress: 169.254.169.254 resolves to an address the egress policy blocks"
        );
    }

    #[test]
    fn follows_redirects_within_the_grant() {
        let redirect =
            "HTTP/1.1 302 Found\r\nLocation: /moved\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string();
        let addr = serve(vec![redirect, ok_response("after")]);
        let egress = loopback_egress();
        let c = client_for(&egress, "127.0.0.1", &["GET"]);
        let rsp = c.do_request(
            &request(format!("http://127.0.0.1:{}/start", addr.port())),
            deadline(),
        );
        assert_eq!(rsp.error, "", "unexpected error: {}", rsp.error);
        assert_eq!(rsp.status, 200);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&rsp.body)
                .expect("base64"),
            b"after"
        );
    }

    #[test]
    fn the_request_budget_is_enforced() {
        let egress = loopback_egress();
        let c = client_for(&egress, "api.example.com", &["GET"]);
        for _ in 0..DEFAULT_MAX_REQUESTS {
            let _ = c.do_request(
                &request("http://other.example.com/x".to_string()),
                deadline(),
            );
        }
        let rsp = c.do_request(&request("http://api.example.com/x".to_string()), deadline());
        assert_eq!(
            rsp.error,
            "sandbox.egress: this run already made 16 requests (maxRequests)"
        );
    }

    /// The full mechanics: a WAT guest calls wasmfn.http, the engine
    /// re-enters its allocator with the client's answer, and the guest
    /// returns the JSON as its response bytes - a real request through a
    /// real socket, end to end.
    #[test]
    fn a_guest_reaches_a_server_through_the_engine() {
        let addr = serve(vec![ok_response("hi from the host")]);
        let egress = loopback_egress();
        let client = client_for(&egress, "127.0.0.1", &["GET"]);

        let request = format!(r#"{{"url":"http://127.0.0.1:{}/greet"}}"#, addr.port());
        let wat = format!(
            r#"(module
  (import "wasmfn" "http" (func $http (param i32 i32) (result i64)))
  (memory (export "memory") 4)
  (data (i32.const 1024) "{data}")
  (global $next (mut i32) (i32.const 131072))
  (func (export "wasmfn_alloc") (param i32) (result i32)
    (local $ptr i32)
    global.get $next
    local.tee $ptr
    local.get 0
    i32.add
    global.set $next
    local.get $ptr)
  (func (export "wasmfn_run") (param i32 i32) (result i64)
    i32.const 1024
    i32.const {len}
    call $http))"#,
            data = request
                .bytes()
                .map(|b| format!("\\{b:02x}"))
                .collect::<String>(),
            len = request.len(),
        );
        let engine = function_wasm_engine::Engine::new(Default::default()).expect("engine");
        let m = engine
            .compile(&wat::parse_str(&wat).expect("wat"))
            .expect("compile");
        let opts = function_wasm_engine::RunOptions {
            http: Some(Arc::new(client)),
            ..Default::default()
        };
        let out = engine.run(&m, b"", opts).expect("run");
        let answer: serde_json::Value = serde_json::from_slice(&out).expect("json");
        assert_eq!(answer["status"], 200);
        let body = base64::engine::general_purpose::STANDARD
            .decode(answer["body"].as_str().expect("body"))
            .expect("base64");
        assert_eq!(body, b"hi from the host");
    }

    #[test]
    fn the_rate_limit_is_process_wide_per_digest() {
        let egress = Arc::new(Egress::new(IpRules::default(), 60.0, 2));
        let c = client_for(&egress, "api.example.com", &["GET"]);
        // Two burst tokens, then refusal - even across clients of the same
        // digest.
        let refused =
            "sandbox.egress: the module's request rate exceeds the egress policy's rateLimit";
        let first = c.do_request(
            &request("http://other.example.com/x".to_string()),
            deadline(),
        );
        assert_ne!(first.error, refused);
        let c2 = client_for(&egress, "api.example.com", &["GET"]);
        let second = c2.do_request(
            &request("http://other.example.com/x".to_string()),
            deadline(),
        );
        assert_ne!(second.error, refused);
        let third = c.do_request(
            &request("http://other.example.com/x".to_string()),
            deadline(),
        );
        assert_eq!(third.error, refused);
    }
}
