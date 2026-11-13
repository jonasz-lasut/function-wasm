//! Normalized module locations - what the compositionPolicy's pullModule
//! permits are matched against. OCI references follow the reference
//! normalization of the common container tooling (default registry,
//! docker.io aliasing, the library/ prefix), so a policy names a repository
//! the way `docker pull` and `crane` do. The registry host goes further
//! than that tooling, to one spelling per registry (canonical_registry):
//! a policy rule names a registry once, however a reference writes it.

/// A parsed, normalized OCI digest reference.
#[derive(Debug, Clone)]
pub struct OciReference {
    /// The registry as policy locations name it (index.docker.io for
    /// Docker Hub).
    pub registry: String,
    pub repository: String,
    /// The pinned manifest digest, sha256:<hex>.
    pub digest: String,
}

impl OciReference {
    /// The normalized location pullModule permits match against.
    #[allow(dead_code)]
    pub fn location(&self) -> String {
        format!("{}/{}", self.registry, self.repository)
    }
}

/// Parses an OCI reference pinned to its manifest digest, normalized the
/// way container tooling does (default registry, docker.io aliasing, the
/// library/ prefix).
pub fn parse_oci_reference(r: &str) -> Result<OciReference, String> {
    let pinned = parse_pinned(r)?;
    Ok(OciReference {
        registry: pinned.registry,
        repository: pinned.repository,
        digest: pinned.digest,
    })
}

/// An OCI reference pinned to its manifest digest in its canonical
/// spelling, registry/repository[:tag]@digest - how messages name an OCI
/// module, so a refusal reads the same however the reference was written.
pub fn canonical_oci_ref(r: &str) -> Result<String, String> {
    let pinned = parse_pinned(r)?;
    let tag = pinned.tag.map(|t| format!(":{t}")).unwrap_or_default();
    Ok(format!(
        "{}/{}{tag}@{}",
        pinned.registry, pinned.repository, pinned.digest
    ))
}

/// A parsed, normalized OCI reference that may carry a tag, a digest, both
/// or neither - what guestfn's push and inspect take, where the runtime
/// itself only ever accepts digest-pinned references.
#[derive(Debug, Clone)]
pub struct AnyReference {
    pub registry: String,
    pub repository: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

impl AnyReference {
    /// The reference as a Composition should hold it: pinned to digest,
    /// with the tag kept for readability when there is one.
    pub fn pinned(&self, digest: &str) -> String {
        let mut out = format!("{}/{}", self.registry, self.repository);
        if let Some(t) = &self.tag {
            out.push(':');
            out.push_str(t);
        }
        out.push('@');
        out.push_str(self.digest.as_deref().unwrap_or(digest));
        out
    }

    /// The distribution-API reference to address a manifest with: the
    /// digest when pinned, else the tag, else latest.
    pub fn manifest_ref(&self) -> String {
        self.digest
            .clone()
            .or_else(|| self.tag.clone())
            .unwrap_or_else(|| "latest".to_string())
    }
}

/// Parses any OCI reference - name[:tag][@sha256:...] - normalized the way
/// container tooling does.
pub fn parse_any_reference(r: &str) -> Result<AnyReference, String> {
    let bad = || format!("cannot parse reference {r:?}");
    let (name, digest) = match r.rsplit_once('@') {
        Some((n, d)) => {
            if !digest_is_valid(d) {
                return Err(bad());
            }
            (n, Some(d.to_string()))
        }
        None => (r, None),
    };
    // A ':' after the last '/' is the tag separator (before it, a port).
    let last_slash = name.rfind('/').map(|i| i + 1).unwrap_or(0);
    let (name, tag) = match name[last_slash..].find(':') {
        Some(i) => {
            let (n, t) = name.split_at(last_slash + i);
            (n, Some(t[1..].to_string()))
        }
        None => (name, None),
    };
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Err(bad());
    }
    // Reuse the normalization (default registry, docker.io aliasing, the
    // library/ prefix) by pinning to a placeholder digest.
    let location = oci_location(&format!("{name}@sha256:{}", "0".repeat(64))).map_err(|_| bad())?;
    let (registry, repository) = location.split_once('/').expect("a location has a registry");
    Ok(AnyReference {
        registry: registry.to_string(),
        repository: repository.to_string(),
        tag,
        digest,
    })
}

/// Checks an OCI reference and returns "registry/repository", without the
/// tag or digest.
pub fn oci_location(r: &str) -> Result<String, String> {
    let pinned = parse_pinned(r)?;
    Ok(format!("{}/{}", pinned.registry, pinned.repository))
}

/// The parts of a digest-pinned reference, normalized.
struct Pinned {
    registry: String,
    repository: String,
    /// The tag before the digest, as written: context only.
    tag: Option<String>,
    digest: String,
}

fn parse_pinned(r: &str) -> Result<Pinned, String> {
    let pinned_err = || {
        format!(
            "module.oci.ref {r:?} must be a reference pinned to its manifest digest (repository@sha256:...); tags are not supported"
        )
    };
    let (name, digest) = r.rsplit_once('@').ok_or_else(pinned_err)?;
    if !digest_is_valid(digest) || name.is_empty() || name.contains(char::is_whitespace) {
        return Err(pinned_err());
    }
    // The registry is the first path component when it can only be one (it
    // carries a "." or ":", or is "localhost"); otherwise Docker Hub is
    // implied, as container tooling defaults.
    let (registry, rest) = match name.split_once('/') {
        Some((first, rest)) if reads_as_registry(first) => {
            let registry = canonical_registry(first).ok_or_else(|| {
                format!(
                    "module.oci.ref {r:?}: registry {first:?} is not a valid registry host (a host name or an IP address with an optional port, no user information)"
                )
            })?;
            (registry, rest)
        }
        _ => (DOCKER_HUB.to_string(), name),
    };
    // A tag before the digest is context only; ":" cannot appear in a
    // repository name, so anything after one is the tag.
    let (repo, tag) = match rest.split_once(':') {
        Some((repo, tag)) => (repo, Some(tag.to_string())),
        None => (rest, None),
    };
    if repo.is_empty() {
        return Err(pinned_err());
    }
    let repo = if registry == DOCKER_HUB && !repo.contains('/') {
        format!("library/{repo}")
    } else {
        repo.to_string()
    };
    for segment in repo.split('/') {
        if !repository_segment_is_valid(segment) {
            return Err(format!(
                "module.oci.ref {r:?}: repository {repo:?} is not a valid repository name (lowercase path components, no . or .. or empty ones)"
            ));
        }
    }
    Ok(Pinned {
        registry,
        repository: repo,
        tag,
        digest: digest.to_string(),
    })
}

/// Docker Hub as locations name it.
const DOCKER_HUB: &str = "index.docker.io";

/// Whether the first path component of a name can only be a registry: it
/// carries a "." or a ":", or is "localhost".
fn reads_as_registry(first: &str) -> bool {
    first.contains('.') || first.contains(':') || first == "localhost"
}

/// The one spelling of a registry, host[:port], that every spelling
/// reaching it is read as - what the policy entities, the signature check
/// and the messages name. None when the text is no host: user information,
/// a character a host cannot carry, an empty label, a port that is no port,
/// or digits that only a lenient parser would take for an IP address.
///
/// - the host is lowercased (DNS names compare case-insensitively);
/// - a trailing dot goes (the same name, written absolute);
/// - the port goes when it is the default of the scheme the registry is
///   spoken to with (443, or 80 for a loopback host, see cleartext); any
///   other port is part of the name;
/// - an IPv6 address is printed the one way std prints it;
/// - docker.io is index.docker.io, as container tooling aliases it: the
///   one alias, applied to what the rules above leave. No other name of
///   Docker Hub is rewritten (registry-1.docker.io is a host like any
///   other).
///
/// A dot or a port is kept where the host would no longer read as a
/// registry without it: "registry:443/team/fn" names a registry, while
/// "registry/team/fn" is a Docker Hub repository.
pub fn canonical_registry(registry: &str) -> Option<String> {
    let lower = registry.to_ascii_lowercase();
    let (host, port) = split_port(&lower);
    let host = canonical_host(host)?;
    let port = match port {
        Some(p) if !p.is_empty() && p.len() <= 5 && p.bytes().all(|c| c.is_ascii_digit()) => {
            Some(p.parse::<u16>().ok()?)
        }
        Some(_) => return None,
        None => None,
    };
    let default_port = if is_loopback_host(&host) { 80 } else { 443 };
    let port = port.filter(|p| *p != default_port || !reads_as_registry(&host));
    Some(match (host.as_str(), port) {
        ("docker.io", None) => DOCKER_HUB.to_string(),
        (_, None) => host,
        (_, Some(port)) => format!("{host}:{port}"),
    })
}

/// Splits host[:port], the host of an IPv6 address kept in its brackets.
fn split_port(registry: &str) -> (&str, Option<&str>) {
    if registry.starts_with('[') {
        return match registry.split_once("]:") {
            Some((host, port)) => (&registry[..host.len() + 1], Some(port)),
            None => (registry, None),
        };
    }
    match registry.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (registry, None),
    }
}

/// The canonical form of a lowercased host, or None when it is none.
fn canonical_host(host: &str) -> Option<String> {
    if let Some(inner) = host.strip_prefix('[') {
        let address: std::net::Ipv6Addr = inner.strip_suffix(']')?.parse().ok()?;
        return Some(format!("[{address}]"));
    }
    // One trailing dot goes, unless it is all that makes a single label
    // read as a registry; the labels are checked without it either way.
    let (host, labels) = match host.strip_suffix('.') {
        Some(relative) if relative.contains('.') || relative == "localhost" => (relative, relative),
        Some(relative) => (host, relative),
        None => (host, host),
    };
    let label_is_valid = |label: &str| {
        !label.is_empty()
            && label
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
    };
    if !labels.split('.').all(label_is_valid) {
        return None;
    }
    // A host ending in a number is an IPv4 address to an HTTP client, which
    // also reads the shorthands (127.1, 0x7f.0.0.1) as one: only the
    // dotted quad is a spelling here, so the address has one name.
    let last = labels.rsplit('.').next().unwrap_or_default();
    let numeric = last.bytes().all(|c| c.is_ascii_digit())
        || last
            .strip_prefix("0x")
            .is_some_and(|hex| hex.bytes().all(|c| c.is_ascii_hexdigit()));
    if numeric && host.parse::<std::net::Ipv4Addr>().is_err() {
        return None;
    }
    Some(host.to_string())
}

/// Whether a canonical host (no port) is this machine: localhost or a
/// loopback IP address. Nothing else - "localhost.example.org" is a name
/// like any other.
fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// Whether a registry (host[:port]) is spoken to over plain HTTP: a
/// loopback host alone (tests, a local registry), whatever its port.
pub fn cleartext(registry: &str) -> bool {
    is_loopback_host(split_port(registry).0)
}

/// One path component of an OCI repository name (the distribution spec's
/// grammar): lowercase alphanumeric runs joined by ".", "_", "__" or one or
/// more "-" - so "..", "." and empty components, which a registry or proxy
/// might collapse, are not repository names.
fn repository_segment_is_valid(s: &str) -> bool {
    let b = s.as_bytes();
    let alnum = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut i = 0;
    let run = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && alnum(b[*i]) {
            *i += 1;
        }
        *i > start
    };
    if !run(&mut i) {
        return false;
    }
    while i < b.len() {
        match b[i] {
            b'.' => i += 1,
            b'_' => {
                i += 1;
                if i < b.len() && b[i] == b'_' {
                    i += 1;
                }
            }
            b'-' => {
                while i < b.len() && b[i] == b'-' {
                    i += 1;
                }
            }
            _ => return false,
        }
        if !run(&mut i) {
            return false;
        }
    }
    true
}

/// Checks a URL naming field (module.http.url or its manifestURL) and
/// returns "scheme://host/path" with the host lowercased and the path
/// required to be normalized, so a prefix cannot be escaped with dot
/// segments the server would collapse; the query is not part of the
/// location.
pub fn http_location(field: &str, raw: &str) -> Result<String, String> {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Err(format!("{field} {raw:?} must be an http or https URL"));
    };
    if scheme != "http" && scheme != "https" {
        return Err(format!("{field} {raw:?} must be an http or https URL"));
    }
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let path = tail.split('?').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(format!(
            "{field} {raw:?} must name a host and carry no user information"
        ));
    }
    if !normalized_path(path) {
        return Err(format!(
            "{field} {raw:?} must have a normalized path (no . or .. segments, no empty segments)"
        ));
    }
    Ok(format!("{scheme}://{}{path}", authority.to_lowercase()))
}

/// Whether p is its own cleaned form (see path_clean, a trailing slash
/// preserved) - the same normalization the egress path rules require.
pub fn normalized_path(p: &str) -> bool {
    if p.is_empty() {
        return true;
    }
    let mut cleaned = path_clean(p);
    if p.ends_with('/') && cleaned != "/" {
        cleaned.push('/');
    }
    cleaned == p
}

/// A purely lexical path normalization - duplicate slashes and
/// "." elided, ".." resolved against the preceding element (or kept at the
/// start of a relative path, or dropped at a rooted one's root).
fn path_clean(p: &str) -> String {
    if p.is_empty() {
        return ".".to_string();
    }
    let rooted = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    let mut dotdot = 0usize;
    for segment in p.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if out.len() > dotdot {
                    out.pop();
                } else if !rooted {
                    out.push("..");
                    dotdot = out.len();
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    match (rooted, joined.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

pub(crate) fn digest_is_valid(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oci_locations_normalize_like_go_containerregistry() {
        let digest = format!("sha256:{}", "3f2a".repeat(16));
        let cases = [
            (
                format!("ghcr.io/example/greeter@{digest}"),
                "ghcr.io/example/greeter",
            ),
            (
                format!("ghcr.io/example/greeter:v1@{digest}"),
                "ghcr.io/example/greeter",
            ),
            (
                format!("docker.io/someone/else@{digest}"),
                "index.docker.io/someone/else",
            ),
            (format!("ubuntu@{digest}"), "index.docker.io/library/ubuntu"),
            (
                format!("localhost:5000/foo/bar@{digest}"),
                "localhost:5000/foo/bar",
            ),
        ];
        for (r, want) in cases {
            assert_eq!(oci_location(&r).expect(&r), want, "{r}");
        }
        assert_eq!(
            canonical_oci_ref(&format!("GHCR.io:443/example/greeter:v1@{digest}")),
            Ok(format!("ghcr.io/example/greeter:v1@{digest}"))
        );
        assert_eq!(
            canonical_oci_ref(&format!("ubuntu@{digest}")),
            Ok(format!("index.docker.io/library/ubuntu@{digest}"))
        );
        for bad in ["ghcr.io/example/greeter:v1", "ghcr.io/Example/greeter@", ""] {
            assert!(oci_location(bad).is_err(), "{bad:?} should be refused");
        }
        let traversal = format!("ghcr.io/example/../secret@{digest}");
        assert!(
            oci_location(&traversal)
                .expect_err("dot segments")
                .contains("not a valid repository name")
        );
    }

    #[test]
    fn a_registry_has_one_spelling() {
        let cases = [
            // Already canonical.
            ("ghcr.io", "ghcr.io"),
            ("localhost", "localhost"),
            ("localhost:5000", "localhost:5000"),
            ("127.0.0.1:5000", "127.0.0.1:5000"),
            ("[::1]:5000", "[::1]:5000"),
            ("registry:5000", "registry:5000"),
            ("my_registry:5000", "my_registry:5000"),
            ("xn--bcher-kva.example", "xn--bcher-kva.example"),
            // Case.
            ("GHCR.IO", "ghcr.io"),
            ("Registry.E2E.svc:5000", "registry.e2e.svc:5000"),
            // A trailing dot.
            ("ghcr.io.", "ghcr.io"),
            ("ghcr.io.:8443", "ghcr.io:8443"),
            ("localhost.", "localhost"),
            ("127.0.0.1.", "127.0.0.1"),
            // The default port of the scheme in use: 443, and 80 for the
            // loopback hosts spoken to over plain HTTP.
            ("ghcr.io:443", "ghcr.io"),
            ("GHCR.IO.:0443", "ghcr.io"),
            ("localhost:80", "localhost"),
            ("127.0.0.1:80", "127.0.0.1"),
            ("[::1]:80", "[::1]"),
            // Any other port is part of the name.
            ("ghcr.io:80", "ghcr.io:80"),
            ("ghcr.io:8443", "ghcr.io:8443"),
            ("localhost:443", "localhost:443"),
            ("registry.e2e.svc:05000", "registry.e2e.svc:5000"),
            // An IPv6 address, the one way std prints it.
            ("[0:0:0:0:0:0:0:1]:5000", "[::1]:5000"),
            ("[2001:DB8::1]:443", "[2001:db8::1]"),
            // Docker Hub.
            ("docker.io", "index.docker.io"),
            ("index.docker.io", "index.docker.io"),
            ("DOCKER.IO", "index.docker.io"),
            ("docker.io:443", "index.docker.io"),
            ("docker.io.", "index.docker.io"),
            ("Docker.IO.:443", "index.docker.io"),
            ("index.docker.io:443", "index.docker.io"),
            ("docker.io:5000", "docker.io:5000"),
            // The one alias: any other host stays itself.
            ("registry-1.docker.io", "registry-1.docker.io"),
            ("registry-1.docker.io:443", "registry-1.docker.io"),
            // A single label reads as a registry by its port or its dot
            // alone, so neither goes.
            ("registry:443", "registry:443"),
            ("registry.", "registry."),
            ("registry.:443", "registry."),
        ];
        for (registry, want) in cases {
            assert_eq!(
                canonical_registry(registry).as_deref(),
                Some(want),
                "{registry}"
            );
            // The canonical spelling is its own.
            assert_eq!(canonical_registry(want).as_deref(), Some(want), "{want}");
        }
        for bad in [
            "user@ghcr.io",
            "user:secret@ghcr.io",
            "ghcr.io:",
            "ghcr.io:https",
            "ghcr.io:65536",
            "ghcr.io:443:443",
            "ghcr..io",
            ".ghcr.io",
            "ghcr.io..",
            "ghcr.io%2f",
            "ghcr.io#fragment",
            "ghcr.io?query",
            "gh\\cr.io",
            "b\u{fc}cher.example",
            "\u{ff47}hcr.io",
            "[::1",
            "[::1]x:5000",
            "[fe80::1%eth0]:5000",
            "::1",
            "127.1",
            "0x7f.0.0.1",
            "127.0.0.01",
            "2130706433.",
            "",
            ":5000",
        ] {
            assert_eq!(canonical_registry(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn every_spelling_of_a_registry_parses_to_one_reference() {
        let digest = format!("sha256:{}", "3f2a".repeat(16));
        let cases = [
            ("ghcr.io/acme/prod/m", "ghcr.io/acme/prod/m"),
            ("GHCR.IO/acme/prod/m", "ghcr.io/acme/prod/m"),
            ("ghcr.io:443/acme/prod/m", "ghcr.io/acme/prod/m"),
            ("ghcr.io./acme/prod/m", "ghcr.io/acme/prod/m"),
            ("Ghcr.Io.:443/acme/prod/m:v1", "ghcr.io/acme/prod/m"),
            ("ghcr.io:8443/acme/prod/m", "ghcr.io:8443/acme/prod/m"),
            (
                "registry.e2e.svc:5000/guest/m",
                "registry.e2e.svc:5000/guest/m",
            ),
            ("LOCALHOST:5000/foo/bar", "localhost:5000/foo/bar"),
            // Docker Hub: docker.io is index.docker.io once the rules above
            // have run, no registry means Docker Hub, and a single-component
            // repository there is under library/.
            ("docker.io/x", "index.docker.io/library/x"),
            ("DOCKER.IO/x", "index.docker.io/library/x"),
            ("docker.io:443/x", "index.docker.io/library/x"),
            ("docker.io./x", "index.docker.io/library/x"),
            ("index.docker.io/x", "index.docker.io/library/x"),
            ("ubuntu", "index.docker.io/library/ubuntu"),
            ("docker.io/someone/else", "index.docker.io/someone/else"),
            (
                "index.docker.io/someone/else",
                "index.docker.io/someone/else",
            ),
            // No other name is an alias of it.
            ("registry-1.docker.io/x", "registry-1.docker.io/x"),
            (
                "registry-1.docker.io/someone/else",
                "registry-1.docker.io/someone/else",
            ),
            ("someone/else", "index.docker.io/someone/else"),
        ];
        for (name, want) in cases {
            let r = format!("{name}@{digest}");
            let reference = parse_oci_reference(&r).expect(&r);
            assert_eq!(
                (reference.location(), reference.digest),
                (want.to_string(), digest.clone()),
                "{r}"
            );
            assert_eq!(oci_location(&r), Ok(want.to_string()), "{r}");
            let any = parse_any_reference(&r).expect(&r);
            assert_eq!(format!("{}/{}", any.registry, any.repository), want, "{r}");
        }
        // guestfn's references carry tags: the port is not one.
        let tagged = parse_any_reference("GHCR.IO:443/acme/prod/m:v1").expect("tagged");
        assert_eq!(
            (
                tagged.registry.as_str(),
                tagged.repository.as_str(),
                tagged.tag.as_deref()
            ),
            ("ghcr.io", "acme/prod/m", Some("v1"))
        );
        assert_eq!(
            tagged.pinned(&digest),
            format!("ghcr.io/acme/prod/m:v1@{digest}")
        );
    }

    #[test]
    fn a_registry_that_is_no_host_is_refused() {
        let digest = format!("sha256:{}", "3f2a".repeat(16));
        for registry in [
            "user@ghcr.io",
            "user:secret@ghcr.io",
            "ghcr.io:port",
            "ghcr..io",
            "127.1",
        ] {
            let r = format!("{registry}/acme/prod/m@{digest}");
            assert_eq!(
                parse_oci_reference(&r).map(|reference| reference.location()),
                Err(format!(
                    "module.oci.ref {r:?}: registry {registry:?} is not a valid registry host (a host name or an IP address with an optional port, no user information)"
                )),
                "{r}"
            );
            assert_eq!(
                parse_any_reference(&r).map(|reference| reference.registry),
                Err(format!("cannot parse reference {r:?}")),
                "{r}"
            );
        }
    }

    #[test]
    fn http_locations_normalize() {
        assert_eq!(
            http_location("module.http.url", "https://Example.COM/modules/fn.wasm?x=1")
                .expect("url"),
            "https://example.com/modules/fn.wasm"
        );
        for bad in [
            "ftp://example.com/fn.wasm",
            "https://user@example.com/fn.wasm",
            "https://example.com/a/../b.wasm",
            "https://example.com//double.wasm",
        ] {
            assert!(
                http_location("module.http.url", bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn path_clean_matches_go() {
        let cases = [
            ("/a/b/c", "/a/b/c"),
            ("/a//b", "/a/b"),
            ("/a/./b", "/a/b"),
            ("/a/b/..", "/a"),
            ("/..", "/"),
            ("a/../b", "b"),
            ("../a", "../a"),
            ("", "."),
        ];
        for (p, want) in cases {
            assert_eq!(path_clean(p), want, "{p:?}");
        }
    }
}
