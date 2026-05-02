//! Runtime-generated OpenAPI 3.1 document for the new `/api/config/*` surface.
//!
//! Built from the same `schemars::JsonSchema` derives the request/response
//! types carry. The generator does not introspect the axum router — instead it
//! walks a hand-maintained `(method, path, request_type, response_type)` list
//! local to this module. New endpoints under the same surface should be added
//! to that list when they land. CI checks (forthcoming) can diff the rendered
//! spec against a committed snapshot to fail builds when handlers are added
//! without a corresponding OpenAPI entry.
//!
//! Cached behind a `OnceCell` because the spec is static per build.
//!
//! See #6175.

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::OnceLock;

#[cfg(feature = "schema-export")]
use schemars::{JsonSchema, schema_for};

static CACHED: OnceLock<serde_json::Value> = OnceLock::new();

/// `GET /api/docs` — the Scalar API explorer page. Loads the standalone Scalar
/// bundle from a CDN and points it at `/api/openapi.json`. The page is a
/// single static HTML blob — no NPM dep, no committed bundle, ~2KB.
///
/// Authentication: Scalar's built-in panel prompts the user for the bearer
/// token before any "Try it out" call, so the docs themselves are
/// unauthenticated but the live calls honor the existing pairing/bearer auth.
pub async fn handle_docs() -> Response {
    let html = include_str!("openapi_docs.html");
    let mut response = (StatusCode::OK, html).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

/// `GET /api/openapi.json` — returns the OpenAPI 3.1 document for the gateway
/// surface that is documented today (`/api/config/*`). Static per build;
/// browsers and the eventual Scalar explorer consume this as their data source.
pub async fn handle_openapi_json() -> Response {
    let body = CACHED.get_or_init(build_spec).clone();
    let mut response = (StatusCode::OK, axum::Json(body)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

/// Build the OpenAPI 3.1 document. Pub so the `xtask gen-openapi` binary
/// can render the same JSON the gateway serves and write it to the
/// committed snapshot at `crates/zeroclaw-gateway/openapi.json`. CI
/// staleness check (`xtask gen-openapi --check`) diffs the rendered
/// spec against the committed file so a handler change without a spec
/// update fails the build.
#[cfg(feature = "schema-export")]
pub fn build_spec() -> serde_json::Value {
    use crate::api_config::{
        DriftEntry, DriftResponse, InitQuery, InitResponse, ListResponse, MigrateResponse, PatchOp,
        PatchResponse, PropPutBody, PropResponse, SecretResponse,
    };
    use zeroclaw_config::api_error::ConfigApiError;

    fn schema_value<T: JsonSchema>() -> serde_json::Value {
        serde_json::to_value(schema_for!(T)).unwrap_or(serde_json::Value::Null)
    }

    let components = serde_json::json!({
        "schemas": {
            "ConfigApiError":   schema_value::<ConfigApiError>(),
            "PropPutBody":      schema_value::<PropPutBody>(),
            "PropResponse":     schema_value::<PropResponse>(),
            "SecretResponse":   schema_value::<SecretResponse>(),
            "ListResponse":     schema_value::<ListResponse>(),
            "PatchOp":          schema_value::<PatchOp>(),
            "PatchResponse":    schema_value::<PatchResponse>(),
            "InitQuery":        schema_value::<InitQuery>(),
            "InitResponse":     schema_value::<InitResponse>(),
            "MigrateResponse":  schema_value::<MigrateResponse>(),
            "DriftEntry":       schema_value::<DriftEntry>(),
            "DriftResponse":    schema_value::<DriftResponse>(),
            "Config":           schema_value::<zeroclaw_config::schema::Config>(),
        },
        "securitySchemes": {
            "bearerAuth": {
                "type": "http",
                "scheme": "bearer",
                "description": "Pairing-derived bearer token. Printed at gateway startup.",
            }
        }
    });

    let path_param = serde_json::json!({
        "name": "path",
        "in": "query",
        "required": true,
        "schema": { "type": "string" },
        "description": "Dotted property path, e.g. `providers.fallback`."
    });

    let prefix_param = serde_json::json!({
        "name": "prefix",
        "in": "query",
        "required": false,
        "schema": { "type": "string" },
        "description": "Optional prefix to scope the listing."
    });

    let section_param = serde_json::json!({
        "name": "section",
        "in": "query",
        "required": false,
        "schema": { "type": "string" },
        "description": "Section prefix to scope the init pass (e.g. `providers`)."
    });

    let error_responses = serde_json::json!({
        "400": {
            "description": "Validation, type, or operation error. See ConfigApiError.code.",
            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConfigApiError" } } }
        },
        "404": {
            "description": "Path not found in the schema.",
            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConfigApiError" } } }
        },
        "409": {
            "description": "On-disk config drifted from in-memory state.",
            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConfigApiError" } } }
        },
        "500": {
            "description": "Internal error or daemon-reload failure.",
            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ConfigApiError" } } }
        }
    });

    let prop_get_responses = serde_json::json!({
        "200": {
            "description": "Property value (non-secret) or `{populated}` (secret).",
            "content": {
                "application/json": {
                    "schema": {
                        "oneOf": [
                            { "$ref": "#/components/schemas/PropResponse" },
                            { "$ref": "#/components/schemas/SecretResponse" }
                        ]
                    }
                }
            }
        },
        "404": error_responses["404"].clone(),
    });

    let paths = serde_json::json!({
        "/api/config/prop": {
            "get": {
                "tags": ["config"],
                "summary": "Read one property",
                "description": "Returns the user value for non-secret fields. For secret fields, returns `{path, populated}` only — never the value, length, or any encoded form.",
                "parameters": [path_param.clone()],
                "responses": prop_get_responses,
            },
            "put": {
                "tags": ["config"],
                "summary": "Set one property",
                "description": "Validates the resulting whole-config state, persists, and swaps in-memory. For secret fields, response carries `{populated: true}` only.",
                "requestBody": {
                    "required": true,
                    "content": { "application/json": { "schema": { "$ref": "#/components/schemas/PropPutBody" } } }
                },
                "responses": prop_get_responses,
            },
            "delete": {
                "tags": ["config"],
                "summary": "Reset one property to its default",
                "parameters": [path_param.clone()],
                "responses": prop_get_responses,
            },
        },
        "/api/config/list": {
            "get": {
                "tags": ["config"],
                "summary": "Enumerate properties",
                "description": "Returns every reachable path with its type, category, and onboard section. Secret entries carry `{populated, is_secret: true}` and no value.",
                "parameters": [prefix_param],
                "responses": {
                    "200": {
                        "description": "List of properties.",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ListResponse" } } }
                    }
                }
            }
        },
        "/api/config": {
            "patch": {
                "tags": ["config"],
                "summary": "Apply a JSON Patch (RFC 6902) document atomically",
                "description": "Operations execute in order against an in-memory copy; `Config::validate()` runs once at the end; on success the snapshot persists and swaps. On failure, on-disk and in-memory state are unchanged. `move`/`copy` return `op_not_supported`. `test` against a secret path returns `secret_test_forbidden`.\n\n**Drift guard:** if the on-disk file has drifted from in-memory state on any path being patched, returns 409 `config_changed_externally` unless the request carries `X-ZeroClaw-Override-Drift: true`. GET /api/config/drift to inspect first.",
                "parameters": [{
                    "name": "X-ZeroClaw-Override-Drift",
                    "in": "header",
                    "required": false,
                    "schema": { "type": "string", "enum": ["true"] },
                    "description": "Set to `true` to overwrite externally-edited values without confirmation."
                }],
                "requestBody": {
                    "required": true,
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "array",
                                "items": { "$ref": "#/components/schemas/PatchOp" }
                            }
                        }
                    }
                },
                "responses": {
                    "200": {
                        "description": "All operations applied and config saved.",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/PatchResponse" } } }
                    },
                    "400": error_responses["400"].clone(),
                    "404": error_responses["404"].clone(),
                    "409": error_responses["409"].clone(),
                    "500": error_responses["500"].clone(),
                }
            }
        },
        "/api/config/init": {
            "post": {
                "tags": ["config"],
                "summary": "Instantiate `None` nested sections with defaults",
                "parameters": [section_param],
                "responses": {
                    "200": {
                        "description": "Initialized section names (empty when nothing was uninitialized).",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/InitResponse" } } }
                    }
                }
            }
        },
        "/api/config/drift": {
            "get": {
                "tags": ["config"],
                "summary": "Drift between in-memory and on-disk config",
                "description": "Returns properties whose in-memory values differ from what's on disk now. Empty when they agree. Secret entries carry only `{path, secret: true, drifted: true}`; values never leave the server.",
                "responses": {
                    "200": {
                        "description": "Drift summary.",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/DriftResponse" } } }
                    }
                }
            }
        },
        "/api/config/migrate": {
            "post": {
                "tags": ["config"],
                "summary": "Apply on-disk schema migration in place",
                "description": "Mirrors `zeroclaw config migrate`. Backs up the previous file as `config.toml.bak` before writing.",
                "responses": {
                    "200": {
                        "description": "Migration applied (or already at the current schema version).",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/MigrateResponse" } } }
                    }
                }
            }
        }
    });

    let mut spec = serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "ZeroClaw Gateway — Config CRUD",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Per-property CRUD endpoints over the same `Config` mutation core that `zeroclaw config get/set/list/init/migrate` uses on the CLI. See https://github.com/zeroclaw-labs/zeroclaw/issues/6175 for the full surface and acceptance checklist.",
        },
        "security": [{"bearerAuth": []}],
        "paths": paths,
        "components": components,
    });
    flatten_defs_into_components(&mut spec);
    spec
}

/// schemars emits nested types under each component's `$defs` and
/// references them as `#/$defs/<Name>`. OpenAPI 3.1 tooling
/// (openapi-typescript, Scalar, codegen) expects them at top-level
/// `#/components/schemas/<Name>`. Hoist every `$defs` entry into
/// `components.schemas` and rewrite refs in place so the spec validates
/// and external tooling can walk it.
#[cfg(feature = "schema-export")]
fn flatten_defs_into_components(spec: &mut serde_json::Value) {
    use serde_json::Value;

    // Collect every `$defs` map across the spec — typically one per
    // top-level component schema. Hoist entries into a single
    // `components.schemas` map. Later entries with the same name win;
    // the macro generates identical schemas for identical types so
    // collisions are benign.
    let mut hoisted: serde_json::Map<String, Value> = serde_json::Map::new();
    collect_defs(spec, &mut hoisted);
    if let Some(schemas) = spec
        .pointer_mut("/components/schemas")
        .and_then(|v| v.as_object_mut())
    {
        for (k, v) in hoisted {
            schemas.entry(k).or_insert(v);
        }
    }
    rewrite_refs(spec);
    strip_defs(spec);
}

#[cfg(feature = "schema-export")]
fn collect_defs(
    value: &mut serde_json::Value,
    out: &mut serde_json::Map<String, serde_json::Value>,
) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Object(defs)) = map.get("$defs") {
                for (name, schema) in defs {
                    out.entry(name.clone()).or_insert_with(|| schema.clone());
                }
            }
            for (_, child) in map.iter_mut() {
                collect_defs(child, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr.iter_mut() {
                collect_defs(child, out);
            }
        }
        _ => {}
    }
}

#[cfg(feature = "schema-export")]
fn rewrite_refs(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get_mut("$ref")
                && let Some(rest) = s.strip_prefix("#/$defs/")
            {
                *s = format!("#/components/schemas/{rest}");
            }
            for (_, child) in map.iter_mut() {
                rewrite_refs(child);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr.iter_mut() {
                rewrite_refs(child);
            }
        }
        _ => {}
    }
}

#[cfg(feature = "schema-export")]
fn strip_defs(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("$defs");
            for (_, child) in map.iter_mut() {
                strip_defs(child);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr.iter_mut() {
                strip_defs(child);
            }
        }
        _ => {}
    }
}

#[cfg(not(feature = "schema-export"))]
pub fn build_spec() -> serde_json::Value {
    serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "ZeroClaw Gateway",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "OpenAPI generation requires the `schema-export` feature; this build was compiled without it.",
        },
        "paths": {},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "schema-export")]
    #[test]
    fn spec_has_expected_paths() {
        let spec = build_spec();
        let paths = spec.get("paths").unwrap();
        assert!(paths.get("/api/config/prop").is_some());
        assert!(paths.get("/api/config/list").is_some());
        assert!(paths.get("/api/config").is_some());
        assert!(paths.get("/api/config/init").is_some());
        assert!(paths.get("/api/config/migrate").is_some());
    }

    #[cfg(feature = "schema-export")]
    #[test]
    fn spec_declares_bearer_auth() {
        let spec = build_spec();
        let scheme = spec
            .pointer("/components/securitySchemes/bearerAuth/scheme")
            .and_then(|v| v.as_str());
        assert_eq!(scheme, Some("bearer"));
    }
}
