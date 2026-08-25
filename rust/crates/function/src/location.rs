//! Normalized module locations - what the compositionPolicy's pullModule
//! permits are matched against. The Rust port of `internal/module`'s
//! ociLocation and httpLocation, with go-containerregistry's reference
//! normalization (default registry, docker.io aliasing, the library/
//! prefix) reproduced so a policy means the same on both runtimes.

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
/// way go-containerregistry normalizes it (default registry, docker.io
/// aliasing, the library/ prefix).
pub fn parse_oci_reference(r: &str) -> Result<OciReference, String> {
    let location = oci_location(r)?;
    let (registry, repository) = location.split_once('/').expect("a location has a registry");
    let digest = r.rsplit_once('@').expect("validated by oci_location").1;
    Ok(OciReference {
        registry: registry.to_string(),
        repository: repository.to_string(),
        digest: digest.to_string(),
    })
}

/// Checks an OCI reference and returns "registry/repository", without the
/// tag or digest.
pub fn oci_location(r: &str) -> Result<String, String> {
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
    // implied, as go-containerregistry defaults.
    let (registry, rest) = match name.split_once('/') {
        Some((first, rest))
            if first.contains('.') || first.contains(':') || first == "localhost" =>
        {
            (first, rest)
        }
        _ => ("index.docker.io", name),
    };
    let registry = if registry == "docker.io" {
        "index.docker.io"
    } else {
        registry
    };
    // A tag before the digest is context only; ":" cannot appear in a
    // repository name, so anything after one is the tag.
    let repo = rest.split_once(':').map_or(rest, |(repo, _tag)| repo);
    if repo.is_empty() {
        return Err(pinned_err());
    }
    let repo = if registry == "index.docker.io" && !repo.contains('/') {
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
    Ok(format!("{registry}/{repo}"))
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

/// Whether p is its own cleaned form (Go's path.Clean, a trailing slash
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

/// Go's path.Clean: a purely lexical normalization - duplicate slashes and
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
