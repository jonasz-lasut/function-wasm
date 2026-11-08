//! The Input's CRD, generated from input.rs. Crossplane never installs a
//! function's Input CRD, and the runtime enforces every rule itself; the
//! document under package/wasm/input/ rides in the package for `crossplane beta
//! validate` and editors, which read a Composition's inputs against it.
//! It used to be hand-maintained beside the types. Now `document` builds
//! it from the types' schema (schemars, the doc comments as descriptions,
//! the Kubernetes extensions beside the fields they apply to) and
//! tests/crd.rs holds the checked-in file to it, re-recorded with
//! UPDATE_GOLDENS=1 like the other goldens.

use schemars::generate::SchemaSettings;
use schemars::{Schema, SchemaGenerator, json_schema};
use serde_json::{Value, json};

use crate::input::{self, Input};

/// The CRD's path from the repository root.
pub const FILE: &str = "package/wasm/input/guestfn.crossplane.io_inputs.yaml";
/// The same file in the function-oci package (docs/one-pager-oci-runtime.md).
pub const OCI_FILE: &str = "package/oci/input/guestfn.crossplane.io_inputs.yaml";

/// Kubernetes' pattern for a quantity, as controller-gen writes it for a
/// resource.Quantity field.
const QUANTITY_PATTERN: &str = r"^(\+|-)?(([0-9]+(\.[0-9]*)?)|(\.[0-9]+))(([KMGTPE]i)|[numkMGTPE]|([eE](\+|-)?(([0-9]+(\.[0-9]*)?)|(\.[0-9]+))))?$";

/// The schema of a field the runtime passes through untouched: an object
/// with whatever keys the author wrote (`x-kubernetes-preserve-unknown-fields`,
/// which a structural schema needs to keep them).
pub fn preserve_unknown_fields(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type": "object", "x-kubernetes-preserve-unknown-fields": true})
}

/// The schema of a quantity an author may write as "128Mi" or as a bare
/// number of bytes (`x-kubernetes-int-or-string`, with no `type`, as
/// controller-gen writes an IntOrString).
pub fn int_or_string_quantity(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "anyOf": [{"type": "integer"}, {"type": "string"}],
        "pattern": QUANTITY_PATTERN,
        "x-kubernetes-int-or-string": true
    })
}

/// The CRD as a JSON value: apiextensions.k8s.io/v1, one served and stored
/// version, the schema from `Input` with every subschema inlined (a CRD
/// takes no `$ref`), `nullable` for the optional fields (OpenAPI 3.0's
/// spelling, which Kubernetes takes) and no titles.
pub fn document() -> Value {
    let settings = SchemaSettings::openapi3().with(|s| s.inline_subschemas = true);
    let mut schema = settings
        .into_generator()
        .into_root_schema_for::<Input>()
        .to_value();
    scrub(&mut schema);
    // The types decode every field with a default (admission, not serde,
    // refuses what is missing), so the required lists are the CRD's own:
    // what a document needs before admission can say anything useful.
    schema["required"] = json!(["module"]);
    schema["properties"]["module"]["required"] = json!(["type"]);
    schema["properties"]["module"]["properties"]["oci"]["required"] = json!(["ref"]);
    schema["properties"]["module"]["properties"]["http"]["required"] = json!(["digest", "url"]);
    let (group, version) = input::API_VERSION.split_once('/').expect("group/version");
    let properties = schema["properties"]
        .as_object_mut()
        .expect("an object schema");
    properties.insert(
        "kind".into(),
        json!({
            "description": "Input, the one kind of the group.",
            "type": "string"
        }),
    );
    properties.insert("metadata".into(), json!({"type": "object"}));
    json!({
        "apiVersion": "apiextensions.k8s.io/v1",
        "kind": "CustomResourceDefinition",
        "metadata": {"name": format!("inputs.{group}")},
        "spec": {
            "group": group,
            "names": {
                "categories": ["crossplane"],
                "kind": input::KIND,
                "listKind": "InputList",
                "plural": "inputs",
                "singular": "input"
            },
            "scope": "Namespaced",
            "versions": [{
                "name": version,
                "schema": {"openAPIV3Schema": schema},
                "served": true,
                "storage": true
            }]
        }
    })
}

/// The CRD as the YAML file under package/wasm/input/.
pub fn yaml() -> String {
    serde_saphyr::to_string(&document()).expect("a CRD serializes")
}

/// Drops what a CRD's structural schema does not take or need: the meta
/// schema, the titles schemars adds for every struct, the definitions map
/// (empty with every subschema inlined) and the defaults the types'
/// `#[serde(default)]` implies: the API server would otherwise write an
/// empty string into every absent field, and the CEL rules read `has()`.
fn scrub(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        o.remove("components");
        o.remove("default");
        for child in o.values_mut() {
            scrub(child);
        }
    } else if let Some(a) = v.as_array_mut() {
        for child in a {
            scrub(child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the hand-written CRD had and the generated one must keep: the
    /// identity, the required fields, the enum, the patterns, the CEL
    /// rules, the pass-through config and the int-or-string quantity.
    #[test]
    fn the_crd_keeps_the_rules_of_the_hand_written_one() {
        let crd = document();
        assert_eq!(crd["metadata"]["name"], "inputs.guestfn.crossplane.io");
        assert_eq!(crd["spec"]["group"], "guestfn.crossplane.io");
        let v = &crd["spec"]["versions"][0];
        assert_eq!(v["name"], "v1");
        let schema = &v["schema"]["openAPIV3Schema"];
        assert_eq!(schema["required"], json!(["module"]));
        assert!(schema["properties"]["kind"].is_object());
        assert!(schema["properties"]["metadata"].is_object());
        assert_eq!(
            schema["x-kubernetes-validations"].as_array().map(Vec::len),
            Some(1)
        );
        let module = &schema["properties"]["module"];
        assert_eq!(module["required"], json!(["type"]));
        assert_eq!(
            module["properties"]["type"]["enum"],
            json!(["OCI", "HTTP", "Path"])
        );
        assert_eq!(
            module["x-kubernetes-validations"].as_array().map(Vec::len),
            Some(5)
        );
        assert_eq!(module["properties"]["oci"]["required"], json!(["ref"]));
        assert_eq!(
            module["properties"]["http"]["required"],
            json!(["digest", "url"])
        );
        assert!(module["properties"]["oci"]["properties"]["ref"]["pattern"].is_string());
        let config = &schema["properties"]["config"];
        assert_eq!(config["x-kubernetes-preserve-unknown-fields"], true);
        assert!(
            config["description"].is_string(),
            "the doc comment is the description"
        );
        let memory = &schema["properties"]["limits"]["properties"]["memory"];
        assert_eq!(memory["x-kubernetes-int-or-string"], true);
        assert!(memory.get("type").is_none());
        assert_eq!(
            schema["properties"]["limits"]["properties"]["concurrency"]["minimum"],
            1
        );
        assert!(
            !crd.to_string().contains("$ref"),
            "every subschema is inlined"
        );
        assert!(
            !crd.to_string().contains("\"default\""),
            "no defaults: the CEL rules read has()"
        );
    }
}
