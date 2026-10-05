//! Render the daemon RPC contract as an OpenRPC document.
//!
//! The source of truth is `zeroclaw-rpc-proto`: the method table, each
//! method's declared params/result shape, the notification names, the error
//! codes and the JSON Schema of every wire type it defines. The runtime
//! contributes one fact per method that only it knows, the authorization
//! classification. Runtime-owned types, which the proto crate lists in
//! `EXTERNAL_TYPES` because it cannot depend on the runtime, take their
//! schema from the runtime's own catalog and are tagged with their owning
//! crate. A contract type that neither catalog knows fails generation, so the
//! document never names a type it cannot describe.
//!
//! Framing is not a schema-language concern. The NDJSON envelope, the
//! handshake and the transport rules are documented in prose in
//! `docs/book/src/architecture/rpc-socket.md`; this document describes what
//! travels inside the envelope.

use anyhow::{Context, bail, ensure};
use schemars::{Schema, SchemaGenerator, generate::SchemaSettings};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use zeroclaw_rpc_proto::method::{EXTERNAL_TYPES, RUNTIME_DOCUMENT_TYPES};
use zeroclaw_rpc_proto::{Method, RPC_PROTOCOL_VERSION, Shape, error_codes, notification, schema};
use zeroclaw_runtime::rpc::dispatch::{MethodAuthz, MethodAuthzExt};

/// Tracked output. Lives next to the prose RPC page it complements.
pub const OUTPUT_FILE: &str = "docs/book/src/architecture/zeroclaw-rpc.openrpc.json";

const DEFINITIONS_PATH: &str = "#/components/schemas/";

fn workspace_root() -> PathBuf {
    crate::util::repo_root()
}

/// OpenRPC 1.3.2 schema objects are JSON Schema Draft 7. Schemars' Draft 7
/// settings rewrite the newer keywords (`prefixItems` for tuples,
/// `unevaluatedProperties`, `$ref` siblings) when the definitions are taken
/// with transforms applied, so every schema in the document is Draft 7.
pub const SCHEMA_DIALECT: &str = "http://json-schema.org/draft-07/schema#";

fn generator() -> SchemaGenerator {
    let mut settings = SchemaSettings::draft07();
    settings.definitions_path = DEFINITIONS_PATH.into();
    settings.inline_subschemas = false;
    settings.meta_schema = None;
    settings.into_generator()
}

/// The by-name parameter descriptors for a method's params shape.
///
/// OpenRPC names each content descriptor after the key the caller puts in
/// `params`, so a typed params struct is expanded into one descriptor per
/// property, with `required` taken from the schema. The struct's own schema
/// is still registered, so its description and nested references stay in
/// `components`. Shapes this document cannot describe field by field (a
/// free-form object, or a type another crate owns without an exported
/// schema) produce no descriptors and are marked in `x-zeroclaw-params`, so
/// a consumer never sees an invented `params` wrapper as a real key.
fn param_descriptors(
    schemas: &Map<String, Value>,
    shape: Shape,
) -> anyhow::Result<(Vec<Value>, Value)> {
    Ok(match shape {
        Shape::None => (Vec::new(), json!({ "shape": "none" })),
        Shape::Untyped => (
            Vec::new(),
            json!({
                "shape": "untyped",
                "description": "Free-form JSON object shaped by the daemon at runtime; keys are not enumerable here.",
            }),
        ),
        Shape::Typed(name) => match schemas.get(name) {
            Some(definition) => {
                let required: Vec<&str> = definition["required"]
                    .as_array()
                    .map(|r| r.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                // OpenRPC orders every required parameter before every
                // optional one; within each group the wire-key order is
                // alphabetical so the document stays deterministic.
                let mut props: Vec<(&String, &Value)> = definition["properties"]
                    .as_object()
                    .map(|props| props.iter().collect())
                    .unwrap_or_default();
                props
                    .sort_by_key(|(prop, _)| (!required.contains(&prop.as_str()), (*prop).clone()));
                let descriptors: Vec<Value> = props
                    .into_iter()
                    .map(|(prop, schema)| {
                        json!({
                            "name": prop,
                            "required": required.contains(&prop.as_str()),
                            "schema": schema,
                        })
                    })
                    .collect();
                let mut marker = json!({ "shape": "typed", "type": name });
                if descriptors.is_empty() {
                    marker["description"] = json!(format!(
                        "`{name}` is not a plain object; its schema is in components."
                    ));
                }
                if let Some(owner) = external_owner(name) {
                    marker["owner"] = json!(owner);
                }
                (descriptors, marker)
            }
            None => bail!("params type `{name}` has no schema in either catalog"),
        },
    })
}

/// The owning crate of a runtime-owned contract type, `None` for a type in
/// the proto catalog.
fn external_owner(name: &str) -> Option<&'static str> {
    EXTERNAL_TYPES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, owner)| *owner)
}

/// Schema for a contract type: the proto catalog first, then the runtime's
/// catalog for the names `EXTERNAL_TYPES` assigns to it.
fn subschema_for_contract(generator: &mut SchemaGenerator, name: &str) -> Option<Schema> {
    schema::subschema_for_named(generator, name)
        .or_else(|| zeroclaw_runtime::rpc::schema::subschema_for_named(generator, name))
}

/// Describe a result side: a schema reference, a free-form value, or
/// nothing.
fn shape_value(generator: &mut SchemaGenerator, shape: Shape) -> anyhow::Result<Option<Value>> {
    Ok(match shape {
        Shape::None => None,
        Shape::Untyped => Some(json!({
            "name": "value",
            "schema": { "description": "Free-form JSON shaped by the daemon at runtime." },
            "x-zeroclaw-shape": "untyped",
        })),
        Shape::Typed(name) => match subschema_for_contract(generator, name) {
            Some(schema) => {
                let mut value = json!({
                    "name": name,
                    "schema": schema,
                    "x-zeroclaw-shape": "typed",
                });
                if let Some(owner) = external_owner(name) {
                    value["x-zeroclaw-owner"] = json!(owner);
                }
                Some(value)
            }
            None => bail!("result type `{name}` has no schema in either catalog"),
        },
    })
}

fn authorization(method: Method) -> Value {
    match method.authz() {
        MethodAuthz::Handshake => json!({ "kind": "handshake" }),
        MethodAuthz::Requires(resource, verb) => json!({
            "kind": "grant",
            "resource": resource.to_string(),
            "verb": verb.to_string(),
        }),
    }
}

/// Build the whole document in memory.
pub fn render() -> anyhow::Result<String> {
    let mut generator = generator();

    // Pass one registers every typed shape with the generator (results as
    // `$ref` values, params by name); pass two, after the definitions have
    // been taken with the Draft 7 transforms applied, expands the params from
    // those transformed definitions so copied property schemas are Draft 7
    // too.
    let mut registered: Vec<(Method, &str, Shape, Value)> = Vec::with_capacity(Method::ALL.len());
    for (method, wire) in Method::ALL {
        let contract = method.contract();
        if let Shape::Typed(name) = contract.params {
            ensure!(
                subschema_for_contract(&mut generator, name).is_some(),
                "{wire}: params type `{name}` has no schema in either catalog"
            );
        }
        let result = shape_value(&mut generator, contract.result)
            .with_context(|| format!("{wire}: result"))?
            .map_or_else(
                || json!({ "name": "result", "schema": { "type": "null" } }),
                |mut r| {
                    if let Some(obj) = r.as_object_mut() {
                        obj.insert("name".into(), json!("result"));
                    }
                    r
                },
            );
        registered.push((*method, *wire, contract.params, result));
    }

    let notifications: Vec<Value> = notification::ALL
        .iter()
        .map(|(name, payload)| match payload {
            Some(payload) => {
                let schema = schema::subschema_for_named(&mut generator, payload).map_or_else(
                    || json!({ "description": format!("`{payload}` (no exported schema)") }),
                    Value::from,
                );
                json!({ "name": name, "params": { "name": "params", "schema": schema } })
            }
            None => json!({
                "name": name,
                "params": { "name": "params", "schema": { "type": "object", "description": "Untyped event object." } },
            }),
        })
        .collect();

    let error_table: Map<String, Value> = error_codes::ALL
        .iter()
        .map(|(name, code)| (code.to_string(), json!(name)))
        .collect();

    // Wire types that carry a runtime value as raw JSON reference its schema
    // by name; register those runtime types so the references resolve.
    for name in RUNTIME_DOCUMENT_TYPES {
        ensure!(
            subschema_for_contract(&mut generator, name).is_some(),
            "document type `{name}` has no schema in either catalog"
        );
    }

    let schemas = generator.take_definitions(true);

    let methods: Vec<Value> = registered
        .into_iter()
        .map(|(method, wire, params_shape, result)| {
            let mut entry = Map::new();
            entry.insert("name".into(), json!(wire));
            entry.insert("paramStructure".into(), json!("by-name"));
            let (params, params_marker) = param_descriptors(&schemas, params_shape)
                .with_context(|| format!("{wire}: params"))?;
            entry.insert("params".into(), Value::Array(params));
            entry.insert("x-zeroclaw-params".into(), params_marker);
            entry.insert("result".into(), result);
            entry.insert("x-zeroclaw-authorization".into(), authorization(method));
            Ok(Value::Object(entry))
        })
        .collect::<anyhow::Result<_>>()?;

    let document = json!({
        "openrpc": "1.3.2",
        "info": {
            "title": "ZeroClaw daemon RPC",
            "version": RPC_PROTOCOL_VERSION.to_string(),
            "description": "JSON-RPC 2.0 methods served by `zeroclaw daemon` over the local socket or named pipe, framed as newline-delimited JSON. Generated by `cargo generate openrpc` from `zeroclaw-rpc-proto`; do not edit by hand. Framing, the handshake and transport rules are described in `rpc-socket.md`.",
        },
        "x-zeroclaw": {
            "protocol_version": RPC_PROTOCOL_VERSION,
            "schema_dialect": SCHEMA_DIALECT,
            "framing": "ndjson",
            "transports": ["unix-socket", "windows-named-pipe", "wss"],
            "source": "crates/zeroclaw-rpc-proto",
            "generator": "cargo generate openrpc",
        },
        "methods": methods,
        "x-notifications": notifications,
        "x-error-codes": error_table,
        "components": { "schemas": schemas },
    });

    let mut rendered = serde_json::to_string_pretty(&document)?;
    rendered.push('\n');
    Ok(rendered)
}

/// Regenerate the tracked document, or fail when `check` finds drift.
pub fn run(check: bool) -> anyhow::Result<()> {
    let path = workspace_root().join(OUTPUT_FILE);
    let rendered = render()?;
    if check {
        let current = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "read {}; run `cargo generate openrpc` to create it",
                path.display()
            )
        })?;
        ensure!(
            current == rendered,
            "{} is out of date with zeroclaw-rpc-proto; run `cargo generate openrpc`",
            OUTPUT_FILE
        );
        println!("openrpc: {OUTPUT_FILE} is in sync");
        return Ok(());
    }
    std::fs::write(&path, rendered).with_context(|| format!("write {}", path.display()))?;
    println!("openrpc: wrote {OUTPUT_FILE}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_lists_every_method_once_and_is_deterministic() {
        let a = render().expect("render");
        let b = render().expect("render");
        assert_eq!(a, b, "rendering must be deterministic");
        let doc: Value = serde_json::from_str(&a).expect("valid JSON");
        let names: Vec<&str> = doc["methods"]
            .as_array()
            .expect("methods array")
            .iter()
            .map(|m| m["name"].as_str().expect("method name"))
            .collect();
        assert_eq!(names.len(), Method::ALL.len());
        for (_, wire) in Method::ALL {
            assert!(names.contains(wire), "{wire} missing from the document");
        }
        assert_eq!(
            doc["info"]["version"],
            json!(RPC_PROTOCOL_VERSION.to_string())
        );
    }

    /// A request built from the descriptors must be what the daemon parses:
    /// the descriptor names are the params keys, not a wrapper around them.
    #[test]
    fn descriptors_name_the_real_params_keys() {
        let doc: Value = serde_json::from_str(&render().expect("render")).expect("valid JSON");
        let methods = doc["methods"].as_array().expect("methods");
        let by_name = |wire: &str| {
            methods
                .iter()
                .find(|m| m["name"] == json!(wire))
                .unwrap_or_else(|| panic!("{wire} missing"))
        };
        // Every descriptor everywhere is a real property, never `params`.
        for m in methods {
            for p in m["params"].as_array().expect("params array") {
                assert_ne!(
                    p["name"],
                    json!("params"),
                    "{}: wrapper descriptor",
                    m["name"]
                );
                // A JSON Schema is an object or a boolean (`true` for a free
                // `serde_json::Value` property such as `clientCapabilities`).
                assert!(
                    p["schema"].is_object() || p["schema"].is_boolean(),
                    "{}: descriptor without schema",
                    m["name"]
                );
                assert!(
                    p["required"].is_boolean(),
                    "{}: descriptor without required",
                    m["name"]
                );
            }
            assert!(
                m["x-zeroclaw-params"]["shape"].is_string(),
                "{}: params shape marker missing",
                m["name"]
            );
        }
        // session/close: build the request the document describes and parse
        // it with the daemon's own type.
        let close = by_name("session/close");
        let names: Vec<&str> = close["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["session_id"]);
        assert_eq!(close["params"][0]["required"], json!(true));
        let request = json!({ "session_id": "s1" });
        let parsed: zeroclaw_rpc_proto::types::SessionIdParams =
            serde_json::from_value(request).expect("descriptor-shaped request parses");
        assert_eq!(parsed.session_id, "s1");
        // initialize: optional fields are optional, the version is required by default.
        let init = by_name("initialize");
        let init_params = init["params"].as_array().unwrap();
        let find = |n: &str| init_params.iter().find(|p| p["name"] == json!(n)).unwrap();
        assert_eq!(find("auth_token")["required"], json!(false));
        assert_eq!(find("protocol_version")["required"], json!(false));
        // Shapes this document cannot enumerate say so instead of inventing a key.
        for m in methods {
            let shape = m["x-zeroclaw-params"]["shape"].as_str().unwrap();
            if shape == "untyped" || shape == "external" || shape == "none" {
                assert!(m["params"].as_array().unwrap().is_empty(), "{}", m["name"]);
            }
        }
    }

    /// OpenRPC 1.3.2 schema objects are Draft 7: no keyword from a later
    /// dialect may appear anywhere in the document, or a Draft 7 consumer
    /// silently ignores it and accepts values the wire type rejects.
    /// Runtime-owned contract types are described, not stubbed: their
    /// params expand to real keys, their results carry a schema, and both
    /// name the owning crate.
    #[test]
    fn runtime_owned_types_carry_schemas_and_their_owner() {
        let doc: Value = serde_json::from_str(&render().expect("render")).expect("valid JSON");
        let methods = doc["methods"].as_array().expect("methods");
        for m in methods {
            assert_ne!(
                m["x-zeroclaw-params"]["shape"],
                json!("external"),
                "{}",
                m["name"]
            );
            assert_ne!(
                m["result"]["x-zeroclaw-shape"],
                json!("external"),
                "{}",
                m["name"]
            );
        }
        let by_name = |wire: &str| {
            methods
                .iter()
                .find(|m| m["name"] == json!(wire))
                .unwrap_or_else(|| panic!("{wire} missing"))
        };

        let add = by_name("cron/add");
        assert_eq!(add["x-zeroclaw-params"]["owner"], json!("zeroclaw-runtime"));
        assert_eq!(add["result"]["x-zeroclaw-owner"], json!("zeroclaw-runtime"));
        let mut request = Map::new();
        for p in add["params"].as_array().expect("cron/add params") {
            if p["required"] == json!(true) {
                request.insert(p["name"].as_str().unwrap().to_string(), json!("x"));
            }
        }
        assert!(request.contains_key("agent") && request.contains_key("schedule"));
        let parsed: zeroclaw_runtime::rpc::types::CronAddParams =
            serde_json::from_value(Value::Object(request))
                .expect("a request built from the required descriptors parses");
        assert_eq!(parsed.agent, "x");

        // Proto-owned types carry no owner tag.
        let close = by_name("session/close");
        assert!(close["x-zeroclaw-params"].get("owner").is_none());

        // The former free-form params now name their keys.
        let renew = by_name("cert/renew");
        assert_eq!(renew["params"][0]["name"], json!("csr_pem"));
        assert_eq!(renew["params"][0]["required"], json!(true));
        let fetch = by_name("locales/fetch");
        assert_eq!(fetch["result"]["x-zeroclaw-shape"], json!("typed"));
    }

    /// Every method's result is described. A method whose result is truly
    /// free-form must say so with a documented result type rather than
    /// `Shape::Untyped`, so the contract never hides a shape.
    #[test]
    fn every_result_is_described() {
        let doc: Value = serde_json::from_str(&render().expect("render")).expect("valid JSON");
        for m in doc["methods"].as_array().expect("methods") {
            assert_ne!(
                m["result"]["x-zeroclaw-shape"],
                json!("untyped"),
                "{}: declare a result type instead of an untyped result",
                m["name"]
            );
        }
    }

    /// A Draft 7 validator for one component, resolving its references
    /// inside the generated document.
    fn component_validator(doc: &Value, component: &str) -> jsonschema::Validator {
        let root = json!({
            "$ref": format!("{DEFINITIONS_PATH}{component}"),
            "components": doc["components"],
        });
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .build(&root)
            .unwrap_or_else(|e| panic!("{component} compiles: {e}"))
    }

    /// `sops/validate` names its real keys, keeps the draft-or-stored
    /// constraint, and a present `sop` is judged as a draft even when a
    /// `name` is also sent.
    #[test]
    fn sops_validate_names_its_keys_and_keeps_its_forms() {
        let doc: Value = serde_json::from_str(&render().expect("render")).expect("valid JSON");
        let validate = doc["methods"]
            .as_array()
            .expect("methods")
            .iter()
            .find(|m| m["name"] == json!("sops/validate"))
            .expect("sops/validate");
        let names: Vec<&str> = validate["params"]
            .as_array()
            .expect("params")
            .iter()
            .map(|p| p["name"].as_str().expect("descriptor name"))
            .collect();
        assert_eq!(names, ["name", "original_name", "sop"]);

        let validator = component_validator(&doc, "SopValidateParams");
        let sop = json!({
            "name": "deploy", "description": "d", "version": "1", "priority": "normal",
            "execution_mode": "supervised", "triggers": [], "steps": [],
        });
        assert!(validator.is_valid(&json!({ "name": "deploy" })));
        assert!(validator.is_valid(&json!({ "sop": sop, "original_name": "old" })));
        assert!(!validator.is_valid(&json!({})), "one form is required");
        assert!(
            !validator.is_valid(&json!({ "name": null })),
            "the stored selector is a string"
        );
        assert!(
            !validator.is_valid(&json!({ "sop": 7, "name": "deploy" })),
            "a malformed draft does not fall back to the stored form"
        );
    }

    /// Fields that carry a SOP or an approval decision as raw JSON accept
    /// exactly what the runtime parser accepts.
    #[test]
    fn raw_sop_and_decision_fields_validate_like_the_runtime_parser() {
        use zeroclaw_runtime::sop::Sop;
        use zeroclaw_runtime::sop::approval::ApprovalDecision;

        let doc: Value = serde_json::from_str(&render().expect("render")).expect("valid JSON");
        let valid_sop = json!({
            "name": "deploy", "description": "d", "version": "1", "priority": "normal",
            "execution_mode": "supervised", "triggers": [], "steps": [],
        });
        let sop_samples = [
            valid_sop.clone(),
            json!(7),
            json!("deploy"),
            json!(null),
            json!({}),
            json!({ "name": "deploy" }),
        ];
        let edit = json!({ "op": "connect", "from": 1, "to": 2, "role": "sequence" });
        for component in ["SopSaveRequest", "SopDraftParams", "SopWireDraftParams"] {
            let validator = component_validator(&doc, component);
            for sample in &sop_samples {
                let request = json!({ "sop": sample, "edit": edit });
                assert_eq!(
                    validator.is_valid(&request),
                    serde_json::from_value::<Sop>(sample.clone()).is_ok(),
                    "{component} disagrees with the runtime parser on sop = {sample}"
                );
            }
        }
        assert!(
            serde_json::from_value::<Sop>(valid_sop).is_ok(),
            "the fixture is a real SOP"
        );

        let validator = component_validator(&doc, "SopDecideRequest");
        for decision in [
            json!("approve"),
            json!({ "deny": { "reason": "no" } }),
            json!({ "deny": {} }),
            json!({ "amend": { "text": "t" } }),
            json!({ "revise": { "guidance": "g" } }),
            json!("not_a_decision"),
            json!({ "amend": {} }),
            json!(7),
        ] {
            let request = json!({ "name": "deploy", "run_id": "r1", "decision": decision });
            assert_eq!(
                validator.is_valid(&request),
                serde_json::from_value::<ApprovalDecision>(decision.clone()).is_ok(),
                "SopDecideRequest disagrees with the runtime parser on decision = {decision}"
            );
        }
    }

    #[test]
    fn every_schema_in_the_document_is_draft_7() {
        const LATER_DIALECT_KEYWORDS: &[&str] = &[
            "prefixItems",
            "$defs",
            "unevaluatedProperties",
            "unevaluatedItems",
            "dependentRequired",
            "dependentSchemas",
            "$dynamicRef",
            "$dynamicAnchor",
            "$recursiveRef",
            "$anchor",
        ];
        let doc: Value = serde_json::from_str(&render().expect("render")).expect("valid JSON");
        assert_eq!(doc["x-zeroclaw"]["schema_dialect"], json!(SCHEMA_DIALECT));
        fn walk(v: &Value, path: &str, out: &mut Vec<String>) {
            match v {
                Value::Object(map) => {
                    for (k, child) in map {
                        if LATER_DIALECT_KEYWORDS.contains(&k.as_str()) {
                            out.push(format!("{path}/{k}"));
                        }
                        walk(child, &format!("{path}/{k}"), out);
                    }
                }
                Value::Array(items) => items
                    .iter()
                    .enumerate()
                    .for_each(|(i, child)| walk(child, &format!("{path}/{i}"), out)),
                _ => {}
            }
        }
        let mut offenders = Vec::new();
        walk(&doc, "", &mut offenders);
        assert!(offenders.is_empty(), "non-Draft-7 keywords: {offenders:?}");
        // The tuple that motivated the check: `LogsQueryResult.next_cursor`
        // is `Option<(String, String)>`; under Draft 7 a validator must take
        // `null` and two strings, and reject anything else.
        let cursor = &doc["components"]["schemas"]["LogsQueryResult"]["properties"]["next_cursor"];
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .build(cursor)
            .expect("Draft 7 schema compiles");
        for accepted in [json!(null), json!(["timestamp", "id"])] {
            assert!(
                validator.is_valid(&accepted),
                "{cursor} must accept {accepted}"
            );
        }
        for rejected in [
            json!([1, 2]),
            json!(["only-one"]),
            json!(["a", "b", "c"]),
            json!(["a", 1]),
            json!("timestamp:id"),
        ] {
            assert!(
                !validator.is_valid(&rejected),
                "{cursor} must reject {rejected}"
            );
        }
    }

    /// OpenRPC orders every required parameter before every optional one.
    #[test]
    fn required_descriptors_precede_optional_ones() {
        let doc: Value = serde_json::from_str(&render().expect("render")).expect("valid JSON");
        for m in doc["methods"].as_array().expect("methods") {
            let flags: Vec<bool> = m["params"]
                .as_array()
                .expect("params array")
                .iter()
                .map(|p| p["required"].as_bool().expect("required flag"))
                .collect();
            let first_optional = flags.iter().position(|r| !r).unwrap_or(flags.len());
            assert!(
                flags[first_optional..].iter().all(|r| !r),
                "{}: a required parameter follows an optional one: {flags:?}",
                m["name"]
            );
        }
    }

    #[test]
    fn schema_references_resolve_inside_the_document() {
        let doc: Value = serde_json::from_str(&render().expect("render")).expect("valid JSON");
        let schemas = doc["components"]["schemas"].as_object().expect("schemas");
        fn walk(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(map) => {
                    if let Some(Value::String(r)) = map.get("$ref") {
                        out.push(r.clone());
                    }
                    map.values().for_each(|v| walk(v, out));
                }
                Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
                _ => {}
            }
        }
        let mut refs = Vec::new();
        walk(&doc, &mut refs);
        assert!(!refs.is_empty(), "expected at least one $ref");
        for r in refs {
            let name = r
                .strip_prefix(DEFINITIONS_PATH)
                .unwrap_or_else(|| panic!("unexpected $ref target {r}"));
            assert!(schemas.contains_key(name), "dangling $ref {r}");
        }
    }
}
