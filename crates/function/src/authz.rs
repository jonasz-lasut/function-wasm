//! Cedar policy decisions, on
//! cedar-policy (Cedar's reference implementation). It owns the
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
//! sibling namespace or an adjacent host. A composed resource's kind is
//! `Kind::"<group>/<Kind>"` under its one `Group::"<group>"` (the core group
//! spelled `core`, the API version absent), so a rule names a kind or a
//! whole group. Cedar owns the decision only; callers keep their own
//! refusal strings.

use std::collections::{BTreeMap, HashMap};
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
/// The output fence (#174): judged over a module's response, in both
/// layers, scoped default-permit in each - the one action the operator
/// layer does not decide default-deny, since a runtime with no policy file
/// must keep serving every module's output as before.
pub const ACTION_COMPOSE_RESOURCE: &str = "composeResource";
/// The read fence (#208): judged over the resource selectors a module's
/// response asks Crossplane for, in both layers, scoped default-permit in
/// each as composeResource is and for the same reason - nothing judged the
/// ask before, and a module that reads an EnvironmentConfig must keep
/// running on upgrade.
pub const ACTION_REQUIRE_RESOURCE: &str = "requireResource";
const ACTION_REQUIRE_SIGNATURE: &str = "requireSignature";
const ACTION_TRUST_SIGNER: &str = "trustSigner";
const ACTION_DIAL_ADDRESS: &str = "dialAddress";

const COMPOSITION_ACTIONS: &[&str] = &[
    ACTION_PULL_MODULE,
    ACTION_SPEND_CREDENTIAL,
    ACTION_GRANT_EGRESS,
    ACTION_USE_PRIVATE_TMP,
    ACTION_SET_ENV,
    ACTION_COMPOSE_RESOURCE,
    ACTION_REQUIRE_RESOURCE,
];

/// The composition layer's scoped default-permit actions: the ones it
/// narrows only where a rule scopes them, and the ones a forbid with an
/// unconstrained action applies to without a permit beside it. pullModule
/// is not among them: it is default-deny, as spendCredential is for a
/// composite-chosen source.
const COMPOSITION_NARROWED: &[&str] = &[
    ACTION_SPEND_CREDENTIAL,
    ACTION_GRANT_EGRESS,
    ACTION_USE_PRIVATE_TMP,
    ACTION_SET_ENV,
    ACTION_COMPOSE_RESOURCE,
    ACTION_REQUIRE_RESOURCE,
];

/// The two actions judged over a module's response, which the operator
/// layer decides scoped default-permit like the composition layer does.
const OUTPUT_FENCE_ACTIONS: &[&str] = &[ACTION_COMPOSE_RESOURCE, ACTION_REQUIRE_RESOURCE];

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

/// How a module changed one entry of the desired state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Add,
    Modify,
    Remove,
}

impl ChangeKind {
    /// The word a policy's `context.change` compares with.
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Add => "add",
            ChangeKind::Modify => "modify",
            ChangeKind::Remove => "remove",
        }
    }

    pub fn parse(s: &str) -> Option<ChangeKind> {
        match s {
            "add" => Some(ChangeKind::Add),
            "modify" => Some(ChangeKind::Modify),
            "remove" => Some(ChangeKind::Remove),
            _ => None,
        }
    }
}

/// One change a module made to the desired state, as the composeResource
/// decision sees it: the resource's kind within its group, what happened to
/// it, and the composition resource name it happened under (empty for the
/// composite resource).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The API group; `core` for the core group.
    pub group: String,
    pub kind: String,
    pub change: ChangeKind,
    pub name: String,
    pub composite: bool,
}

impl Change {
    /// The Kind entity's id, `<group>/<Kind>`.
    pub fn kind_id(&self) -> String {
        format!("{}/{}", self.group, self.kind)
    }

    /// The word a policy's `context.target` compares with.
    pub fn target(&self) -> &'static str {
        if self.composite {
            "composite"
        } else {
            "composed"
        }
    }
}

/// How a resource selector matches what it asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementMatch {
    /// `matchName`: one resource by name.
    Name(String),
    /// `matchLabels`: every resource with these labels, in key order.
    Labels(BTreeMap<String, String>),
    /// Neither: a selector Crossplane would not serve, judged by its kind
    /// alone, with no `name` or `labels` in the context.
    None,
}

/// One resource selector a module's response asks Crossplane for, as the
/// requireResource decision sees it (#208): the resource's kind within its
/// group, how it matches, the namespace when the selector sets one, and the
/// requirements map and key it came from, for the sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// The API group; `core` for the core group.
    pub group: String,
    pub kind: String,
    pub matches: RequirementMatch,
    pub namespace: Option<String>,
    /// `resources` or `extra_resources`, as the response spelled it.
    pub field: String,
    pub key: String,
}

impl Requirement {
    /// The Kind entity's id, `<group>/<Kind>`.
    pub fn kind_id(&self) -> String {
        format!("{}/{}", self.group, self.kind)
    }
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
    policy: Judged,
    // Which actions any of the policy's rules scope - the signal the
    // scoped-default-permit reads: an author who wrote any rule for an
    // action has opted into narrowing it.
    scoped: HashMap<&'static str, bool>,
    // The policies with everything permitted, present when a forbid leaves
    // its action unconstrained: what an action no rule names is judged
    // under, so that such a forbid applies to it.
    open: Option<Judged>,
}

type CompositionCacheEntry = Result<Option<Arc<CompositionPolicy>>, String>;

/// Compiles an Input's compositionPolicy text, cached by content hash (the
/// text is per-Composition, not per-request). Failures are cached too;
/// entries are bounded by dropping the whole map at the cap. Empty text is
/// the absent layer.
pub fn compile_composition_policy(text: &str) -> CompositionCacheEntry {
    static CACHE: OnceLock<CompositionCache> = OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .get(text, CompositionPolicy::new)
}

/// The compiled compositionPolicy texts, by content hash. The map's lock
/// is held for a lookup alone: an entry is a cell its first request
/// compiles into, outside that lock, so a new text being parsed (tens of
/// milliseconds for a large one) holds back the requests carrying that
/// same text, which wait for the one compile, and no request carrying
/// another.
#[derive(Default)]
struct CompositionCache {
    entries: Mutex<HashMap<[u8; 32], Arc<OnceLock<CompositionCacheEntry>>>>,
}

impl CompositionCache {
    /// Most texts held before the map starts over.
    const CAP: usize = 512;

    /// The entry for text, compiled by compile when this is the first
    /// request to carry it. Whatever compile checks is checked before the
    /// entry exists for anyone.
    fn get(
        &self,
        text: &str,
        compile: impl FnOnce(&str) -> Result<CompositionPolicy, String>,
    ) -> CompositionCacheEntry {
        if text.is_empty() {
            return Ok(None);
        }
        let key: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        let cell = {
            let mut entries = self.entries.lock().expect("poisoned");
            match entries.get(&key) {
                Some(cell) => Arc::clone(cell),
                None => {
                    // A cell dropped here while its compile runs still
                    // answers the requests holding it.
                    if entries.len() >= Self::CAP {
                        entries.clear();
                    }
                    Arc::clone(entries.entry(key).or_default())
                }
            }
        };
        cell.get_or_init(|| compile(text).map(|p| Some(Arc::new(p))))
            .clone()
    }
}

impl CompositionPolicy {
    fn new(doc: &str) -> Result<Self, String> {
        let policy: PolicySet = doc
            .parse()
            .map_err(|e| format!("cannot compile the compositionPolicy as Cedar: {e}"))?;
        check_repository_names(&policy)?;
        let mut scoped = HashMap::new();
        for action in COMPOSITION_ACTIONS {
            let hit = policy.policies().any(|p| policy_scopes_action(p, action));
            scoped.insert(*action, hit);
        }
        let open = open_under_forbids(&policy)?;
        Ok(CompositionPolicy {
            policy: Judged::new(policy),
            scoped,
            open: open.map(Judged::new),
        })
    }

    /// Whether the layer judges action at all - its opt-in signal: a rule
    /// names the action, or, for an action the layer narrows, a forbid
    /// leaves its action unconstrained. An action neither holds for is not
    /// narrowed by this layer.
    pub fn scopes_action(&self, action: &str) -> bool {
        self.names_action(action) || (self.open.is_some() && COMPOSITION_NARROWED.contains(&action))
    }

    fn names_action(&self, action: &str) -> bool {
        self.scoped.get(action).copied().unwrap_or_default()
    }

    /// The policies a narrowed action is judged under: the layer's own
    /// where a rule names the action (a permit must match), and otherwise
    /// the layer with everything permitted, so that only its forbids with
    /// an unconstrained action decide.
    fn narrowing(&self, action: &str) -> &Judged {
        match &self.open {
            Some(open) if !self.names_action(action) => open,
            _ => &self.policy,
        }
    }

    pub fn permits_private_tmp(&self, principal: &Principal) -> bool {
        decide(
            self.narrowing(ACTION_USE_PRIVATE_TMP),
            principal,
            ACTION_USE_PRIVATE_TMP,
            uid_json("Capability", "privateTmp"),
            Vec::new(),
            json!({}),
        )
    }

    pub fn permits_env(&self, principal: &Principal, keys: &[String]) -> bool {
        decide(
            self.narrowing(ACTION_SET_ENV),
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
            self.narrowing(ACTION_GRANT_EGRESS),
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
        // With no repository in play the credential is a sandbox grant,
        // which the layer narrows; for a composite-chosen source it is
        // fenced default-deny, under the layer's own policies alone.
        let policies = if location.is_empty() {
            self.narrowing(ACTION_SPEND_CREDENTIAL)
        } else {
            &self.policy
        };
        decide(
            policies,
            principal,
            ACTION_SPEND_CREDENTIAL,
            resource,
            entities,
            Value::Object(ctx),
        )
    }

    /// Whether a permit matches composeResource for one change a module
    /// made to the desired state, over the Kind-in-Group hierarchy with
    /// `context.change`, `context.name` and `context.target`.
    pub fn permits_compose(&self, principal: &Principal, change: &Change) -> bool {
        let (resource, entities) = kind_entities(&change.group, &change.kind);
        decide(
            self.narrowing(ACTION_COMPOSE_RESOURCE),
            principal,
            ACTION_COMPOSE_RESOURCE,
            resource,
            entities,
            compose_context(change),
        )
    }

    /// Whether a permit matches requireResource for one resource selector
    /// of a module's response, over the Kind-in-Group hierarchy with
    /// `context.name`, `context.labels` and `context.namespace`, each
    /// present only where the selector sets it.
    pub fn permits_require(&self, principal: &Principal, r: &Requirement) -> bool {
        let (resource, entities) = kind_entities(&r.group, &r.kind);
        decide(
            self.narrowing(ACTION_REQUIRE_RESOURCE),
            principal,
            ACTION_REQUIRE_RESOURCE,
            resource,
            entities,
            require_context(r),
        )
    }
}

/// The operator's grant policy, compiled from --sandbox-policy-file and
/// immutable for the process: the sole authority that enables a sandbox
/// capability, default-deny. The no-policy-file case is `None` at the caller
/// and denies everything there.
#[derive(Debug)]
pub struct OperatorPolicy {
    policy: Judged,
    // Which of the output-fence actions any rule scopes: the operator layer
    // narrows a module's output, and its reads, only where it says so.
    scoped: HashMap<&'static str, bool>,
    // The policies with everything permitted, present when a forbid leaves
    // its action unconstrained: what an action no rule names is judged
    // under, so that such a forbid applies to it.
    open: Option<Judged>,
}

impl crate::cosign::SignerPolicy for OperatorPolicy {
    fn trusts_signer(&self, repository: &str, signer: &crate::cosign::Signer) -> bool {
        self.permits_signer(repository, &signer.issuer, &signer.identity)
    }
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
        check_repository_names(&policy).map_err(|e| format!("operator policy {name}: {e}"))?;
        let mut scoped = HashMap::new();
        for action in OUTPUT_FENCE_ACTIONS {
            let hit = policy.policies().any(|p| policy_scopes_action(p, action));
            scoped.insert(*action, hit);
        }
        let open = open_under_forbids(&policy)
            .map_err(|e| format!("cannot compile the operator policy {name}: {e}"))?;
        Ok(OperatorPolicy {
            policy: Judged::new(policy),
            scoped,
            open: open.map(Judged::new),
        })
    }

    /// Whether any rule scopes one of the output-fence actions - the
    /// operator layer's opt-in to judging a module's response. Unscoped, the
    /// layer does not narrow a module's output or its reads: unlike a
    /// capability, which no policy file means no grant for, neither was
    /// gated before its fence existed. A forbid that leaves its action
    /// unconstrained opts the layer in too, and is then all that decides.
    /// False for every other action, which the layer decides default-deny.
    pub fn scopes_action(&self, action: &str) -> bool {
        self.names_action(action) || (self.open.is_some() && OUTPUT_FENCE_ACTIONS.contains(&action))
    }

    fn names_action(&self, action: &str) -> bool {
        self.scoped.get(action).copied().unwrap_or_default()
    }

    /// As the composition layer's: the layer's own policies where a rule
    /// names the fence action, otherwise its forbids alone.
    fn narrowing(&self, action: &str) -> &Judged {
        match &self.open {
            Some(open) if !self.names_action(action) => open,
            _ => &self.policy,
        }
    }

    /// Whether any rule scopes composeResource.
    pub fn scopes_compose_resource(&self) -> bool {
        self.scopes_action(ACTION_COMPOSE_RESOURCE)
    }

    /// Whether any rule scopes requireResource.
    pub fn scopes_require_resource(&self) -> bool {
        self.scopes_action(ACTION_REQUIRE_RESOURCE)
    }

    /// Whether a permit matches requireResource for one resource selector,
    /// as the composition layer decides it.
    pub fn permits_require(&self, principal: &Principal, r: &Requirement) -> bool {
        let (resource, entities) = kind_entities(&r.group, &r.kind);
        decide(
            self.narrowing(ACTION_REQUIRE_RESOURCE),
            principal,
            ACTION_REQUIRE_RESOURCE,
            resource,
            entities,
            require_context(r),
        )
    }

    /// Whether a permit matches composeResource for one change, as the
    /// composition layer decides it.
    pub fn permits_compose(&self, principal: &Principal, change: &Change) -> bool {
        let (resource, entities) = kind_entities(&change.group, &change.kind);
        decide(
            self.narrowing(ACTION_COMPOSE_RESOURCE),
            principal,
            ACTION_COMPOSE_RESOURCE,
            resource,
            entities,
            compose_context(change),
        )
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
            ("Module", "module"),
            None,
            ACTION_REQUIRE_SIGNATURE,
            resource,
            entities,
            json!({}),
        )
    }

    /// Whether the operator policy contains any requireSignature rule, so
    /// --cosign-key without one warns loudly rather than lapse silently.
    pub fn has_signature_rules(&self) -> bool {
        self.policy
            .policies()
            .any(|p| policy_scopes_action(p, ACTION_REQUIRE_SIGNATURE))
    }

    /// Whether the operator policy trusts a keyless cosign signature by
    /// identity, authenticated by the OIDC issuer, for a module at location:
    /// a trustSigner permit over the boundary-correct Repository hierarchy
    /// whose condition reads context.issuer and context.identity (Cedar's
    /// `like` matches a workflow's tag pattern). Default-deny: no rule, no
    /// trusted signer. Caller-independent, so the principal is a placeholder.
    pub fn permits_signer(&self, location: &str, issuer: &str, identity: &str) -> bool {
        let (resource, entities) = repository_entities(location);
        decide_raw(
            &self.policy,
            ("Module", "module"),
            None,
            ACTION_TRUST_SIGNER,
            resource,
            entities,
            json!({ "issuer": issuer, "identity": identity }),
        )
    }

    /// Whether the operator policy contains any trustSigner rule, so
    /// --cosign-trusted-root without one warns loudly rather than lapse
    /// silently.
    pub fn has_signer_rules(&self) -> bool {
        self.policy
            .policies()
            .any(|p| policy_scopes_action(p, ACTION_TRUST_SIGNER))
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

/// A masked CIDR prefix, the stdlib-only form of an address and a length.
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

    pub(crate) fn masked(self) -> Self {
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

    /// Parses a plain CIDR ("10.0.0.0/8"), masked.
    pub fn parse(s: &str) -> Result<IpPrefix, String> {
        let (addr, len) = s
            .split_once('/')
            .ok_or_else(|| format!("{s:?} is not a CIDR"))?;
        let addr: IpAddr = addr
            .parse()
            .map_err(|e| format!("{s:?} is not a CIDR: {e}"))?;
        let len: u8 = len
            .parse()
            .map_err(|e| format!("{s:?} is not a CIDR: {e}"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if len > max {
            return Err(format!(
                "{s:?} is not a CIDR: prefix length {len} out of range"
            ));
        }
        Ok(IpPrefix { addr, len }.masked())
    }

    /// Whether ip falls inside this prefix; an IPv4 prefix never contains an
    /// IPv6 address and vice versa, as with netip.
    pub fn contains(&self, ip: IpAddr) -> bool {
        // The other family first: its prefix lengths run past this one's
        // width, and masking with one of them is no comparison at all.
        if ip.is_ipv4() != self.addr.is_ipv4() {
            return false;
        }
        IpPrefix {
            addr: ip,
            len: self.len,
        }
        .masked()
        .addr
            == self.addr
    }
}

/// Refuses a policy naming a Repository in a spelling no module's location
/// ever has: the runtime reads every reference's registry into its
/// canonical form (location.rs), so a rule over another spelling would
/// match nothing - loudly for a permit, silently for a requireSignature or
/// a forbid. The names are the entity literals of the policies' stable
/// JSON (EST) form, in the scope and in the conditions alike; Cedar has no
/// way to build an entity from a string, so a literal is the only way a
/// policy names one. The first offender in name order, so the refusal does
/// not depend on the order of the rules.
fn check_repository_names(policy: &PolicySet) -> Result<(), String> {
    let mut names = std::collections::BTreeSet::new();
    let mut read = |est: Result<Value, String>| -> Result<(), String> {
        collect_repository_names(&est?, &mut names);
        Ok(())
    };
    for p in policy.policies() {
        read(p.to_json().map_err(|e| format!("cannot read policy: {e}")))?;
    }
    for t in policy.templates() {
        read(t.to_json().map_err(|e| format!("cannot read policy: {e}")))?;
    }
    for name in names {
        match crate::location::canonical_repository_name(&name) {
            Ok(canonical) if canonical == name => {}
            Ok(canonical) => {
                return Err(format!(
                    r#"Repository::{name:?} is not the canonical name of that repository; write Repository::{canonical:?}"#
                ));
            }
            Err(registry) => {
                return Err(format!(
                    r#"Repository::{name:?} does not name a repository: registry {registry:?} is not a valid registry host (a host name or an IP address with an optional port, no user information)"#
                ));
            }
        }
    }
    Ok(())
}

/// Collects the id of every Repository entity reference under an EST node:
/// {"type": "Repository", "id": "..."}, the shape of an entity in a scope
/// ("entity", "entities") and of a literal in an expression ("__entity").
fn collect_repository_names(node: &Value, names: &mut std::collections::BTreeSet<String>) {
    match node {
        Value::Object(fields) => {
            if fields.len() == 2
                && fields.get("type").and_then(Value::as_str) == Some("Repository")
                && let Some(id) = fields.get("id").and_then(Value::as_str)
            {
                names.insert(id.to_string());
                return;
            }
            for child in fields.values() {
                collect_repository_names(child, names);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_repository_names(child, names);
            }
        }
        _ => {}
    }
}

/// The policy set with one more rule, a permit of everything, when a forbid
/// in it leaves its action unconstrained, and None otherwise. Judged under
/// it, an action no rule names is refused exactly where such a forbid
/// matches: the forbid applies without turning the layer default-deny for
/// that action, which naming the action does.
fn open_under_forbids(policy: &PolicySet) -> Result<Option<PolicySet>, String> {
    let unconstrained = policy.policies().any(|p| {
        p.effect() == cedar_policy::Effect::Forbid
            && p.to_json().is_ok_and(|v| v["action"]["op"] == "All")
    });
    if !unconstrained {
        return Ok(None);
    }
    let permit = Policy::parse(
        Some(cedar_policy::PolicyId::new(
            "guestfn.permit-what-no-forbid-matches",
        )),
        "permit (principal, action, resource);",
    )
    .map_err(|e| e.to_string())?;
    let mut open = policy.clone();
    open.add(permit).map_err(|e| e.to_string())?;
    Ok(Some(open))
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
/// policy with an unrecognized shape is an error, whose wording the
/// conformance goldens pin.
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

/// A policy set as the layers judge under it: the policies and the number
/// that tells this set's decisions from every other's in the memo. The
/// number is drawn once per set and never reused, so a set compiled later -
/// another Input's compositionPolicy, a layer's forbids-only variant - can
/// never read a decision made under this one.
#[derive(Debug)]
struct Judged {
    policies: PolicySet,
    id: u64,
}

impl Judged {
    fn new(policies: PolicySet) -> Judged {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Judged {
            policies,
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }
}

impl std::ops::Deref for Judged {
    type Target = PolicySet;

    fn deref(&self) -> &PolicySet {
        &self.policies
    }
}

/// The decisions the policy sets have already given. A decision is a pure
/// function of the policy set and of what the question hands Cedar - the
/// principal and its attributes, the action, the resource, the entities and
/// the context - and the key is exactly that, written out whole: the set's
/// number and the JSON of every other argument of `decide_raw`. Nothing is
/// summarised or hashed away, so two questions share an entry only when
/// Cedar would be asked the same thing of the same policies.
///
/// Bounded in two generations: a new decision goes into the recent one,
/// which becomes the older one when full, dropping what was there; a hit
/// in the older one moves the decision back. What a busy composition asks
/// on every reconcile therefore stays, and what one caller asked once ages
/// out, whatever the number of namespaces, kinds or names. A question
/// whose key is longer than MAX_KEY is answered and not kept, so a module
/// cannot size the memo with what it writes in a response.
#[derive(Default)]
struct Decisions {
    recent: HashMap<String, bool>,
    older: HashMap<String, bool>,
}

impl Decisions {
    /// The entries of one generation; the memo holds at most twice that.
    const GENERATION: usize = 8192;
    /// The longest key kept, in bytes.
    const MAX_KEY: usize = 1024;

    fn get(&mut self, key: &str) -> Option<bool> {
        if let Some(decision) = self.recent.get(key) {
            return Some(*decision);
        }
        let (key, decision) = self.older.remove_entry(key)?;
        self.put(key, decision);
        Some(decision)
    }

    fn put(&mut self, key: String, decision: bool) {
        if key.len() > Self::MAX_KEY {
            return;
        }
        if self.recent.len() >= Self::GENERATION {
            self.older = std::mem::take(&mut self.recent);
        }
        self.recent.insert(key, decision);
    }
}

fn decisions() -> std::sync::MutexGuard<'static, Decisions> {
    static DECISIONS: OnceLock<Mutex<Decisions>> = OnceLock::new();
    DECISIONS
        .get_or_init(Default::default)
        .lock()
        .expect("poisoned")
}

/// Evaluates one request with the Request::"self" principal carrying the
/// caller's attributes - the evaluation both layers share, so a policy means
/// the same in both.
fn decide(
    ps: &Judged,
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
        ("Request", "self"),
        Some(principal_entity),
        action,
        resource,
        entities,
        context,
    )
}

/// One decision, from the memo when this set was already asked the same
/// question. Every argument but the set goes into the key as it is given;
/// the set is in it by its number.
fn decide_raw(
    ps: &Judged,
    principal: (&str, &str),
    principal_entity: Option<Value>,
    action: &str,
    resource: Value,
    entities: Vec<Value>,
    context: Value,
) -> bool {
    let key = decision_key(
        ps.id,
        principal,
        &principal_entity,
        action,
        &resource,
        &entities,
        &context,
    );
    if let Some(decision) = decisions().get(&key) {
        return decision;
    }
    // Evaluated outside the lock: two requests asking the same new
    // question both evaluate it, and both write the same answer.
    let decision = evaluate(
        &ps.policies,
        principal,
        principal_entity,
        action,
        resource,
        entities,
        context,
    );
    decisions().put(key, decision);
    decision
}

/// A question as the memo keys it: a JSON array of the set's number and
/// every argument, in order. JSON is unambiguous, so two different
/// questions never write the same key.
fn decision_key(
    set: u64,
    principal: (&str, &str),
    principal_entity: &Option<Value>,
    action: &str,
    resource: &Value,
    entities: &[Value],
    context: &Value,
) -> String {
    serde_json::to_string(&(
        set,
        principal,
        principal_entity,
        action,
        resource,
        entities,
        context,
    ))
    .expect("JSON values serialize")
}

fn evaluate(
    ps: &PolicySet,
    principal: (&str, &str),
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
        uid(principal.0, principal.1),
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

/// The Kind entity for a composed resource and the store giving it its one
/// ancestor, its group: `resource in Group::"apps"` is true for every kind
/// of the apps group and for no other, since the hierarchy is built from
/// the parsed group rather than a string prefix.
fn kind_entities(group: &str, kind: &str) -> (Value, Vec<Value>) {
    let group_uid = uid_json("Group", group);
    let resource = uid_json("Kind", &format!("{group}/{kind}"));
    let entities = vec![
        flat_entity(&group_uid),
        json!({ "uid": resource, "attrs": {}, "parents": [group_uid] }),
    ];
    (resource, entities)
}

fn compose_context(c: &Change) -> Value {
    json!({ "change": c.change.as_str(), "name": c.name, "target": c.target() })
}

/// The requireResource context: `name` for a match by name, `labels` (a
/// record keyed by label) for a match by labels, `namespace` where the
/// selector sets one - each absent otherwise, so a rule tests it with `has`.
fn require_context(r: &Requirement) -> Value {
    let mut ctx = serde_json::Map::new();
    match &r.matches {
        RequirementMatch::Name(name) => {
            ctx.insert("name".to_string(), json!(name));
        }
        RequirementMatch::Labels(labels) => {
            ctx.insert("labels".to_string(), json!(labels));
        }
        RequirementMatch::None => {}
    }
    if let Some(ns) = &r.namespace {
        ctx.insert("namespace".to_string(), json!(ns));
    }
    Value::Object(ctx)
}

impl std::fmt::Display for IpPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.len)
    }
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

    fn change(group: &str, kind: &str, change: ChangeKind, name: &str) -> Change {
        Change {
            group: group.to_string(),
            kind: kind.to_string(),
            change,
            name: name.to_string(),
            composite: false,
        }
    }

    #[test]
    fn compose_resource_hierarchy_is_group_correct() {
        let p = composition(
            r#"permit (principal, action == Action::"composeResource", resource in Group::"policy")
               when { context.change == "add" };
               permit (principal, action == Action::"composeResource", resource == Kind::"core/ConfigMap");"#,
        );
        let principal = Principal::default();
        let cases = [
            (
                change("policy", "PodDisruptionBudget", ChangeKind::Add, "pdb"),
                true,
            ),
            (
                change("policy", "PodDisruptionBudget", ChangeKind::Modify, "pdb"),
                false,
            ),
            (
                change("policy.example.io", "Thing", ChangeKind::Add, "t"),
                false,
            ),
            (change("core", "ConfigMap", ChangeKind::Remove, "cm"), true),
            (change("core", "Secret", ChangeKind::Add, "s"), false),
            (change("apps", "Deployment", ChangeKind::Add, "web"), false),
        ];
        for (c, want) in cases {
            assert_eq!(p.permits_compose(&principal, &c), want, "{c:?}");
        }
        assert!(p.scopes_action(ACTION_COMPOSE_RESOURCE));
    }

    #[test]
    fn compose_resource_context_narrows_by_name_and_target() {
        let p = composition(
            r#"permit (principal, action == Action::"composeResource", resource)
               when { context.target == "composite" || context.name like "addon-*" };"#,
        );
        let principal = Principal::default();
        let mut composite = change("platform.example.org", "WebApp", ChangeKind::Modify, "");
        composite.composite = true;
        assert!(p.permits_compose(&principal, &composite));
        assert!(p.permits_compose(
            &principal,
            &change("apps", "Deployment", ChangeKind::Add, "addon-pdb")
        ));
        assert!(!p.permits_compose(
            &principal,
            &change("apps", "Deployment", ChangeKind::Add, "deployment")
        ));
    }

    #[test]
    fn a_forbid_alone_scopes_compose_resource_and_refuses_every_change() {
        let p = composition(
            r#"forbid (principal, action == Action::"composeResource", resource == Kind::"core/Secret");"#,
        );
        assert!(p.scopes_action(ACTION_COMPOSE_RESOURCE));
        let principal = Principal::default();
        assert!(!p.permits_compose(&principal, &change("core", "Secret", ChangeKind::Add, "s")));
        assert!(!p.permits_compose(
            &principal,
            &change("apps", "Deployment", ChangeKind::Add, "d")
        ));
    }

    #[test]
    fn a_forbid_of_any_action_applies_to_what_no_rule_names() {
        let doc =
            r#"forbid (principal, action, resource) when { resource == Kind::"core/Secret" };"#;
        let principal = Principal::default();
        let secret = change("core", "Secret", ChangeKind::Add, "s");
        let deployment = change("apps", "Deployment", ChangeKind::Add, "d");
        let wants_secret = requirement("core", "Secret", labels(&[("a", "b")]));
        let wants_config = requirement("core", "ConfigMap", labels(&[("a", "b")]));

        // The layer judges both fences, permits what the forbid does not
        // match and stays default-permit for the rest.
        let comp = composition(doc);
        assert!(comp.scopes_action(ACTION_COMPOSE_RESOURCE));
        assert!(comp.scopes_action(ACTION_REQUIRE_RESOURCE));
        assert!(!comp.scopes_action(ACTION_PULL_MODULE));
        assert!(!comp.permits_compose(&principal, &secret));
        assert!(comp.permits_compose(&principal, &deployment));
        assert!(!comp.permits_require(&principal, &wants_secret));
        assert!(comp.permits_require(&principal, &wants_config));
        assert!(comp.permits_private_tmp(&principal));
        assert!(comp.permits_env(&principal, &["TOKEN".to_string()]));
        // The fences of a composite-chosen source stay default-deny.
        assert!(!comp.permits_pull_module(&principal, "ghcr.io/acme/x"));
        assert!(!comp.permits_spend_credential(&principal, "pull", "ghcr.io/acme/x"));

        let operator = OperatorPolicy::new("test", doc).expect("policy");
        assert!(operator.scopes_compose_resource());
        assert!(operator.scopes_require_resource());
        assert!(!operator.permits_compose(&principal, &secret));
        assert!(operator.permits_compose(&principal, &deployment));
        assert!(!operator.permits_require(&principal, &wants_secret));
        assert!(operator.permits_require(&principal, &wants_config));
        // A capability is still granted by a permit alone.
        assert!(!operator.permits_private_tmp(&principal));
    }

    #[test]
    fn a_forbid_of_any_action_narrows_the_sandbox_actions() {
        let comp = composition(
            r#"forbid (principal, action, resource) when { resource == Credential::"cmdb" };"#,
        );
        let principal = Principal::default();
        assert!(comp.scopes_action(ACTION_SPEND_CREDENTIAL));
        assert!(!comp.permits_spend_credential(&principal, "cmdb", ""));
        assert!(comp.permits_spend_credential(&principal, "other", ""));
    }

    #[test]
    fn a_rule_naming_the_action_decides_beside_a_forbid_of_any() {
        // Named, the action needs a permit; the forbid of any action still
        // wins over it.
        let comp = composition(
            r#"forbid (principal, action, resource) when { resource == Kind::"core/Secret" };
               permit (principal, action == Action::"composeResource", resource in Group::"core");"#,
        );
        let principal = Principal::default();
        assert!(!comp.permits_compose(&principal, &change("core", "Secret", ChangeKind::Add, "s")));
        assert!(comp.permits_compose(
            &principal,
            &change("core", "ConfigMap", ChangeKind::Add, "c")
        ));
        assert!(!comp.permits_compose(
            &principal,
            &change("apps", "Deployment", ChangeKind::Add, "d")
        ));
    }

    #[test]
    fn a_permit_of_any_action_scopes_nothing() {
        let comp = composition("permit (principal, action, resource);");
        assert!(!comp.scopes_action(ACTION_COMPOSE_RESOURCE));
        assert!(!comp.scopes_action(ACTION_SET_ENV));
        let operator =
            OperatorPolicy::new("test", "permit (principal, action, resource);").expect("policy");
        assert!(!operator.scopes_compose_resource());
    }

    #[test]
    fn the_operator_policy_scopes_compose_resource_only_when_a_rule_names_it() {
        let unscoped = OperatorPolicy::new(
            "test",
            r#"permit (principal, action == Action::"usePrivateTmp", resource);"#,
        )
        .expect("policy");
        assert!(!unscoped.scopes_compose_resource());
        let ceiling = OperatorPolicy::new(
            "test",
            r#"forbid (principal, action == Action::"composeResource", resource)
               when { resource in Group::"rbac.authorization.k8s.io" || resource == Kind::"core/Secret" };
               permit (principal, action == Action::"composeResource", resource);"#,
        )
        .expect("policy");
        assert!(ceiling.scopes_compose_resource());
        let principal = Principal::default();
        assert!(ceiling.permits_compose(
            &principal,
            &change("apps", "Deployment", ChangeKind::Add, "d")
        ));
        assert!(
            !ceiling.permits_compose(&principal, &change("core", "Secret", ChangeKind::Add, "s"))
        );
        assert!(!ceiling.permits_compose(
            &principal,
            &change(
                "rbac.authorization.k8s.io",
                "RoleBinding",
                ChangeKind::Modify,
                "rb"
            )
        ));
    }

    fn requirement(group: &str, kind: &str, matches: RequirementMatch) -> Requirement {
        Requirement {
            group: group.to_string(),
            kind: kind.to_string(),
            matches,
            namespace: None,
            field: "resources".to_string(),
            key: "r".to_string(),
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> RequirementMatch {
        RequirementMatch::Labels(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn require_resource_hierarchy_is_group_correct() {
        let p = composition(
            r#"permit (principal, action == Action::"requireResource", resource in Group::"apiextensions.crossplane.io");
               permit (principal, action == Action::"requireResource", resource == Kind::"core/ConfigMap");"#,
        );
        assert!(p.scopes_action(ACTION_REQUIRE_RESOURCE));
        let principal = Principal::default();
        let by_name = |g: &str, k: &str| requirement(g, k, RequirementMatch::Name("x".to_string()));
        for (r, want) in [
            (
                by_name("apiextensions.crossplane.io", "EnvironmentConfig"),
                true,
            ),
            (by_name("apiextensions.crossplane.io", "Usage"), true),
            (
                by_name("apiextensions.crossplane.io.example", "Thing"),
                false,
            ),
            (by_name("core", "ConfigMap"), true),
            (
                requirement("core", "ConfigMap", RequirementMatch::None),
                true,
            ),
            (by_name("core", "Secret"), false),
        ] {
            assert_eq!(p.permits_require(&principal, &r), want, "{r:?}");
        }
    }

    #[test]
    fn require_resource_context_is_present_only_where_the_selector_sets_it() {
        let p = composition(
            r#"permit (principal, action == Action::"requireResource", resource)
               when { context has name && context.name like "team-a-*" };
               permit (principal, action == Action::"requireResource", resource)
               when { context has labels && context.labels has team && context.labels.team == "a" };
               permit (principal, action == Action::"requireResource", resource)
               when { context has labels && context.labels["app.kubernetes.io/part-of"] == "shop" };
               permit (principal, action == Action::"requireResource", resource == Kind::"core/ConfigMap")
               when { context has namespace && context.namespace == principal.namespace };"#,
        );
        let team_a = Principal {
            namespace: "team-a".to_string(),
            ..Default::default()
        };
        let mut cases = vec![
            (
                requirement(
                    "core",
                    "Secret",
                    RequirementMatch::Name("team-a-db".to_string()),
                ),
                true,
            ),
            (
                requirement(
                    "core",
                    "Secret",
                    RequirementMatch::Name("team-b-db".to_string()),
                ),
                false,
            ),
            (
                requirement("core", "Secret", labels(&[("team", "a")])),
                true,
            ),
            (
                requirement("core", "Secret", labels(&[("team", "b")])),
                false,
            ),
            (
                requirement(
                    "core",
                    "Secret",
                    labels(&[("app.kubernetes.io/part-of", "shop")]),
                ),
                true,
            ),
            // No labels at all: the `has labels` guards fail, the rules do not error.
            (requirement("core", "Secret", labels(&[])), false),
            (requirement("core", "Secret", RequirementMatch::None), false),
        ];
        let mut own = requirement("core", "ConfigMap", RequirementMatch::None);
        own.namespace = Some("team-a".to_string());
        let mut other = own.clone();
        other.namespace = Some("team-b".to_string());
        cases.push((own, true));
        cases.push((other, false));
        cases.push((
            requirement("core", "ConfigMap", RequirementMatch::None),
            false,
        ));
        for (r, want) in cases {
            assert_eq!(p.permits_require(&team_a, &r), want, "{r:?}");
        }
    }

    #[test]
    fn a_forbid_alone_scopes_require_resource_and_refuses_every_selector() {
        let p = composition(
            r#"forbid (principal, action == Action::"requireResource", resource == Kind::"core/Secret");"#,
        );
        assert!(p.scopes_action(ACTION_REQUIRE_RESOURCE));
        assert!(!p.scopes_action(ACTION_COMPOSE_RESOURCE));
        let principal = Principal::default();
        assert!(!p.permits_require(
            &principal,
            &requirement("core", "Secret", RequirementMatch::None)
        ));
        assert!(!p.permits_require(
            &principal,
            &requirement("core", "ConfigMap", RequirementMatch::None)
        ));
    }

    #[test]
    fn the_operator_policy_scopes_require_resource_only_when_a_rule_names_it() {
        let compose_only = OperatorPolicy::new(
            "test",
            r#"permit (principal, action == Action::"composeResource", resource);"#,
        )
        .expect("policy");
        assert!(compose_only.scopes_compose_resource());
        assert!(!compose_only.scopes_require_resource());
        assert!(!compose_only.scopes_action(ACTION_USE_PRIVATE_TMP));
        let ceiling = OperatorPolicy::new(
            "test",
            r#"forbid (principal, action == Action::"requireResource", resource == Kind::"core/Secret");
               permit (principal, action == Action::"requireResource", resource);"#,
        )
        .expect("policy");
        assert!(ceiling.scopes_require_resource());
        assert!(!ceiling.scopes_compose_resource());
        let principal = Principal::default();
        assert!(ceiling.permits_require(
            &principal,
            &requirement(
                "apiextensions.crossplane.io",
                "EnvironmentConfig",
                RequirementMatch::Name("e".to_string())
            )
        ));
        assert!(!ceiling.permits_require(
            &principal,
            &requirement("core", "Secret", labels(&[("team", "a")]))
        ));
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
    fn a_policy_names_a_repository_canonically_or_is_refused() {
        fn refusal(written: &str, canonical: &str) -> Option<String> {
            Some(format!(
                r#"Repository::"{written}" is not the canonical name of that repository; write Repository::"{canonical}""#
            ))
        }
        let cases: Vec<(&str, Option<String>)> = vec![
            ("ghcr.io/acme/prod", None),
            ("ghcr.io/acme/", None),
            ("ghcr.io", None),
            ("index.docker.io/library", None),
            ("registry-1.docker.io/acme", None),
            // An explicit port other than the default is part of the name.
            ("registry.e2e.svc:5000/e2e", None),
            ("ghcr.io:8443/acme", None),
            ("localhost:5000", None),
            ("127.0.0.1:5000/example", None),
            // An HTTP location is not a registry reference: untouched.
            ("https://example.com/modules", None),
            ("https://Example.COM:443/modules", None),
            ("http://localhost:8080/", None),
            // No registry part to read: untouched.
            ("acme/prod", None),
            ("GHCR.IO/acme/prod", refusal("GHCR.IO/acme/prod", "ghcr.io/acme/prod")),
            ("ghcr.io:443/acme/prod", refusal("ghcr.io:443/acme/prod", "ghcr.io/acme/prod")),
            ("ghcr.io./acme/prod", refusal("ghcr.io./acme/prod", "ghcr.io/acme/prod")),
            ("Ghcr.Io.:443/acme/", refusal("Ghcr.Io.:443/acme/", "ghcr.io/acme/")),
            ("GHCR.IO", refusal("GHCR.IO", "ghcr.io")),
            ("ghcr.io:443", refusal("ghcr.io:443", "ghcr.io")),
            ("localhost:80/example", refusal("localhost:80/example", "localhost/example")),
            ("[0:0:0:0:0:0:0:1]:5000/x", refusal("[0:0:0:0:0:0:0:1]:5000/x", "[::1]:5000/x")),
            ("docker.io/library", refusal("docker.io/library", "index.docker.io/library")),
            ("docker.io", refusal("docker.io", "index.docker.io")),
            (
                "user@ghcr.io/acme",
                Some(
                    r#"Repository::"user@ghcr.io/acme" does not name a repository: registry "user@ghcr.io" is not a valid registry host (a host name or an IP address with an optional port, no user information)"#
                        .to_string(),
                ),
            ),
        ];
        // Where a policy can name a repository: the resource scope (== and
        // in), a when and an unless condition, a set, and
        // spendCredential's context.
        let shapes = [
            r#"permit (principal, action == Action::"pullModule", resource in Repository::"NAME");"#,
            r#"forbid (principal, action == Action::"pullModule", resource == Repository::"NAME");"#,
            r#"permit (principal, action == Action::"pullModule", resource) when { resource in Repository::"NAME" };"#,
            r#"permit (principal, action == Action::"pullModule", resource) unless { resource in [Repository::"ghcr.io/other", Repository::"NAME"] };"#,
            r#"permit (principal, action == Action::"spendCredential", resource) when { context has repository && context.repository in Repository::"NAME" };"#,
        ];
        let operator_shapes = [
            r#"permit (principal, action == Action::"requireSignature", resource) when { resource in Repository::"NAME" };"#,
            r#"permit (principal, action == Action::"requireSignature", resource in Repository::"NAME");"#,
            r#"permit (principal, action == Action::"trustSigner", resource in Repository::"NAME") when { context.issuer == "https://issuer.example" };"#,
            r#"forbid (principal, action == Action::"spendCredential", resource) when { context has repository && context.repository == Repository::"NAME" };"#,
        ];
        for (name, want) in &cases {
            for shape in shapes {
                let doc = shape.replace("NAME", name);
                assert_eq!(CompositionPolicy::new(&doc).err(), want.clone(), "{doc}");
            }
            for shape in operator_shapes {
                let doc = shape.replace("NAME", name);
                assert_eq!(
                    OperatorPolicy::new("policy.cedar", &doc).err(),
                    want.as_ref()
                        .map(|e| format!("operator policy policy.cedar: {e}")),
                    "{doc}"
                );
            }
        }
        // A policy naming no repository, or another entity type by the
        // same text, is not this check's.
        assert!(
            OperatorPolicy::new(
                "policy.cedar",
                r#"permit (principal, action == Action::"grantEgress", resource in HostPattern::"GHCR.IO");"#
            )
            .is_ok()
        );
        // The first offender in name order, whatever the order of the rules.
        let two = r#"permit (principal, action == Action::"pullModule", resource in Repository::"ghcr.io:443/b");
                     permit (principal, action == Action::"pullModule", resource in Repository::"GHCR.IO/a");"#;
        assert_eq!(
            CompositionPolicy::new(two).err(),
            refusal("GHCR.IO/a", "ghcr.io/a")
        );
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
        assert!(!p.has_signer_rules());
    }

    #[test]
    fn operator_policy_trusts_signers_per_repository() {
        const ISSUER: &str = "https://token.actions.githubusercontent.com";
        const RELEASE: &str = "https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-pkg.yml@refs/tags/v0.6.0";
        let p = OperatorPolicy::new(
            "test",
            r#"permit (principal, action == Action::"trustSigner", resource in Repository::"ghcr.io/jonasz-lasut/wasmfn/examples")
               when { context.issuer == "https://token.actions.githubusercontent.com"
                      && context.identity like "https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-pkg.yml@refs/tags/v*" };"#,
        )
        .expect("compile");
        let module = "ghcr.io/jonasz-lasut/wasmfn/examples/pdb-addon";
        assert!(p.permits_signer(module, ISSUER, RELEASE));
        // Another branch, another issuer, another repository: refused.
        assert!(!p.permits_signer(
            module,
            ISSUER,
            "https://github.com/jonasz-lasut/function-wasm/.github/workflows/publish-pkg.yml@refs/heads/main"
        ));
        assert!(!p.permits_signer(module, "https://accounts.google.com", RELEASE));
        assert!(!p.permits_signer("ghcr.io/jonasz-lasut/other/pdb-addon", ISSUER, RELEASE));
        assert!(p.has_signer_rules());
        assert!(!p.has_signature_rules());
        // The trait the verifier consumes reads the same decision.
        use crate::cosign::{Signer, SignerPolicy};
        assert!(p.trusts_signer(
            module,
            &Signer {
                issuer: ISSUER.to_string(),
                identity: RELEASE.to_string()
            }
        ));
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

    fn operator(doc: &str) -> OperatorPolicy {
        OperatorPolicy::new("test", doc).expect("compile")
    }

    fn principal(namespace: &str, xr_kind: &str) -> Principal {
        Principal {
            namespace: namespace.to_string(),
            xr_kind: xr_kind.to_string(),
            composition: String::new(),
        }
    }

    /// A text being compiled holds back nobody but the requests carrying
    /// it: while one compile is held open, another text is compiled and
    /// then read back, and a second request for the slow text waits for
    /// that one compile.
    #[test]
    fn a_slow_compile_holds_back_its_own_text_alone() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc::channel;
        use std::time::Duration;

        const SLOW: &str = r#"permit (principal, action == Action::"setEnv", resource);"#;
        const OTHER: &str = r#"permit (principal, action == Action::"usePrivateTmp", resource);"#;
        let cache = Arc::new(CompositionCache::default());
        let compiles = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = channel::<()>();
        let (release_tx, release_rx) = channel::<()>();
        let slow = {
            let (cache, compiles) = (Arc::clone(&cache), Arc::clone(&compiles));
            std::thread::spawn(move || {
                cache.get(SLOW, |text| {
                    compiles.fetch_add(1, Ordering::SeqCst);
                    started_tx.send(()).expect("the test waits");
                    release_rx.recv().expect("the test releases");
                    CompositionPolicy::new(text)
                })
            })
        };
        started_rx.recv().expect("the slow compile starts");

        // Another text, compiled and read back beside it, on a thread so
        // that a cache that blocked it fails the test instead of hanging.
        let (done_tx, done_rx) = channel();
        let other = Arc::clone(&cache);
        std::thread::spawn(move || {
            let first = other.get(OTHER, CompositionPolicy::new);
            let again = other.get(OTHER, |_| panic!("compiled once"));
            let _ = done_tx.send((first, again));
        });
        let (first, again) = done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("another text is not held back by the slow compile");
        let (first, again) = (first.unwrap().unwrap(), again.unwrap().unwrap());
        assert!(Arc::ptr_eq(&first, &again), "one compiled policy per text");

        // The same text: no second compile, the first one's result.
        let joined = {
            let (cache, compiles) = (Arc::clone(&cache), Arc::clone(&compiles));
            std::thread::spawn(move || {
                cache.get(SLOW, |text| {
                    compiles.fetch_add(1, Ordering::SeqCst);
                    CompositionPolicy::new(text)
                })
            })
        };
        release_tx.send(()).expect("release the slow compile");
        let slow = slow.join().expect("join").unwrap().unwrap();
        let joined = joined.join().expect("join").unwrap().unwrap();
        assert!(Arc::ptr_eq(&slow, &joined));
        assert_eq!(compiles.load(Ordering::SeqCst), 1);
    }

    /// A refused text is refused for everyone and never compiled twice;
    /// the checks made on the way are the entry's own, and the map starts
    /// over at its cap.
    #[test]
    fn the_cache_keeps_refusals_and_stays_bounded() {
        let cache = CompositionCache::default();
        let unnormalised = r#"permit (principal, action == Action::"pullModule", resource in Repository::"GHCR.io/acme");"#;
        let refused = cache.get(unnormalised, CompositionPolicy::new).unwrap_err();
        assert_eq!(
            cache
                .get(unnormalised, |_| panic!("a refusal is kept"))
                .unwrap_err(),
            refused
        );
        assert_eq!(
            refused,
            CompositionPolicy::new(unnormalised).unwrap_err(),
            "what the compile refuses is what the cache answers"
        );
        assert!(
            cache
                .get("", |_| panic!("no text, no layer"))
                .unwrap()
                .is_none()
        );

        // A refusal is an entry like any other, and costs no parse here.
        for i in 0..2 * CompositionCache::CAP {
            let refusal = cache.get(&format!("text {i}"), |text| Err(text.to_string()));
            assert_eq!(refusal.unwrap_err(), format!("text {i}"));
            assert!(cache.entries.lock().unwrap().len() <= CompositionCache::CAP);
        }
    }

    #[test]
    fn a_remembered_decision_stays_with_its_policy_set() {
        let question = |p: &OperatorPolicy| p.permits_private_tmp(&Principal::default());
        let permits =
            operator(r#"permit (principal, action == Action::"usePrivateTmp", resource);"#);
        let denies = operator(r#"permit (principal, action == Action::"setEnv", resource);"#);
        for _ in 0..3 {
            assert!(question(&permits));
            assert!(!question(&denies));
        }
        // The same text compiled again is another set, with its own answers.
        let again = operator(r#"permit (principal, action == Action::"usePrivateTmp", resource);"#);
        assert_ne!(permits.policy.id, again.policy.id);
        assert!(question(&again));
    }

    #[test]
    fn a_remembered_decision_is_not_another_callers() {
        let p = operator(
            r#"permit (principal, action == Action::"usePrivateTmp", resource)
               when { principal.namespace == "team-a" && principal.xrKind == "Bucket" };"#,
        );
        for _ in 0..3 {
            assert!(p.permits_private_tmp(&principal("team-a", "Bucket")));
            assert!(!p.permits_private_tmp(&principal("team-b", "Bucket")));
            assert!(!p.permits_private_tmp(&principal("team-a", "Database")));
            assert!(!p.permits_private_tmp(&Principal::default()));
        }
        // The composition's name is the principal's third attribute.
        let named = operator(
            r#"permit (principal, action == Action::"usePrivateTmp", resource)
               when { principal.composition == "buckets" };"#,
        );
        let mut caller = principal("team-a", "Bucket");
        for _ in 0..3 {
            caller.composition = "buckets".to_string();
            assert!(named.permits_private_tmp(&caller));
            caller.composition = "databases".to_string();
            assert!(!named.permits_private_tmp(&caller));
        }
    }

    #[test]
    fn a_remembered_decision_is_not_another_questions() {
        let p = operator(
            r#"permit (principal, action == Action::"setEnv", resource)
               when { context.keys.contains("TOKEN") };
               permit (principal, action == Action::"spendCredential", resource == Credential::"cmdb");
               permit (principal, action == Action::"grantEgress", resource in HostPattern::"example.org")
               when { context.method == "GET" && context.path like "/teams/*" };
               permit (principal, action == Action::"requireSignature", resource in Repository::"ghcr.io/acme");"#,
        );
        let caller = principal("team-a", "Bucket");
        let keys = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let egress = |host: &str, method: &str, path: &str| EgressGrant {
            host: host.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            ..Default::default()
        };
        for _ in 0..3 {
            assert!(p.permits_env(&caller, &keys(&["TOKEN"])));
            assert!(!p.permits_env(&caller, &keys(&["OTHER"])));
            assert!(p.permits_env(&caller, &keys(&["OTHER", "TOKEN"])));
            assert!(p.permits_spend_credential(&caller, "cmdb"));
            assert!(!p.permits_spend_credential(&caller, "cmdb2"));
            assert!(p.permits_egress(&caller, &egress("cmdb.example.org", "GET", "/teams/")));
            assert!(!p.permits_egress(&caller, &egress("cmdb.example.org", "POST", "/teams/")));
            assert!(!p.permits_egress(&caller, &egress("cmdb.example.org", "GET", "/admin/")));
            assert!(!p.permits_egress(&caller, &egress("cmdb.example.net", "GET", "/teams/")));
            assert!(p.requires_signature("ghcr.io/acme/fn"));
            assert!(!p.requires_signature("ghcr.io/acme-evil/fn"));
        }
    }

    /// Every question the layers ask, of a set that has answered it before
    /// and of one compiled for the question alone, which has no memory:
    /// the two must agree on every policy, caller and question, in whatever
    /// order they are asked.
    #[test]
    fn remembered_decisions_match_fresh_ones() {
        const POLICIES: &[&str] = &[
            r#"permit (principal, action, resource);"#,
            r#"permit (principal, action == Action::"usePrivateTmp", resource)
               when { principal.namespace == "team-a" };
               permit (principal, action == Action::"setEnv", resource)
               when { context.keys.containsAll(["A"]) && principal.xrKind == "Bucket" };
               permit (principal, action == Action::"spendCredential", resource == Credential::"a");
               permit (principal, action == Action::"grantEgress", resource in HostPattern::"example.org")
               when { context.method == "GET" };
               permit (principal, action == Action::"pullModule", resource in Repository::"ghcr.io/acme");
               permit (principal, action == Action::"requireSignature", resource in Repository::"ghcr.io/acme");
               permit (principal, action == Action::"trustSigner", resource in Repository::"ghcr.io/acme")
               when { context.issuer == "https://issuer.example" };
               permit (principal, action == Action::"composeResource", resource in Group::"apps")
               when { context.change == "add" && context.name like "web-*" };
               permit (principal, action == Action::"requireResource", resource == Kind::"core/Secret")
               when { context has name && context.name == "db" };"#,
            r#"forbid (principal, action, resource) when { principal.namespace == "team-b" };
               permit (principal, action == Action::"setEnv", resource);
               permit (principal, action == Action::"spendCredential", resource)
               when { context has repository && context.repository in Repository::"ghcr.io/acme" };"#,
            r#"forbid (principal, action == Action::"composeResource", resource in Group::"core")
               unless { principal.xrKind == "Database" };
               permit (principal, action == Action::"composeResource", resource);
               permit (principal, action == Action::"requireResource", resource)
               when { context has labels && context.labels has "team" && context has namespace && context.namespace == principal.namespace };"#,
        ];
        let callers = [
            Principal::default(),
            principal("team-a", "Bucket"),
            principal("team-a", "Database"),
            principal("team-b", "Bucket"),
        ];
        let keys = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let egress = |host: &str, pattern: &str, method: &str| EgressGrant {
            host: host.to_string(),
            host_pattern: pattern.to_string(),
            method: method.to_string(),
            path: "/".to_string(),
        };
        let requirement =
            |kind: &str, matches: RequirementMatch, namespace: Option<&str>| Requirement {
                group: "core".to_string(),
                kind: kind.to_string(),
                matches,
                namespace: namespace.map(str::to_string),
                field: "resources".to_string(),
                key: "k".to_string(),
            };
        type Question = Box<dyn Fn(&OperatorPolicy, &CompositionPolicy, &Principal) -> Vec<bool>>;
        let questions: Vec<Question> = vec![
            Box::new(|o, c, p| vec![o.permits_private_tmp(p), c.permits_private_tmp(p)]),
            Box::new(move |o, c, p| {
                let mut out = Vec::new();
                for k in [keys(&["A"]), keys(&["B"]), keys(&["A", "B"]), keys(&[])] {
                    out.push(o.permits_env(p, &k));
                    out.push(c.permits_env(p, &k));
                }
                out
            }),
            Box::new(|o, c, p| {
                let mut out = Vec::new();
                for name in ["a", "b", ""] {
                    out.push(o.permits_spend_credential(p, name));
                    for location in ["", "ghcr.io/acme/fn", "ghcr.io/other/fn"] {
                        out.push(c.permits_spend_credential(p, name, location));
                    }
                }
                out
            }),
            Box::new(move |o, c, p| {
                let mut out = Vec::new();
                for g in [
                    egress("api.example.org", "", "GET"),
                    egress("api.example.org", "", "POST"),
                    egress("", "*.example.org", "GET"),
                    egress("example.org.evil.net", "", "GET"),
                ] {
                    out.push(o.permits_egress(p, &g));
                    out.push(c.permits_egress(p, &g));
                }
                out
            }),
            Box::new(|o, c, p| {
                let mut out = Vec::new();
                for location in ["ghcr.io/acme/fn", "ghcr.io/acme-evil/fn", "ghcr.io/acme"] {
                    out.push(c.permits_pull_module(p, location));
                    out.push(o.requires_signature(location));
                    out.push(o.permits_signer(location, "https://issuer.example", "ci"));
                    out.push(o.permits_signer(location, "https://other.example", "ci"));
                }
                out
            }),
            Box::new(|o, c, p| {
                let mut out = Vec::new();
                for change in [
                    change("apps", "Deployment", ChangeKind::Add, "web-1"),
                    change("apps", "Deployment", ChangeKind::Modify, "web-1"),
                    change("apps", "Deployment", ChangeKind::Add, "db"),
                    change("core", "Secret", ChangeKind::Add, "web-1"),
                    change("core", "Secret", ChangeKind::Remove, "web-1"),
                ] {
                    out.push(o.permits_compose(p, &change));
                    out.push(c.permits_compose(p, &change));
                }
                out
            }),
            Box::new(move |o, c, p| {
                let labels = BTreeMap::from([("team".to_string(), "a".to_string())]);
                let mut out = Vec::new();
                for r in [
                    requirement("Secret", RequirementMatch::Name("db".to_string()), None),
                    requirement("Secret", RequirementMatch::Name("other".to_string()), None),
                    requirement("Secret", RequirementMatch::None, Some("team-a")),
                    requirement("ConfigMap", RequirementMatch::Labels(labels.clone()), None),
                    requirement(
                        "ConfigMap",
                        RequirementMatch::Labels(labels.clone()),
                        Some("team-a"),
                    ),
                ] {
                    out.push(o.permits_require(p, &r));
                    out.push(c.permits_require(p, &r));
                }
                out
            }),
        ];

        let shared: Vec<(OperatorPolicy, CompositionPolicy)> = POLICIES
            .iter()
            .map(|text| (operator(text), composition(text)))
            .collect();
        let mut asked = 0;
        let mut permitted = 0;
        // Three passes over every combination, each in another order, so a
        // remembered answer is read back between questions of every other
        // set and caller.
        for pass in 0..3usize {
            let mut order: Vec<(usize, usize, usize)> = Vec::new();
            for policy in 0..POLICIES.len() {
                for caller in 0..callers.len() {
                    for question in 0..questions.len() {
                        order.push((policy, caller, question));
                    }
                }
            }
            match pass {
                1 => order.reverse(),
                2 => order.sort_by_key(|(policy, caller, question)| (*question, *caller, *policy)),
                _ => {}
            }
            for (policy, caller, question) in order {
                let (o, c) = &shared[policy];
                let remembered = questions[question](o, c, &callers[caller]);
                let fresh = questions[question](
                    &operator(POLICIES[policy]),
                    &composition(POLICIES[policy]),
                    &callers[caller],
                );
                assert_eq!(
                    remembered, fresh,
                    "pass {pass}, policy {policy}, caller {caller}, question {question}"
                );
                asked += remembered.len();
                permitted += remembered.iter().filter(|d| **d).count();
            }
        }
        // The corpus exercises both answers.
        assert!(permitted > asked / 10, "{permitted} of {asked} permitted");
        assert!(
            permitted < asked * 9 / 10,
            "{permitted} of {asked} permitted"
        );
    }

    #[test]
    fn a_key_tells_every_argument_apart() {
        let base = || {
            decision_key(
                7,
                ("Request", "self"),
                &Some(json!({"attrs": {"namespace": "a"}})),
                "setEnv",
                &uid_json("Capability", "env"),
                &[flat_entity(&uid_json("Capability", "env"))],
                &json!({"keys": ["A"]}),
            )
        };
        let variants = [
            decision_key(
                8,
                ("Request", "self"),
                &Some(json!({"attrs": {"namespace": "a"}})),
                "setEnv",
                &uid_json("Capability", "env"),
                &[flat_entity(&uid_json("Capability", "env"))],
                &json!({"keys": ["A"]}),
            ),
            decision_key(
                7,
                ("Module", "module"),
                &Some(json!({"attrs": {"namespace": "a"}})),
                "setEnv",
                &uid_json("Capability", "env"),
                &[flat_entity(&uid_json("Capability", "env"))],
                &json!({"keys": ["A"]}),
            ),
            decision_key(
                7,
                ("Request", "self"),
                &Some(json!({"attrs": {"namespace": "b"}})),
                "setEnv",
                &uid_json("Capability", "env"),
                &[flat_entity(&uid_json("Capability", "env"))],
                &json!({"keys": ["A"]}),
            ),
            decision_key(
                7,
                ("Request", "self"),
                &None,
                "setEnv",
                &uid_json("Capability", "env"),
                &[flat_entity(&uid_json("Capability", "env"))],
                &json!({"keys": ["A"]}),
            ),
            decision_key(
                7,
                ("Request", "self"),
                &Some(json!({"attrs": {"namespace": "a"}})),
                "usePrivateTmp",
                &uid_json("Capability", "env"),
                &[flat_entity(&uid_json("Capability", "env"))],
                &json!({"keys": ["A"]}),
            ),
            decision_key(
                7,
                ("Request", "self"),
                &Some(json!({"attrs": {"namespace": "a"}})),
                "setEnv",
                &uid_json("Capability", "privateTmp"),
                &[flat_entity(&uid_json("Capability", "env"))],
                &json!({"keys": ["A"]}),
            ),
            decision_key(
                7,
                ("Request", "self"),
                &Some(json!({"attrs": {"namespace": "a"}})),
                "setEnv",
                &uid_json("Capability", "env"),
                &[],
                &json!({"keys": ["A"]}),
            ),
            decision_key(
                7,
                ("Request", "self"),
                &Some(json!({"attrs": {"namespace": "a"}})),
                "setEnv",
                &uid_json("Capability", "env"),
                &[flat_entity(&uid_json("Capability", "env"))],
                &json!({"keys": ["A", "B"]}),
            ),
        ];
        assert_eq!(base(), base());
        let distinct: std::collections::BTreeSet<&String> = variants.iter().collect();
        assert_eq!(distinct.len(), variants.len());
        for v in &variants {
            assert_ne!(*v, base());
        }
        // A value cannot pose as two: the quotes and commas a string holds
        // are escaped, so no argument reads into its neighbour.
        assert_ne!(
            decision_key(7, ("a\",\"b", "c"), &None, "x", &json!(1), &[], &json!(2)),
            decision_key(7, ("a", "b\",\"c"), &None, "x", &json!(1), &[], &json!(2)),
        );
    }

    #[test]
    fn the_memo_is_bounded_and_keeps_what_is_asked_again() {
        let mut memo = Decisions::default();
        let key = |i: usize| format!("question-{i}");
        memo.put("busy".to_string(), true);
        for i in 0..Decisions::GENERATION * 5 {
            memo.put(key(i), i.is_multiple_of(2));
            // Asked on every reconcile, so it survives every turnover.
            assert_eq!(memo.get("busy"), Some(true));
            assert!(memo.recent.len() <= Decisions::GENERATION);
            assert!(memo.older.len() <= Decisions::GENERATION);
        }
        // The last generation and a half or so is there, with its answers.
        let last = Decisions::GENERATION * 5 - 1;
        assert_eq!(memo.get(&key(last)), Some(last.is_multiple_of(2)));
        assert_eq!(memo.get(&key(last - 1)), Some((last - 1).is_multiple_of(2)));
        // What was asked once, long ago, is gone.
        assert_eq!(memo.get(&key(0)), None);
        assert_eq!(memo.get("never asked"), None);
    }

    #[test]
    fn a_question_too_long_to_keep_is_answered_every_time() {
        let mut memo = Decisions::default();
        let long = "k".repeat(Decisions::MAX_KEY + 1);
        memo.put(long.clone(), true);
        assert_eq!(memo.get(&long), None);
        assert!(memo.recent.is_empty());
        let fits = "k".repeat(Decisions::MAX_KEY);
        memo.put(fits.clone(), true);
        assert_eq!(memo.get(&fits), Some(true));

        // Through a policy: a selector with more labels than a key holds
        // gets its own decision each time, and the right one.
        let p = operator(
            r#"permit (principal, action == Action::"requireResource", resource)
               when { context has labels && context.labels has "k0" };"#,
        );
        let labels = |n: usize| -> BTreeMap<String, String> {
            (0..n).map(|i| (format!("k{i}"), "v".repeat(64))).collect()
        };
        let selector = |labels: BTreeMap<String, String>| Requirement {
            group: "core".to_string(),
            kind: "Secret".to_string(),
            matches: RequirementMatch::Labels(labels),
            namespace: None,
            field: "resources".to_string(),
            key: "k".to_string(),
        };
        let mut without = labels(40);
        without.remove("k0");
        for _ in 0..3 {
            assert!(p.permits_require(&Principal::default(), &selector(labels(40))));
            assert!(!p.permits_require(&Principal::default(), &selector(without.clone())));
        }
    }
}
