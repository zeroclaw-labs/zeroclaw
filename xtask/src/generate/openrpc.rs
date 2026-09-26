//! Render the daemon RPC contract as an OpenRPC document.
//!
//! The source of truth is `zeroclaw-rpc-proto`: the method table, each
//! method's declared params/result shape, the notification names, the error
//! codes and the JSON Schema of every wire type it defines. The runtime
//! contributes one fact per method that only it knows, the authorization
//! classification. Types the proto crate does not own are recorded by name
//! with their owning crate instead of a schema, so the document never claims
//! more than the code guarantees.
//!
//! Framing is not a schema-language concern. The NDJSON envelope, the
//! handshake and the transport rules are documented in prose in
//! `docs/book/src/architecture/rpc-socket.md`; this document describes what
//! travels inside the envelope.

use anyhow::{Context, ensure};
use schemars::{SchemaGenerator, generate::SchemaSettings};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use zeroclaw_rpc_proto::method::EXTERNAL_TYPES;
use zeroclaw_rpc_proto::{Method, RPC_PROTOCOL_VERSION, Shape, error_codes, notification, schema};
use zeroclaw_runtime::rpc::dispatch::{MethodAuthz, MethodAuthzExt};

/// Tracked output. Lives next to the prose RPC page it complements.
pub const OUTPUT_FILE: &str = "docs/book/src/architecture/zeroclaw-rpc.openrpc.json";

const DEFINITIONS_PATH: &str = "#/components/schemas/";

fn workspace_root() -> PathBuf {
    crate::util::repo_root()
}

fn generator() -> SchemaGenerator {
    let mut settings = SchemaSettings::draft2020_12();
    settings.definitions_path = DEFINITIONS_PATH.into();
    settings.inline_subschemas = false;
    settings.into_generator()
}

/// Describe one params/result side: a schema reference, an external type
/// marker, a free-form value, or nothing.
fn shape_value(generator: &mut SchemaGenerator, shape: Shape) -> Option<Value> {
    match shape {
        Shape::None => None,
        Shape::Untyped => Some(json!({
            "name": "value",
            "schema": { "description": "Free-form JSON shaped by the daemon at runtime." },
            "x-zeroclaw-shape": "untyped",
        })),
        Shape::Typed(name) => match schema::subschema_for_named(generator, name) {
            Some(schema) => Some(json!({
                "name": name,
                "schema": schema,
                "x-zeroclaw-shape": "typed",
            })),
            None => {
                let owner = EXTERNAL_TYPES
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, owner)| *owner)
                    .unwrap_or("unknown");
                Some(json!({
                    "name": name,
                    "schema": { "description": format!("`{name}`, defined in `{owner}`; no schema is exported yet.") },
                    "x-zeroclaw-shape": "external",
                    "x-zeroclaw-owner": owner,
                }))
            }
        },
    }
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

    let methods: Vec<Value> = Method::ALL
        .iter()
        .map(|(method, wire)| {
            let contract = method.contract();
            let mut entry = Map::new();
            entry.insert("name".into(), json!(wire));
            entry.insert("paramStructure".into(), json!("by-name"));
            let params: Vec<Value> = shape_value(&mut generator, contract.params)
                .map(|mut p| {
                    if let Some(obj) = p.as_object_mut() {
                        obj.insert("name".into(), json!("params"));
                        obj.insert("required".into(), json!(true));
                    }
                    p
                })
                .into_iter()
                .collect();
            entry.insert("params".into(), Value::Array(params));
            let result = shape_value(&mut generator, contract.result).map_or_else(
                || json!({ "name": "result", "schema": { "type": "null" } }),
                |mut r| {
                    if let Some(obj) = r.as_object_mut() {
                        obj.insert("name".into(), json!("result"));
                    }
                    r
                },
            );
            entry.insert("result".into(), result);
            entry.insert("x-zeroclaw-authorization".into(), authorization(*method));
            Value::Object(entry)
        })
        .collect();

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

    let schemas = generator.take_definitions(true);

    let document = json!({
        "openrpc": "1.3.2",
        "info": {
            "title": "ZeroClaw daemon RPC",
            "version": RPC_PROTOCOL_VERSION.to_string(),
            "description": "JSON-RPC 2.0 methods served by `zeroclaw daemon` over the local socket or named pipe, framed as newline-delimited JSON. Generated by `cargo generate openrpc` from `zeroclaw-rpc-proto`; do not edit by hand. Framing, the handshake and transport rules are described in `rpc-socket.md`.",
        },
        "x-zeroclaw": {
            "protocol_version": RPC_PROTOCOL_VERSION,
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
