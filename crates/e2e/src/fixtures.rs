//! The Crossplane objects a test writes: the Guest XRD the suite installs
//! once, a Composition per test around one function-wasm step, the Guest
//! composite resource whose spec drives the test module, and the Secrets a
//! step reads. Mirrors of the test module's status types sit here too, so a
//! test reads the composite's status typed.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

pub const GROUP: &str = "e2e.function-wasm.io";
pub const API_VERSION: &str = "e2e.function-wasm.io/v1alpha1";
pub const KIND: &str = "Guest";
pub const PLURAL: &str = "guests";
pub const XRD_NAME: &str = "guests.e2e.function-wasm.io";
/// The label a test's cluster-scoped objects carry, naming its namespace,
/// so the namespace's teardown finds them.
pub const NAMESPACE_LABEL: &str = "e2e.function-wasm.io/namespace";
/// The step credential every Composition names for the module pull, and the
/// Secret in the test's namespace that backs it.
pub const PULL_CREDENTIAL: &str = "registry";
pub const PULL_SECRET: &str = "registry-pull";
pub const NOP_API_VERSION: &str = "nop.crossplane.io/v1alpha1";
pub const NOP_KIND: &str = "NopResource";
pub const ENVIRONMENT_CONFIG_API_VERSION: &str = "apiextensions.crossplane.io/v1beta1";
pub const ENVIRONMENT_CONFIG_KIND: &str = "EnvironmentConfig";

/// The Guest XRD: a namespaced Crossplane v2 composite whose spec is open
/// (the test module reads it) and whose status declares what the module
/// writes: Crossplane applies the status against the CRD's schema and
/// refuses a field it does not declare, keeping only `spec` open.
pub fn xrd() -> Value {
    json!({
        "apiVersion": "apiextensions.crossplane.io/v2",
        "kind": "CompositeResourceDefinition",
        "metadata": {"name": XRD_NAME},
        "spec": {
            "group": GROUP,
            "names": {"kind": KIND, "plural": PLURAL},
            "scope": "Namespaced",
            "versions": [{
                "name": "v1alpha1",
                "served": true,
                "referenceable": true,
                "schema": {"openAPIV3Schema": {
                    "type": "object",
                    "properties": {
                        "spec": {"type": "object", "x-kubernetes-preserve-unknown-fields": true},
                        "status": {"type": "object", "properties": {
                            "module": {"type": "string"},
                            "egress": {"type": "object", "properties": {
                                "status": {"type": "integer"},
                                "body": {"type": "string"},
                                "error": {"type": "string"},
                            }},
                            "env": {"type": "object", "additionalProperties": {"type": "string", "nullable": true}},
                            "credentials": {"type": "object", "additionalProperties": {"type": "array", "items": {"type": "string"}}},
                            "tmp": {"type": "object", "properties": {
                                "ok": {"type": "boolean"},
                                "error": {"type": "string"},
                            }},
                            "environmentConfig": {"type": "object", "x-kubernetes-preserve-unknown-fields": true},
                        }},
                    },
                }},
            }],
        },
    })
}

/// One function-wasm pipeline step: the Input and the step credentials.
#[derive(Debug, Clone)]
pub struct Step {
    pub module: Value,
    pub composition_policy: Option<String>,
    pub limits: Option<Value>,
    pub config: Option<Value>,
    /// (credential name, Secret in the test's namespace)
    pub credentials: Vec<(String, String)>,
}

impl Step {
    /// A module pulled by digest from the suite's registry with the pull
    /// credential every test namespace holds.
    pub fn oci(reference: &str) -> Step {
        Step {
            module: json!({"type": "OCI", "oci": {"ref": reference, "credentials": PULL_CREDENTIAL}}),
            composition_policy: None,
            limits: None,
            config: None,
            credentials: vec![(PULL_CREDENTIAL.to_string(), PULL_SECRET.to_string())],
        }
    }

    /// A public module, pulled anonymously.
    pub fn oci_public(reference: &str) -> Step {
        Step {
            module: json!({"type": "OCI", "oci": {"ref": reference}}),
            composition_policy: None,
            limits: None,
            config: None,
            credentials: Vec::new(),
        }
    }

    /// An XR-chosen OCI module (`module.from`), fenced by the policy given.
    pub fn from(field: &str, allow_empty: bool, composition_policy: &str) -> Step {
        Step {
            module: json!({"type": "OCI", "from": field, "allowEmpty": allow_empty}),
            composition_policy: Some(composition_policy.to_string()),
            limits: None,
            config: None,
            credentials: vec![(PULL_CREDENTIAL.to_string(), PULL_SECRET.to_string())],
        }
    }

    /// The Input's compositionPolicy for a static source: the composition
    /// author's narrowing layer (the output fence, a sandbox narrowing).
    pub fn with_composition_policy(mut self, policy: &str) -> Step {
        self.composition_policy = Some(policy.to_string());
        self
    }

    pub fn with_limits(mut self, limits: Value) -> Step {
        self.limits = Some(limits);
        self
    }

    pub fn with_credential(mut self, name: &str, secret: &str) -> Step {
        self.credentials
            .push((name.to_string(), secret.to_string()));
        self
    }

    fn input(&self) -> Value {
        let mut input = json!({
            "apiVersion": "wasm.fn.crossplane.io/v1",
            "kind": "Input",
            "module": self.module,
        });
        if let Some(p) = &self.composition_policy {
            input["compositionPolicy"] = Value::String(p.clone());
        }
        if let Some(l) = &self.limits {
            input["limits"] = l.clone();
        }
        if let Some(c) = &self.config {
            input["config"] = c.clone();
        }
        input
    }
}

/// A Composition of one step, for the Guests of one test namespace.
pub fn composition(name: &str, namespace: &str, step: &Step) -> Value {
    let credentials: Vec<Value> = step
        .credentials
        .iter()
        .map(|(credential, secret)| {
            json!({
                "name": credential,
                "source": "Secret",
                "secretRef": {"namespace": namespace, "name": secret},
            })
        })
        .collect();
    json!({
        "apiVersion": "apiextensions.crossplane.io/v1",
        "kind": "Composition",
        "metadata": {"name": name, "labels": {NAMESPACE_LABEL: namespace}},
        "spec": {
            "compositeTypeRef": {"apiVersion": API_VERSION, "kind": KIND},
            "mode": "Pipeline",
            "pipeline": [{
                "step": "guest",
                "functionRef": {"name": super::k8s::FUNCTION_NAME},
                "input": step.input(),
                "credentials": credentials,
            }],
        },
    })
}

/// A Guest composite resource selecting its Composition by name.
pub fn guest(namespace: &str, name: &str, composition: &str, spec: Value) -> Value {
    let mut spec = spec;
    spec["crossplane"] = json!({"compositionRef": {"name": composition}});
    json!({
        "apiVersion": API_VERSION,
        "kind": KIND,
        "metadata": {"name": name, "namespace": namespace},
        "spec": spec,
    })
}

pub fn secret(namespace: &str, name: &str, data: &[(&str, &str)]) -> Value {
    let string_data: BTreeMap<&str, &str> = data.iter().copied().collect();
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": name, "namespace": namespace},
        "type": "Opaque",
        "stringData": string_data,
    })
}

pub fn environment_config(name: &str, namespace: &str, data: Value) -> Value {
    json!({
        "apiVersion": ENVIRONMENT_CONFIG_API_VERSION,
        "kind": ENVIRONMENT_CONFIG_KIND,
        "metadata": {"name": name, "labels": {NAMESPACE_LABEL: namespace}},
        "data": data,
    })
}

/// What the test module wrote into the composite's status.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Status {
    pub module: String,
    pub egress: Option<EgressStatus>,
    pub env: Option<BTreeMap<String, Option<String>>>,
    pub credentials: Option<BTreeMap<String, Vec<String>>>,
    pub tmp: Option<TmpStatus>,
    pub environment_config: Option<Value>,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct EgressStatus {
    pub status: Option<u16>,
    pub body: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TmpStatus {
    pub ok: bool,
    pub error: Option<String>,
}
