//! HTTP egress through the host: the ceiling, grant and per-run client, what answers a guest's wasi:http
//! sends. The guest never opens a socket: the host resolves the name,
//! judges every resolved address against the block list (the operator's
//! Cedar dialAddress rules compiled in), dials only addresses it checked,
//! terminates TLS with its own roots, applies the module's admitted rules
//! on the first request and every redirect hop, enforces the fixed per-run
//! budgets and the process-wide rate limit, and writes one audit line per
//! request. A refusal is an in-band error the guest reads, never a trap.
//! Refusal strings are pinned by the conformance goldens; transport-level
//! error text is the library's own.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use guestfn_engine::wire;

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
/// carrier-grade NAT, IPv6 unique-local, the prefixes that carry an IPv4
/// address inside an IPv6 one (IPv4-compatible, both NAT64 prefixes, 6to4
/// and Teredo, each a second spelling of an address this list refuses),
/// and the unspecified, multicast, benchmarking and reserved ranges.
pub const DEFAULT_BLOCKED_CIDRS: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "224.0.0.0/4",
    "240.0.0.0/4",
    "::/96",
    "64:ff9b::/96",
    "64:ff9b:1::/48",
    "2001::/32",
    "2002::/16",
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
    /// The one HTTP client of the ceiling: its
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

/// What the runtime itself fetches from an address a response chose: the
/// target of a redirect that leaves the host an Input named. Such a target
/// is held to the default block list, with the egress client's own
/// guarantees (every resolved address judged, the judged address dialled,
/// no proxy), so a redirect cannot point the runtime at an address inside
/// the cluster.
pub struct Guarded {
    egress: Arc<Egress>,
}

impl Default for Guarded {
    fn default() -> Self {
        Guarded {
            egress: Arc::new(Egress::new(IpRules::default(), 0.0, 0)),
        }
    }
}

impl Guarded {
    /// GETs url without following a redirect, or says why not.
    pub fn get(
        &self,
        url: &reqwest::Url,
        timeout: Duration,
    ) -> Result<reqwest::blocking::Response, String> {
        let host = url.host_str().unwrap_or_default();
        let blocked = || format!("{host} resolves to an address the default block list refuses");
        // A literal never reaches the resolver, so it is judged here.
        let literal = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .ok();
        if let Some(ip) = literal.map(unmap)
            && (ip.is_unspecified() || self.egress.blocked_by(ip).is_some())
        {
            return Err(blocked());
        }
        self.egress
            .http_client()
            .get(url.clone())
            .timeout(timeout)
            .send()
            .map_err(|e| {
                let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&e);
                while let Some(s) = source {
                    if s.to_string().contains(REFUSAL_FRAME) {
                        return blocked();
                    }
                    source = s.source();
                }
                e.to_string()
            })
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

impl guestfn_engine::HttpRequester for Client {
    fn do_request(&self, req: &wire::Request, deadline: Instant) -> wire::Response {
        let start = Instant::now();
        let method = method_of(req);
        let (rsp, outcome, url, detail) = self.perform(req, deadline);
        guestfn_engine::metrics::HTTP_REQUESTS
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
        let body = req.body.clone();

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
                    guestfn_engine::duration::format(budget.timeout)
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
                // Authorization and Cookie survive a
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
                body: limited,
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

/// Response headers, canonicalised to the usual capitalisation
/// (Content-Type), so a guest sees the same names whatever the server sent.
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
    use guestfn_engine::HttpRequester;
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
        assert_eq!(rsp.body, b"hello");
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
    fn the_default_list_covers_addresses_that_embed_another() {
        let egress = Egress::new(IpRules::default(), 0.0, 0);
        let cases = [
            ("192.0.0.8", "192.0.0.0/24"),
            ("198.18.0.1", "198.18.0.0/15"),
            ("198.19.255.254", "198.18.0.0/15"),
            // 10.0.0.1 behind the local-use NAT64 prefix, 6to4 and Teredo.
            ("64:ff9b:1::a00:1", "64:ff9b:1::/48"),
            ("2002:a00:1::1", "2002::/16"),
            ("2001:0:a00:1::1", "2001::/32"),
        ];
        for (ip, by) in cases {
            let ip: IpAddr = ip.parse().expect("ip");
            assert_eq!(egress.blocked_by(ip).as_deref(), Some(by), "{ip}");
        }
        // Their neighbours stay reachable.
        for ip in ["192.0.1.1", "198.20.0.1", "2001:db8::1", "2003::1"] {
            let ip: IpAddr = ip.parse().expect("ip");
            assert_eq!(egress.blocked_by(ip), None, "{ip}");
        }
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
        assert_eq!(rsp.body, b"after");
    }

    /// As serve, with the responses built from the server's port, and
    /// every request it received (the head, and the body its
    /// Content-Length announces) handed back in order.
    fn recording(
        responses: impl FnOnce(u16) -> Vec<String>,
    ) -> (SocketAddr, std::sync::mpsc::Receiver<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let responses = responses(addr.port());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut conn, _)) = listener.accept() else {
                    return;
                };
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                let received = loop {
                    if let Some(received) = Received::parse(&raw) {
                        break received;
                    }
                    match conn.read(&mut buf) {
                        Ok(n) if n > 0 => raw.extend_from_slice(&buf[..n]),
                        _ => return,
                    }
                };
                let _ = tx.send(received);
                let _ = conn.write_all(response.as_bytes());
            }
        });
        (addr, rx)
    }

    /// One request as the server read it, header names lowercased.
    #[derive(Debug, PartialEq)]
    struct Received {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    impl Received {
        /// None until the head and the announced body have arrived.
        fn parse(raw: &[u8]) -> Option<Received> {
            let text = String::from_utf8_lossy(raw);
            let (head, body) = text.split_once("\r\n\r\n")?;
            let mut lines = head.lines();
            let mut request_line = lines.next()?.split_whitespace();
            let method = request_line.next()?.to_string();
            let path = request_line.next()?.to_string();
            let headers: BTreeMap<String, String> = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            let announced = headers
                .get("content-length")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            (body.len() >= announced).then(|| Received {
                method,
                path,
                headers,
                body: body.to_string(),
            })
        }

        fn header(&self, name: &str) -> Option<&str> {
            self.headers.get(name).map(String::as_str)
        }
    }

    fn redirect(status: u16, location: &str) -> String {
        format!(
            "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    /// loopback_egress with the IPv6 loopback open too: `localhost`, the
    /// tests' second host name, resolves to both, and every resolved
    /// address is judged.
    fn localhost_egress() -> Arc<Egress> {
        let rules = IpRules {
            allowed: vec![
                IpPrefix::parse("127.0.0.0/8").expect("cidr"),
                IpPrefix::parse("::1/128").expect("cidr"),
            ],
            ..Default::default()
        };
        Arc::new(Egress::new(rules, 0.0, 0))
    }

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), vec![v.to_string()]))
            .collect()
    }

    #[test]
    fn credentials_survive_a_redirect_to_the_same_host_alone() {
        // The same server under two names: 127.0.0.1, the host the request
        // names, and localhost, another host to the client.
        let cases = [
            ("127.0.0.1", Some("Bearer t0ken"), Some("session=1")),
            ("localhost", None, None),
        ];
        for (host, authorization, cookie) in cases {
            let (addr, received) = recording(|port| {
                vec![
                    redirect(302, &format!("http://{host}:{port}/next")),
                    ok_response("after"),
                ]
            });
            let egress = localhost_egress();
            let c = client_for_hosts(&egress, &["127.0.0.1", "localhost"], &["GET"]);
            let rsp = c.do_request(
                &wire::Request {
                    url: format!("http://127.0.0.1:{}/start", addr.port()),
                    headers: headers(&[
                        ("Authorization", "Bearer t0ken"),
                        ("Cookie", "session=1"),
                        ("X-Request-Id", "7"),
                    ]),
                    ..Default::default()
                },
                deadline(),
            );
            assert_eq!(rsp.error, "", "{host}");
            assert_eq!(rsp.body, b"after", "{host}");
            let first = received.recv().expect("the first request");
            assert_eq!(
                (
                    first.path.as_str(),
                    first.header("authorization"),
                    first.header("cookie"),
                    first.header("x-request-id")
                ),
                ("/start", Some("Bearer t0ken"), Some("session=1"), Some("7")),
                "{host}: the request itself"
            );
            let hop = received.recv().expect("the redirect hop");
            assert_eq!(
                (
                    hop.path.as_str(),
                    hop.header("authorization"),
                    hop.header("cookie"),
                    hop.header("x-request-id")
                ),
                ("/next", authorization, cookie, Some("7")),
                "{host}: the redirect hop"
            );
        }
    }

    #[test]
    fn a_guest_cannot_set_the_headers_the_host_owns() {
        let (addr, received) = recording(|_| vec![ok_response("ok")]);
        let egress = loopback_egress();
        let c = client_for(&egress, "127.0.0.1", &["POST"]);
        let rsp = c.do_request(
            &wire::Request {
                url: format!("http://127.0.0.1:{}/submit", addr.port()),
                method: "POST".to_string(),
                headers: headers(&[
                    ("Host", "metadata.internal"),
                    ("Content-Length", "2"),
                    ("Transfer-Encoding", "chunked"),
                    ("Content-Type", "text/plain"),
                ]),
                body: b"hello".to_vec(),
            },
            deadline(),
        );
        assert_eq!(rsp.error, "");
        let got = received.recv().expect("the request");
        let host = addr.to_string();
        assert_eq!(
            (
                got.header("host"),
                got.header("content-length"),
                got.header("transfer-encoding"),
                got.header("content-type"),
                got.body.as_str()
            ),
            (
                Some(host.as_str()),
                Some("5"),
                None,
                Some("text/plain"),
                "hello"
            )
        );
    }

    #[test]
    fn a_redirect_rewrites_the_method_as_its_status_says() {
        // 301, 302 and 303 turn a POST into a GET without its body; 307
        // and 308 repeat the request as it was.
        let cases = [
            (301, "GET", ""),
            (302, "GET", ""),
            (303, "GET", ""),
            (307, "POST", "payload"),
            (308, "POST", "payload"),
        ];
        for (status, method, body) in cases {
            let (addr, received) =
                recording(|_| vec![redirect(status, "/moved"), ok_response("after")]);
            let egress = loopback_egress();
            let c = client_for(&egress, "127.0.0.1", &["GET", "POST"]);
            let rsp = c.do_request(
                &wire::Request {
                    url: format!("http://127.0.0.1:{}/start", addr.port()),
                    method: "POST".to_string(),
                    body: b"payload".to_vec(),
                    ..Default::default()
                },
                deadline(),
            );
            assert_eq!(rsp.error, "", "{status}");
            let first = received.recv().expect("the first request");
            assert_eq!(
                (first.method.as_str(), first.body.as_str()),
                ("POST", "payload"),
                "{status}: the request itself"
            );
            let hop = received.recv().expect("the redirect hop");
            assert_eq!(
                (hop.method.as_str(), hop.path.as_str(), hop.body.as_str()),
                (method, "/moved", body),
                "{status}: the redirect hop"
            );
        }
    }

    #[test]
    fn a_redirect_hop_is_held_to_the_grant() {
        // The hop is judged as it will be sent: under another host name,
        // and with the method the redirect rewrote.
        let (addr, _received) = recording(|port| {
            vec![
                redirect(302, &format!("http://localhost:{port}/next")),
                redirect(302, "/moved"),
            ]
        });
        let port = addr.port();
        let egress = localhost_egress();
        let c = client_for(&egress, "127.0.0.1", &["POST"]);
        let post = wire::Request {
            url: format!("http://127.0.0.1:{port}/start"),
            method: "POST".to_string(),
            ..Default::default()
        };
        assert_eq!(
            c.do_request(&post, deadline()).error,
            format!(
                r#"redirect to http://localhost:{port}/next refused: sandbox.egress: no rule admits host "localhost""#
            )
        );
        assert_eq!(
            c.do_request(&post, deadline()).error,
            format!(
                r#"redirect to http://127.0.0.1:{port}/moved refused: sandbox.egress: no rule for host "127.0.0.1" admits GET /moved"#
            )
        );
    }

    #[test]
    fn the_redirect_budget_is_enforced() {
        let hops = |n: usize| {
            let mut responses = vec![redirect(302, "/again"); n];
            responses.push(ok_response("after"));
            serve(responses)
        };
        let egress = loopback_egress();
        let get = |addr: SocketAddr| {
            let c = client_for(&egress, "127.0.0.1", &["GET"]);
            let rsp = c.do_request(
                &request(format!("http://127.0.0.1:{}/start", addr.port())),
                deadline(),
            );
            (rsp.error, rsp.body)
        };
        assert_eq!(
            get(hops(DEFAULT_MAX_REDIRECTS)),
            (String::new(), b"after".to_vec())
        );
        assert_eq!(
            get(hops(DEFAULT_MAX_REDIRECTS + 1)),
            (
                "sandbox.egress: stopped after 5 redirects (maxRedirects)".to_string(),
                Vec::new()
            )
        );
    }

    #[test]
    fn the_response_body_budget_is_enforced() {
        let egress = loopback_egress();
        let get = |len: usize| {
            let addr = serve(vec![ok_response(&"x".repeat(len))]);
            let c = client_for(&egress, "127.0.0.1", &["GET"]);
            let rsp = c.do_request(
                &request(format!("http://127.0.0.1:{}/big", addr.port())),
                deadline(),
            );
            (rsp.error, rsp.body.len())
        };
        let max = DEFAULT_MAX_RESPONSE_BYTES as usize;
        assert_eq!(get(max), (String::new(), max));
        assert_eq!(
            get(max + 1),
            (
                "sandbox.egress: the response body exceeds 4194304 bytes (maxResponseBytes)"
                    .to_string(),
                0
            )
        );
    }

    /// A server that accepts and never answers.
    fn silent_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for conn in listener.incoming().flatten() {
                held.push(conn);
            }
        });
        addr
    }

    #[test]
    fn a_request_is_cut_at_the_shorter_of_its_timeout_and_the_deadline() {
        let addr = silent_server();
        let url = format!("http://127.0.0.1:{}/slow", addr.port());
        let rules = || IpRules {
            allowed: vec![IpPrefix::parse("127.0.0.0/8").expect("cidr")],
            ..Default::default()
        };
        let outcome = |egress: Egress, within: Duration| {
            let c = client_for(&Arc::new(egress), "127.0.0.1", &["GET"]);
            let (rsp, outcome, _, _) = c.perform(&request(url.clone()), Instant::now() + within);
            (rsp.error, outcome)
        };

        // The run's deadline is nearer than the ten seconds a request gets.
        assert_eq!(
            outcome(Egress::new(rules(), 0.0, 0), Duration::from_millis(50)),
            (
                "sandbox.egress: the request exceeded the run's remaining deadline".to_string(),
                "budget"
            )
        );
        // A run with no time left is told the same.
        assert_eq!(
            outcome(Egress::new(rules(), 0.0, 0), Duration::ZERO),
            (
                "sandbox.egress: the request exceeded the run's remaining deadline".to_string(),
                "budget"
            )
        );
        // The request's own timeout is the nearer one.
        let mut short = Egress::new(rules(), 0.0, 0);
        short.budget.timeout = Duration::from_millis(50);
        assert_eq!(
            outcome(short, Duration::from_secs(5)),
            (
                "sandbox.egress: the request exceeded its 50ms timeout".to_string(),
                "budget"
            )
        );
    }

    #[test]
    fn the_unspecified_address_is_refused_whatever_the_operator_allows() {
        // 0.0.0.0 reaches the host itself on most systems. An allow rule
        // that covers it opens the block list's entries, never the address.
        let rules = IpRules {
            allowed: vec![IpPrefix::parse("0.0.0.0/0").expect("cidr")],
            ..Default::default()
        };
        let egress = Arc::new(Egress::new(rules, 0.0, 0));
        let c = client_for(&egress, "0.0.0.0", &["GET"]);
        let (rsp, outcome, _, detail) =
            c.perform(&request("http://0.0.0.0:9/x".to_string()), deadline());
        assert_eq!(
            (rsp.error.as_str(), outcome, detail.as_str()),
            (
                "sandbox.egress: 0.0.0.0 resolves to an address the egress policy blocks",
                "refused",
                "0.0.0.0 is the unspecified address 0.0.0.0"
            )
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
