//! Cedar policy decisions - the Rust port of `internal/authz`, on
//! cedar-policy (the reference implementation cedar-go ports). It owns the
//! two policy layers of the three-layer capability decision, compiled
//! against one shared schema and AND-combined by the callers so neither can
//! widen the other: the operator's grant policy (--sandbox-policy-file,
//! default-deny, absent denies everything) and the composition author's
//! policy (the Input's compositionPolicy, scoped default-permit for sandbox
//! actions, the required default-deny fence over a composite-chosen module).
//!
//! Repositories and host patterns are modelled as Cedar entity hierarchies:
//! a location's ancestors are its path-boundary prefixes (a repository) or
//! its DNS suffixes (a host), so `in` an allowed entity is true exactly when
//! that entity equals the location or fences it at a boundary, never a
//! sibling namespace or an adjacent host. Cedar owns the decision only;
//! callers keep their own refusal strings.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};

use cedar_policy::{
    Authorizer, Context, Decision, Entities, EntityId, EntityTypeName, EntityUid, Policy,
    PolicySet, Request,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The action names of the shared schema, as a policy's action scope spells
/// them.
pub const ACTION_PULL_MODULE: &str = "pullModule";
pub const ACTION_SPEND_CREDENTIAL: &str = "spendCredential";
pub const ACTION_GRANT_EGRESS: &str = "grantEgress";
pub const ACTION_USE_PRIVATE_TMP: &str = "usePrivateTmp";
pub const ACTION_SET_ENV: &str = "setEnv";
const ACTION_REQUIRE_SIGNATURE: &str = "requireSignature";
const ACTION_DIAL_ADDRESS: &str = "dialAddress";

const COMPOSITION_ACTIONS: &[&str] = &[
    ACTION_PULL_MODULE,
    ACTION_SPEND_CREDENTIAL,
    ACTION_GRANT_EGRESS,
    ACTION_USE_PRIVATE_TMP,
    ACTION_SET_ENV,
];

/// Identifies the caller of a grant-policy decision: the observed composite
/// resource's namespace and kind, and the Composition's name. Any field may
/// be empty, in which case a policy condition testing it simply does not
/// match - safe, because both layers only narrow.
#[derive(Debug, Default, Clone)]
pub struct Principal {
    pub namespace: String,
    pub xr_kind: String,
    pub composition: String,
}

/// One egress request the policy layers judge: a single host or host
/// pattern, one method, and the rule's path prefix.
#[derive(Debug, Default, Clone)]
pub struct EgressGrant {
    pub host: String,
    pub host_pattern: String,
    pub method: String,
    pub path: String,
}

/// The composition author's own Cedar layer, compiled from the Input's
/// compositionPolicy text. Immutable once compiled; the absent layer is
/// `None` at the caller and every Permits question denies there.
#[derive(Debug)]
pub struct CompositionPolicy {
    policy: PolicySet,
    // Which actions any of the policy's rules scope - the signal the
    // scoped-default-permit reads: an author who wrote any rule for an
    // action has opted into narrowing it.
    scoped: HashMap<&'static str, bool>,
}

type CompositionCacheEntry = Result<Option<Arc<CompositionPolicy>>, String>;

/// Compiles an Input's compositionPolicy text, cached by content hash (the
/// text is per-Composition, not per-request). Failures are cached too;
/// entries are bounded by dropping the whole map at the cap. Empty text is
/// the absent layer.
pub fn compile_composition_policy(text: &str) -> CompositionCacheEntry {
    if text.is_empty() {
        return Ok(None);
    }
    static CACHE: OnceLock<Mutex<HashMap<[u8; 32], CompositionCacheEntry>>> = OnceLock::new();
    const CACHE_CAP: usize = 512;
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key: [u8; 32] = Sha256::digest(text.as_bytes()).into();
    let mut entries = cache.lock().expect("poisoned");
    if let Some(e) = entries.get(&key) {
        return e.clone();
    }
    let compiled = CompositionPolicy::new(text).map(|p| Some(Arc::new(p)));
    if entries.len() >= CACHE_CAP {
        entries.clear();
    }
    entries.insert(key, compiled.clone());
    compiled
}

impl CompositionPolicy {
    fn new(doc: &str) -> Result<Self, String> {
        let policy: PolicySet = doc
            .parse()
            .map_err(|e| format!("cannot compile the compositionPolicy as Cedar: {e}"))?;
        let mut scoped = HashMap::new();
        for action in COMPOSITION_ACTIONS {
            let hit = policy.policies().any(|p| policy_scopes_action(p, action));
            scoped.insert(*action, hit);
        }
        Ok(CompositionPolicy { policy, scoped })
    }

    /// Whether any of the policy's rules scope action - the composition
    /// layer's opt-in signal: an action no rule scopes is not narrowed by
    /// this layer.
    pub fn scopes_action(&self, action: &str) -> bool {
        self.scoped.get(action).copied().unwrap_or_default()
    }

    pub fn permits_private_tmp(&self, principal: &Principal) -> bool {
        decide(
            &self.policy,
            principal,
            ACTION_USE_PRIVATE_TMP,
            uid_json("Capability", "privateTmp"),
            Vec::new(),
            json!({}),
        )
    }

    pub fn permits_env(&self, principal: &Principal, keys: &[String]) -> bool {
        decide(
            &self.policy,
            principal,
            ACTION_SET_ENV,
            uid_json("Capability", "env"),
            Vec::new(),
            json!({ "keys": keys }),
        )
    }

    pub fn permits_egress(&self, principal: &Principal, g: &EgressGrant) -> bool {
        let (resource, entities) = host_entities(g);
        decide(
            &self.policy,
            principal,
            ACTION_GRANT_EGRESS,
            resource,
            entities,
            egress_context(g),
        )
    }

    /// Whether a permit matches pullModule for a module at location - the
    /// normalized location the source produces - over the boundary-correct
    /// Repository hierarchy. The absent layer (None at the caller) denies:
    /// the fence a composite-chosen source requires.
    pub fn permits_pull_module(&self, principal: &Principal, location: &str) -> bool {
        let (resource, entities) = repository_entities(location);
        decide(
            &self.policy,
            principal,
            ACTION_PULL_MODULE,
            resource,
            entities,
            json!({}),
        )
    }

    /// Whether a permit matches spendCredential for a step credential.
    /// location, when given (a composite-chosen source's repository),
    /// travels as context.repository with its boundary hierarchy, so a
    /// policy can co-locate both halves.
    pub fn permits_spend_credential(
        &self,
        principal: &Principal,
        credential: &str,
        location: &str,
    ) -> bool {
        let resource = uid_json("Credential", credential);
        let mut entities = vec![flat_entity(&resource)];
        let mut ctx = serde_json::Map::new();
        if !location.is_empty() {
            let (_, repo_entities) = repository_entities(location);
            entities.extend(repo_entities);
            ctx.insert(
                "repository".to_string(),
                json!({ "__entity": { "type": "Repository", "id": location } }),
            );
        }
        decide(
            &self.policy,
            principal,
            ACTION_SPEND_CREDENTIAL,
            resource,
            entities,
            Value::Object(ctx),
        )
    }
}

/// The operator's grant policy, compiled from --sandbox-policy-file and
/// immutable for the process: the sole authority that enables a sandbox
/// capability, default-deny. The no-policy-file case is `None` at the caller
/// and denies everything there.
#[derive(Debug)]
pub struct OperatorPolicy {
    policy: PolicySet,
}

impl OperatorPolicy {
    /// Reads and compiles a Cedar grant policy from a file.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "cannot read operator policy: {}",
                crate::resolver::go_io_error("open", path, &e)
            )
        })?;
        Self::new(&path.display().to_string(), &raw)
    }

    pub fn new(name: &str, doc: &str) -> Result<Self, String> {
        let policy: PolicySet = doc
            .parse()
            .map_err(|e| format!("cannot compile the operator policy {name}: {e}"))?;
        Ok(OperatorPolicy { policy })
    }

    pub fn permits_private_tmp(&self, principal: &Principal) -> bool {
        decide(
            &self.policy,
            principal,
            ACTION_USE_PRIVATE_TMP,
            uid_json("Capability", "privateTmp"),
            Vec::new(),
            json!({}),
        )
    }

    /// Whether the operator policy contains any usePrivateTmp rule, so the
    /// runtime probes $TMPDIR at startup only when a private /tmp can ever
    /// be granted.
    pub fn has_private_tmp_rules(&self) -> bool {
        self.policy
            .policies()
            .any(|p| policy_scopes_action(p, ACTION_USE_PRIVATE_TMP))
    }

    pub fn permits_env(&self, principal: &Principal, keys: &[String]) -> bool {
        decide(
            &self.policy,
            principal,
            ACTION_SET_ENV,
            uid_json("Capability", "env"),
            Vec::new(),
            json!({ "keys": keys }),
        )
    }

    pub fn permits_spend_credential(&self, principal: &Principal, credential: &str) -> bool {
        let resource = uid_json("Credential", credential);
        decide(
            &self.policy,
            principal,
            ACTION_SPEND_CREDENTIAL,
            resource,
            Vec::new(),
            json!({}),
        )
    }

    pub fn permits_egress(&self, principal: &Principal, g: &EgressGrant) -> bool {
        let (resource, entities) = host_entities(g);
        decide(
            &self.policy,
            principal,
            ACTION_GRANT_EGRESS,
            resource,
            entities,
            egress_context(g),
        )
    }

    /// Whether the operator policy demands a cosign signature for a module
    /// at location, over the boundary-correct Repository hierarchy. The
    /// requirement is caller-independent, so the principal is a placeholder.
    pub fn requires_signature(&self, location: &str) -> bool {
        let (resource, entities) = repository_entities(location);
        decide_raw(
            &self.policy,
            uid("Module", "module"),
            None,
            ACTION_REQUIRE_SIGNATURE,
            resource,
            entities,
            json!({}),
        )
    }

    /// Whether the operator policy contains any requireSignature rule, so
    /// --cosign-key without one warns loudly rather than lapse silently.
    /// (Used once --cosign-key is ported.)
    #[allow(dead_code)]
    pub fn has_signature_rules(&self) -> bool {
        self.policy
            .policies()
            .any(|p| policy_scopes_action(p, ACTION_REQUIRE_SIGNATURE))
    }

    /// Compiles the policy's Action::"dialAddress" rules into the SSRF
    /// decision table egress evaluates: forbids to blocked, permits to
    /// allowed, so no Cedar runs on the dial hot path. The dial path is
    /// security-critical, so an unrecognized ip-rule shape is a load error
    /// rather than a silently mis-compiled table.
    pub fn compile_ip_rules(&self) -> Result<IpRules, String> {
        let mut out = IpRules::default();
        let mut policies: Vec<&Policy> = self.policy.policies().collect();
        // Sort so a file with more than one malformed rule always reports
        // the same one, keeping the load error deterministic.
        policies.sort_by_key(|p| p.id().to_string());
        for pol in policies {
            let id = pol.id().to_string();
            let compiled = compile_dial_rule(pol)
                .map_err(|e| format!("operator policy: dialAddress rule {id:?}: {e}"))?;
            let Some(prefixes) = compiled else { continue };
            if pol.effect() == cedar_policy::Effect::Forbid {
                out.blocked.extend(prefixes);
            } else {
                out.allowed.extend(prefixes);
            }
        }
        Ok(out)
    }
}

/// The SSRF CIDR decision table compiled from the operator's Cedar policy,
/// in the shape the egress dial path evaluates without Cedar.
#[derive(Debug, Default)]
pub struct IpRules {
    /// Prefixes a forbid rule refuses; they win over allowed, mirroring
    /// Cedar's forbid-wins precedence.
    pub blocked: Vec<IpPrefix>,
    /// Prefixes a permit rule admits: holes in the default block list.
    pub allowed: Vec<IpPrefix>,
}

/// A masked CIDR prefix, the stdlib-only equivalent of Go's netip.Prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpPrefix {
    pub addr: IpAddr,
    pub len: u8,
}

impl IpPrefix {
    /// Parses a Cedar ip literal the way Cedar does: a bare address becomes
    /// a host prefix (/32 or /128), a CIDR its masked network.
    fn parse_cedar(s: &str) -> Result<Self, String> {
        let (addr, len) = match s.split_once('/') {
            None => {
                let addr: IpAddr = s.parse().map_err(|e| format!("{e}"))?;
                let len = if addr.is_ipv4() { 32 } else { 128 };
                (addr, len)
            }
            Some((a, l)) => {
                let addr: IpAddr = a.parse().map_err(|e| format!("{e}"))?;
                let len: u8 = l.parse().map_err(|e| format!("{e}"))?;
                let max = if addr.is_ipv4() { 32 } else { 128 };
                if len > max {
                    return Err(format!("prefix length {len} out of range"));
                }
                (addr, len)
            }
        };
        Ok(IpPrefix { addr, len }.masked())
    }

    fn masked(self) -> Self {
        let addr = match self.addr {
            IpAddr::V4(v4) => {
                let bits = u32::from(v4);
                let mask = if self.len == 0 {
                    0
                } else {
                    u32::MAX << (32 - u32::from(self.len))
                };
                IpAddr::V4((bits & mask).into())
            }
            IpAddr::V6(v6) => {
                let bits = u128::from(v6);
                let mask = if self.len == 0 {
                    0
                } else {
                    u128::MAX << (128 - u32::from(self.len))
                };
                IpAddr::V6((bits & mask).into())
            }
        };
        IpPrefix {
            addr,
            len: self.len,
        }
    }

    /// Whether ip falls inside this prefix; an IPv4 prefix never contains an
    /// IPv6 address and vice versa, as with netip. (The egress dial path's
    /// check, once the client is ported.)
    #[allow(dead_code)]
    pub fn contains(&self, ip: IpAddr) -> bool {
        IpPrefix {
            addr: ip,
            len: self.len,
        }
        .masked()
        .addr
            == self.addr
    }
}

/// Whether a policy's action scope names id, in either the
/// `== Action::"id"` or `in [..., Action::"id", ...]` form, read from the
/// stable Cedar JSON (EST) representation.
fn policy_scopes_action(pol: &Policy, id: &str) -> bool {
    let Ok(v) = pol.to_json() else { return false };
    let action = &v["action"];
    if action["entity"]["type"] == "Action" && action["entity"]["id"] == id {
        return true;
    }
    action["entities"]
        .as_array()
        .is_some_and(|es| es.iter().any(|e| e["type"] == "Action" && e["id"] == id))
}

/// Whether pol is an Action::"dialAddress" rule and, if so, the prefixes its
/// condition names. A policy for another action is Ok(None); a dialAddress
/// policy with an unrecognized shape is an error, with the Go compiler's
/// exact wording.
fn compile_dial_rule(pol: &Policy) -> Result<Option<Vec<IpPrefix>>, String> {
    let v = pol
        .to_json()
        .map_err(|e| format!("cannot read policy: {e}"))?;
    let action = &v["action"];
    let is_dial_eq = action["op"] == "=="
        && action["entity"]["type"] == "Action"
        && action["entity"]["id"] == ACTION_DIAL_ADDRESS;
    if !is_dial_eq {
        // A dialAddress rule written `action in [...]` would otherwise be
        // silently skipped and compile to nothing - a fail-open no-op for a
        // forbid meant as a block.
        let in_set = action["op"] == "in"
            && action["entities"].as_array().is_some_and(|es| {
                es.iter()
                    .any(|e| e["type"] == "Action" && e["id"] == ACTION_DIAL_ADDRESS)
            });
        if in_set {
            return Err(
                r#"must scope the action as == Action::"dialAddress", not an "in" set"#.to_string(),
            );
        }
        return Ok(None);
    }
    if v["principal"]["op"] != "All" || v["resource"]["op"] != "All" {
        return Err(
            "must not constrain the principal or resource: the dial has neither identity"
                .to_string(),
        );
    }
    let conditions = v["conditions"].as_array().cloned().unwrap_or_default();
    if conditions.is_empty() {
        // A scope-only rule applies to every address.
        return Ok(Some(vec![
            IpPrefix::parse_cedar("0.0.0.0/0").expect("v4 default"),
            IpPrefix::parse_cedar("::/0").expect("v6 default"),
        ]));
    }
    if conditions.len() != 1 {
        return Err("takes at most one when condition".to_string());
    }
    if conditions[0]["kind"] != "when" {
        return Err(format!(
            "may only use a when condition, not {}",
            conditions[0]["kind"].as_str().unwrap_or_default()
        ));
    }
    ip_test_prefixes(&conditions[0]["body"]).map(Some)
}

/// Walks an ip-test expression: context.ip.isInRange(ip("CIDR")),
/// context.ip.isLoopback(), or a || of those. Any other node is refused so
/// the compiled table cannot silently mean less than the operator wrote.
fn ip_test_prefixes(body: &Value) -> Result<Vec<IpPrefix>, String> {
    let Some(node) = body.as_object() else {
        return Err("the condition must be a single ip test".to_string());
    };
    if node.len() != 1 {
        return Err("the condition must be a single ip test".to_string());
    }
    let (op, arg) = node.iter().next().expect("one entry");
    match op.as_str() {
        "||" => {
            let mut left = ip_test_prefixes(&arg["left"])?;
            left.extend(ip_test_prefixes(&arg["right"])?);
            Ok(left)
        }
        "isInRange" => {
            let args = arg.as_array().filter(|a| a.len() == 2).ok_or_else(|| {
                r#"isInRange takes context.ip and one ip("CIDR") literal"#.to_string()
            })?;
            require_context_ip(&args[0])?;
            Ok(vec![ip_literal_prefix(&args[1])?])
        }
        "isLoopback" => {
            let args = arg
                .as_array()
                .filter(|a| a.len() == 1)
                .ok_or_else(|| "isLoopback takes context.ip and no argument".to_string())?;
            require_context_ip(&args[0])?;
            // Cedar's isLoopback is 127.0.0.0/8 for IPv4 and ::1 for IPv6.
            Ok(vec![
                IpPrefix::parse_cedar("127.0.0.0/8").expect("loopback v4"),
                IpPrefix::parse_cedar("::1/128").expect("loopback v6"),
            ])
        }
        other => Err(format!(
            "unsupported operation {other:?} (an ip test uses isInRange, isLoopback or ||)"
        )),
    }
}

/// Refuses any ip test whose subject is not context.ip: the dialed address
/// is the only value the dial supplies.
fn require_context_ip(raw: &Value) -> Result<(), String> {
    let access = &raw["."];
    if access["left"]["Var"] == "context" && access["attr"] == "ip" {
        return Ok(());
    }
    Err("an ip test may only test context.ip".to_string())
}

/// Reads the ip("CIDR") literal of an isInRange call.
fn ip_literal_prefix(raw: &Value) -> Result<IpPrefix, String> {
    let lit = raw["ip"]
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(|| r#"isInRange takes one ip("CIDR") literal"#.to_string())?;
    let value = lit[0]["Value"]
        .as_str()
        .ok_or_else(|| r#"isInRange takes one ip("CIDR") literal"#.to_string())?;
    IpPrefix::parse_cedar(value).map_err(|e| format!("{value:?} is not an ip literal: {e}"))
}

/// Evaluates one request with the Request::"self" principal carrying the
/// caller's attributes - the evaluation both layers share, so a policy means
/// the same in both.
fn decide(
    ps: &PolicySet,
    principal: &Principal,
    action: &str,
    resource: Value,
    entities: Vec<Value>,
    context: Value,
) -> bool {
    let principal_entity = json!({
        "uid": { "type": "Request", "id": "self" },
        "attrs": {
            "namespace": principal.namespace,
            "xrKind": principal.xr_kind,
            "composition": principal.composition,
        },
        "parents": [],
    });
    decide_raw(
        ps,
        uid("Request", "self"),
        Some(principal_entity),
        action,
        resource,
        entities,
        context,
    )
}

fn decide_raw(
    ps: &PolicySet,
    principal: EntityUid,
    principal_entity: Option<Value>,
    action: &str,
    resource: Value,
    mut entities: Vec<Value>,
    context: Value,
) -> bool {
    // The resource entity is added when the caller did not supply it (a flat
    // Capability or Credential), so both `resource == ...` and
    // `resource in ...` conditions evaluate.
    let present = entities.iter().any(|e| e["uid"] == resource);
    if !present {
        entities.push(flat_entity(&resource));
    }
    if let Some(p) = principal_entity {
        entities.push(p);
    }
    let Ok(entities) = Entities::from_json_value(Value::Array(entities), None) else {
        return false;
    };
    let Ok(context) = Context::from_json_value(context, None) else {
        return false;
    };
    let resource_uid = uid(
        resource["type"].as_str().unwrap_or_default(),
        resource["id"].as_str().unwrap_or_default(),
    );
    let Ok(request) = Request::new(
        principal,
        uid("Action", action),
        resource_uid,
        context,
        None,
    ) else {
        return false;
    };
    Authorizer::new()
        .is_authorized(&request, ps, &entities)
        .decision()
        == Decision::Allow
}

fn uid(ty: &str, id: &str) -> EntityUid {
    EntityUid::from_type_name_and_id(
        ty.parse::<EntityTypeName>()
            .expect("a fixed entity type name"),
        EntityId::new(id),
    )
}

/// The uid half of an entity's JSON, shared between the request and store.
fn uid_json(ty: &str, id: &str) -> Value {
    json!({ "type": ty, "id": id })
}

fn flat_entity(uid: &Value) -> Value {
    json!({ "uid": uid, "attrs": {}, "parents": [] })
}

/// The Repository entity for location and the store that gives it its
/// path-boundary ancestors, so `resource in Repository::"p"` is true when p
/// equals location (Cedar's `in` is reflexive) or fences it at a "/".
fn repository_entities(location: &str) -> (Value, Vec<Value>) {
    let resource = uid_json("Repository", location);
    let prefixes = boundary_prefixes(location);
    let mut entities: Vec<Value> = prefixes
        .iter()
        .map(|p| flat_entity(&uid_json("Repository", p)))
        .collect();
    let parents: Vec<Value> = prefixes.iter().map(|p| uid_json("Repository", p)).collect();
    entities.push(json!({ "uid": resource, "attrs": {}, "parents": parents }));
    (resource, entities)
}

/// The path-boundary ancestors of a repository location: for every prefix
/// ending immediately before a "/" both forms - "ghcr.io/team" and
/// "ghcr.io/team/" - so an allowlist entry matches with or without a
/// trailing slash, while a sibling namespace never does. The location itself
/// is not included: Cedar's `in` is reflexive.
fn boundary_prefixes(location: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, c) in location.char_indices() {
        if c != '/' {
            continue;
        }
        let p = &location[..i];
        if p.is_empty() || p.contains('\0') {
            continue;
        }
        out.push(p.to_string());
        out.push(format!("{p}/"));
    }
    out
}

/// The HostPattern entity for an egress grant and the store giving it its
/// DNS-suffix ancestors, so `resource in HostPattern::"example.com"` is true
/// for an exact host under example.com and for the pattern "*.example.com".
/// The entity carries a `host` attribute for `like` conditions.
fn host_entities(g: &EgressGrant) -> (Value, Vec<Value>) {
    let (label, boundary) = if !g.host.is_empty() {
        let l = normalize_host(&g.host);
        (l.clone(), l)
    } else {
        // A pattern "*.example.com" is bounded by example.com, which becomes
        // an ancestor so the pattern is `in HostPattern::"example.com"`.
        let l = normalize_host(&g.host_pattern);
        let b = l.strip_prefix("*.").unwrap_or(&l).to_string();
        (l, b)
    };
    let mut ancestors = Vec::new();
    if g.host.is_empty() {
        ancestors.push(boundary.clone());
    }
    ancestors.extend(dns_suffixes(&boundary));
    let resource = uid_json("HostPattern", &label);
    let mut entities: Vec<Value> = ancestors
        .iter()
        .map(|a| flat_entity(&uid_json("HostPattern", a)))
        .collect();
    let parents: Vec<Value> = ancestors
        .iter()
        .map(|a| uid_json("HostPattern", a))
        .collect();
    entities.push(json!({ "uid": resource, "attrs": { "host": label }, "parents": parents }));
    (resource, entities)
}

/// The proper label-boundary suffixes of a host, longest first:
/// "api.example.com" yields "example.com" and "com".
fn dns_suffixes(h: &str) -> Vec<String> {
    let labels: Vec<&str> = h.split('.').collect();
    (1..labels.len()).map(|i| labels[i..].join(".")).collect()
}

/// Lowercases a host name and drops surrounding space and a trailing dot, so
/// a rule and a policy compare it the way DNS does.
fn normalize_host(h: &str) -> String {
    h.trim().to_lowercase().trim_end_matches('.').to_string()
}

fn egress_context(g: &EgressGrant) -> Value {
    json!({ "method": g.method.to_uppercase(), "path": g.path })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composition(doc: &str) -> CompositionPolicy {
        CompositionPolicy::new(doc).expect("compile")
    }

    #[test]
    fn pull_module_is_boundary_correct() {
        let p = composition(
            r#"permit (principal, action == Action::"pullModule", resource in Repository::"ghcr.io/example");"#,
        );
        let principal = Principal::default();
        let cases = [
            ("ghcr.io/example/greeter", true),
            ("ghcr.io/example", true),
            ("ghcr.io/example-evil/greeter", false),
            ("ghcr.io/examplesuffix/greeter", false),
            ("evil.example.net/ghcr.io/example", false),
        ];
        for (location, want) in cases {
            assert_eq!(
                p.permits_pull_module(&principal, location),
                want,
                "{location}"
            );
        }
    }

    #[test]
    fn egress_host_hierarchy_is_dns_correct() {
        let p = composition(
            r#"permit (principal, action == Action::"grantEgress", resource in HostPattern::"example.com");"#,
        );
        let principal = Principal::default();
        let grant = |host: &str| EgressGrant {
            host: host.to_string(),
            method: "GET".to_string(),
            ..Default::default()
        };
        assert!(p.permits_egress(&principal, &grant("api.example.com")));
        assert!(p.permits_egress(&principal, &grant("example.com")));
        assert!(!p.permits_egress(&principal, &grant("example.com.attacker.net")));
        assert!(!p.permits_egress(&principal, &grant("badexample.com")));
        // A pattern is bounded by its suffix.
        let pattern = EgressGrant {
            host_pattern: "*.example.com".to_string(),
            method: "GET".to_string(),
            ..Default::default()
        };
        assert!(p.permits_egress(&principal, &pattern));
    }

    #[test]
    fn scoped_actions_are_detected() {
        let p = composition(
            r#"permit (principal, action == Action::"pullModule", resource);
               permit (principal, action in [Action::"setEnv", Action::"usePrivateTmp"], resource);"#,
        );
        assert!(p.scopes_action(ACTION_PULL_MODULE));
        assert!(p.scopes_action(ACTION_SET_ENV));
        assert!(p.scopes_action(ACTION_USE_PRIVATE_TMP));
        assert!(!p.scopes_action(ACTION_GRANT_EGRESS));
    }

    #[test]
    fn principal_conditions_narrow() {
        let p = composition(
            r#"permit (principal, action == Action::"usePrivateTmp", resource)
               when { principal.namespace == "prod" };"#,
        );
        let prod = Principal {
            namespace: "prod".to_string(),
            ..Default::default()
        };
        let dev = Principal {
            namespace: "dev".to_string(),
            ..Default::default()
        };
        assert!(p.permits_private_tmp(&prod));
        assert!(!p.permits_private_tmp(&dev));
    }

    #[test]
    fn spend_credential_reads_context_repository() {
        let p = composition(
            r#"permit (principal, action == Action::"spendCredential", resource == Credential::"regcred")
               when { context has repository && context.repository in Repository::"ghcr.io/example" };"#,
        );
        let principal = Principal::default();
        assert!(p.permits_spend_credential(&principal, "regcred", "ghcr.io/example/greeter"));
        assert!(!p.permits_spend_credential(&principal, "regcred", "ghcr.io/other/greeter"));
        assert!(!p.permits_spend_credential(&principal, "other", "ghcr.io/example/greeter"));
        // An env binding spends with no repository: the condition cannot match.
        assert!(!p.permits_spend_credential(&principal, "regcred", ""));
    }

    #[test]
    fn operator_policy_signature_requirement() {
        let p = OperatorPolicy::new(
            "test",
            r#"permit (principal, action == Action::"requireSignature", resource)
               when { resource in Repository::"ghcr.io/secure" };"#,
        )
        .expect("compile");
        assert!(p.requires_signature("ghcr.io/secure/greeter"));
        assert!(!p.requires_signature("ghcr.io/open/greeter"));
        assert!(p.has_signature_rules());
    }

    #[test]
    fn ip_rules_compile_and_refuse() {
        let good = OperatorPolicy::new(
            "test",
            r#"forbid (principal, action == Action::"dialAddress", resource)
               when { context.ip.isInRange(ip("10.0.0.0/8")) || context.ip.isLoopback() };
               permit (principal, action == Action::"dialAddress", resource)
               when { context.ip.isInRange(ip("10.1.2.3")) };
               permit (principal, action == Action::"grantEgress", resource);"#,
        )
        .expect("compile");
        let rules = good.compile_ip_rules().expect("rules");
        assert_eq!(rules.blocked.len(), 3);
        assert_eq!(rules.allowed.len(), 1);
        assert!(rules.blocked[0].contains("10.1.2.3".parse().unwrap()));
        assert!(!rules.blocked[0].contains("11.0.0.1".parse().unwrap()));
        assert_eq!(rules.allowed[0].len, 32);

        let bad = OperatorPolicy::new(
            "test",
            r#"forbid (principal, action == Action::"dialAddress", resource)
               when { context.ip.isMulticast() };"#,
        )
        .expect("compile");
        assert_eq!(
            bad.compile_ip_rules().expect_err("refuse"),
            r#"operator policy: dialAddress rule "policy0": unsupported operation "isMulticast" (an ip test uses isInRange, isLoopback or ||)"#
        );
    }
}
