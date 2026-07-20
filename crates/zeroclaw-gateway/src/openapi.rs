//! Runtime-generated OpenAPI 3.1 document for the new `/api/config/*` surface.

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::OnceLock;

/// Route-specific CSP for the Scalar explorer page. The finalized router is
/// wrapped by the default security-header layer, whose `set_if_absent` keeps a
/// handler-owned `content-security-policy`. The default dashboard CSP only
/// permits `script-src 'self'`, which would block the Scalar bundle served from
/// `cdn.jsdelivr.net` and silently degrade `/api/docs` to the offline fallback.
/// This policy admits the CDN script (and the styles/fonts/images it injects)
/// while still denying framing and object embedding.
const DOCS_CSP: &str = "default-src 'self'; \
     script-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net; \
     style-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net; \
     img-src 'self' data: https://cdn.jsdelivr.net; \
     font-src 'self' data: https://cdn.jsdelivr.net; \
     connect-src 'self'; \
     object-src 'none'; \
     frame-ancestors 'none'; \
     base-uri 'none'";

#[cfg(feature = "schema-export")]
use schemars::{JsonSchema, schema_for};

static CACHED: OnceLock<serde_json::Value> = OnceLock::new();

pub async fn handle_docs() -> Response {
    let html = include_str!("openapi_docs.html");
    let mut response = (StatusCode::OK, html).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(DOCS_CSP),
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

#[cfg(feature = "schema-export")]
pub fn build_spec() -> serde_json::Value {
    use crate::api_config::{
        DriftEntry, DriftResponse, InitQuery, InitResponse, ListResponse, MigrateResponse, PatchOp,
        PatchResponse, PropPutBody, PropResponse, ReloadStatusResponse, SecretResponse,
    };
    use crate::version::{
        UpgradeAcceptedResponse, UpgradeRequest, UpgradeStatusResponse, VersionCheckResponse,
        VersionErrorResponse,
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
            "ReloadStatusResponse": schema_value::<ReloadStatusResponse>(),
            "Config":           schema_value::<zeroclaw_config::schema::Config>(),
            "VersionCheckResponse":   schema_value::<VersionCheckResponse>(),
            "UpgradeRequest":         schema_value::<UpgradeRequest>(),
            "UpgradeAcceptedResponse": schema_value::<UpgradeAcceptedResponse>(),
            "UpgradeStatusResponse":  schema_value::<UpgradeStatusResponse>(),
            "VersionError":           schema_value::<VersionErrorResponse>(),
            "Sop":              schema_value::<zeroclaw_runtime::sop::Sop>(),
            "SopGraph":         schema_value::<zeroclaw_runtime::sop::SopGraph>(),
            "GraphLegend":      schema_value::<zeroclaw_runtime::sop::GraphLegend>(),
            "RunOverlay":       schema_value::<zeroclaw_runtime::sop::RunOverlay>(),
            "ApprovalDecision": schema_value::<zeroclaw_runtime::sop::ApprovalDecision>(),
            "TriggerSourceRegistry": schema_value::<zeroclaw_runtime::sop::TriggerSourceRegistry>(),
            "SlashOptionKindsResult": schema_value::<crate::api_skills::SlashOptionKindsResult>(),
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
        "description": "Dotted property path, e.g. `agents.researcher.model_provider`."
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
        "description": "Section prefix to scope the init pass (e.g. `model_providers`)."
    });

    let force_param = serde_json::json!({
        "name": "force",
        "in": "query",
        "required": false,
        "schema": { "type": "boolean" },
        "description": "Bypass the 1h server-side cache and re-query GitHub."
    });

    let check_version_param = serde_json::json!({
        "name": "version",
        "in": "query",
        "required": false,
        "schema": { "type": "string" },
        "description": "Check a specific release tag instead of the latest."
    });

    let handoff_param = serde_json::json!({
        "name": "handoff_id",
        "in": "query",
        "required": false,
        "schema": { "type": "string" },
        "description": "Scope the status read to a specific upgrade run (404 on mismatch)."
    });

    let version_error = |description: &str| {
        serde_json::json!({
            "description": description,
            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/VersionError" } } }
        })
    };

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
        "/api/skills/slash-option-kinds": {
            "get": {
                "tags": ["skills"],
                "summary": "Typed slash-option kind registry",
                "description": "Returns the canonical set of typed slash-command option kinds and each kind's constraint capabilities (choices / numeric bounds / length bounds), built by walking the backend kind enum. Surfaces read this instead of restating the kind list.",
                "responses": {
                    "200": {
                        "description": "The slash-option kind registry.",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SlashOptionKindsResult" } } }
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
        "/api/config/reload-status": {
            "get": {
                "tags": ["config"],
                "summary": "Pending-reload flag for the running daemon",
                "description": "Returns `{pending_reload: true}` when one or more config writes have landed since the last `/admin/reload`. Distinct from `/api/config/drift`, which compares disk to in-memory; this flag fires on in-process PATCHes that hot-swap memory but still need subsystem re-init (channels, providers, scheduler) to take effect.",
                "responses": {
                    "200": {
                        "description": "Pending-reload flag.",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ReloadStatusResponse" } } }
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
        },
        "/api/version/check": {
            "get": {
                "tags": ["version"],
                "summary": "Check for a newer release",
                "description": "Runs `zeroclaw update --check --json` server-side (1h cache, force-refreshable). Never fails the dashboard: on any error it still returns 200 with `is_newer: false` and an `error` string so the version badge degrades gracefully.",
                "parameters": [force_param, check_version_param],
                "responses": {
                    "200": {
                        "description": "Version comparison, or a soft-error envelope carrying `error`.",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/VersionCheckResponse" } } }
                    }
                }
            }
        },
        "/api/version/upgrade": {
            "post": {
                "tags": ["version"],
                "summary": "Apply an upgrade via `zeroclaw update`",
                "description": "Replaces the running binary and (opt-in) restarts the process. Gated by `gateway.allow_self_upgrade` (default off → 403). Single-flight: a concurrent call returns 409. Returns 202 with a `handoff_id`; poll `/api/version/upgrade/status` for progress. An empty body uses defaults (latest version, no auto-restart).",
                "requestBody": {
                    "required": false,
                    "content": { "application/json": { "schema": { "$ref": "#/components/schemas/UpgradeRequest" } } }
                },
                "responses": {
                    "202": {
                        "description": "Upgrade accepted; it runs on a detached task.",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/UpgradeAcceptedResponse" } } }
                    },
                    "400": version_error("Invalid JSON body, or `auto_restart` is not available in this environment (container/non-unix bare process)."),
                    "403": version_error("Self-upgrade is disabled (`gateway.allow_self_upgrade = false`)."),
                    "409": version_error("An upgrade is already in progress."),
                }
            }
        },
        "/api/version/upgrade/status": {
            "get": {
                "tags": ["version"],
                "summary": "Poll in-flight upgrade progress",
                "description": "Returns `{ state: \"idle\" }` when no upgrade has run this process, else the live phase (0..=6), the last ~50 log lines, and restart metadata. Pass `handoff_id` to scope the read to a specific run.",
                "parameters": [handoff_param],
                "responses": {
                    "200": {
                        "description": "Current upgrade progress.",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/UpgradeStatusResponse" } } }
                    },
                    "404": version_error("Unknown `handoff_id`."),
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
    #[cfg(feature = "a2a")]
    augment_spec_with_a2a(
        &mut spec,
        schema_value::<crate::a2a::JsonRpcRequest>(),
        schema_value::<crate::a2a::OutTask>(),
    );
    flatten_defs_into_components(&mut spec);
    spec
}

/// Add the A2A task endpoint and its request/response schemas to the spec.
/// Gated on `feature = "a2a"` so `--no-default-features --features
/// schema-export` (a2a off) still compiles and renders a coherent spec.
#[cfg(all(feature = "schema-export", feature = "a2a"))]
fn augment_spec_with_a2a(
    spec: &mut serde_json::Value,
    task_request_schema: serde_json::Value,
    task_schema: serde_json::Value,
) {
    if let Some(schemas) = spec
        .pointer_mut("/components/schemas")
        .and_then(|v| v.as_object_mut())
    {
        schemas.insert("A2aTaskRequest".to_string(), task_request_schema);
        schemas.insert("A2aTask".to_string(), task_schema);
    }
    if let Some(paths) = spec.pointer_mut("/paths").and_then(|v| v.as_object_mut()) {
        paths.insert(
            "/a2a/{alias}".to_string(),
            serde_json::json!({
                "post": {
                    "tags": ["a2a"],
                    "summary": "Send a task to a published A2A agent",
                    "description": "JSON-RPC 2.0 endpoint for one published agent. Only `message/send` is handled: the message `parts` of kind `text` are joined into the agent prompt, the agent runs one turn, and a completed A2A `Task` carrying the reply as an artifact is returned. Requires a pairing-derived bearer token (the turn is tool-enabled, so it is never served unauthenticated). Unpublished or disabled aliases return 404. The server must be enabled (`[a2a.server] enabled`) and the alias published (`[agents.<alias>.a2a] published`).",
                    "parameters": [{
                        "name": "alias",
                        "in": "path",
                        "required": true,
                        "schema": { "type": "string" },
                        "description": "Published agent alias, as listed in the discovery catalog."
                    }],
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/A2aTaskRequest" } } }
                    },
                    "responses": {
                        "200": {
                            "description": "JSON-RPC response. On success `result` is a completed A2A Task; on a JSON-RPC error (unknown method, bad params) `error` carries the code and message.",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/A2aTask" } } }
                        },
                        "401": {
                            "description": "Missing or invalid bearer token while pairing is required."
                        },
                        "404": {
                            "description": "Server disabled, alias unpublished, or alias unknown."
                        }
                    }
                }
            }),
        );
    }
}

#[cfg(feature = "schema-export")]
fn flatten_defs_into_components(spec: &mut serde_json::Value) {
    use serde_json::Value;

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
        assert!(paths.get("/api/config/drift").is_some());
        assert!(paths.get("/api/config/reload-status").is_some());
        assert!(paths.get("/api/version/check").is_some());
        assert!(paths.get("/api/version/upgrade").is_some());
        assert!(paths.get("/api/version/upgrade/status").is_some());
        assert!(paths.get("/api/skills/slash-option-kinds").is_some());
        #[cfg(feature = "a2a")]
        assert!(paths.get("/a2a/{alias}").is_some());
    }

    #[cfg(feature = "schema-export")]
    #[test]
    fn spec_registers_version_schemas() {
        let spec = build_spec();
        let schemas = spec.pointer("/components/schemas").unwrap();
        assert!(schemas.get("VersionCheckResponse").is_some());
        assert!(schemas.get("UpgradeRequest").is_some());
        assert!(schemas.get("UpgradeAcceptedResponse").is_some());
        assert!(schemas.get("UpgradeStatusResponse").is_some());
        assert!(schemas.get("VersionError").is_some());
        // The `state` enum is hoisted out of UpgradeStatusResponse's `$defs`
        // into top-level components by `flatten_defs_into_components`.
        assert!(schemas.get("UpgradeStatusState").is_some());
        // Refs must be rewritten to point at the hoisted component, not `$defs`.
        let spec_str = serde_json::to_string(&spec).unwrap();
        assert!(!spec_str.contains("#/$defs/"));
    }

    #[cfg(feature = "schema-export")]
    #[test]
    fn config_api_schemas_keep_operator_descriptions() {
        let spec = build_spec();
        let cases = [
            ("/components/schemas/PatchOp/description", "JSON Patch"),
            (
                "/components/schemas/PatchResponse/properties/warnings/description",
                "Non-fatal validation warnings",
            ),
            (
                "/components/schemas/ListEntry/description",
                "Single entry in the list response",
            ),
            (
                "/components/schemas/DriftEntry/description",
                "in-memory Config diverges",
            ),
            (
                "/components/schemas/ReloadStatusResponse/properties/pending_reload/description",
                "subsystem re-instantiation",
            ),
        ];

        for (pointer, expected) in cases {
            let description = spec
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| panic!("missing generated description at {pointer}"));
            assert!(
                description.contains(expected),
                "description at {pointer} must retain `{expected}`: {description}",
            );
        }
    }

    #[cfg(all(feature = "schema-export", feature = "a2a"))]
    #[test]
    fn spec_registers_a2a_task_schemas() {
        let spec = build_spec();
        let schemas = spec.pointer("/components/schemas").unwrap();
        assert!(schemas.get("A2aTaskRequest").is_some());
        assert!(schemas.get("A2aTask").is_some());
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

    #[tokio::test]
    async fn docs_route_sets_own_csp_admitting_scalar_cdn() {
        let response = handle_docs().await;
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .expect("docs route must set its own CSP")
            .to_str()
            .unwrap();
        assert!(
            csp.contains("script-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net"),
            "docs CSP must admit the Scalar CDN script: {csp}"
        );
    }

    #[test]
    fn docs_csp_survives_default_security_layer() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(DOCS_CSP),
        );
        crate::security_headers::inject(&mut headers, false);
        let csp = headers
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            csp.contains("https://cdn.jsdelivr.net"),
            "default layer must not clobber the handler-owned docs CSP: {csp}"
        );
    }

    #[cfg(all(feature = "schema-export", feature = "a2a"))]
    #[test]
    fn a2a_task_operation_requires_bearer_auth() {
        let spec = build_spec();
        // No per-operation security override: the endpoint inherits the
        // global `bearerAuth` requirement. A tool-enabled agent turn is never
        // served unauthenticated.
        let security = spec.pointer("/paths/~1a2a~1{alias}/post/security");
        assert_eq!(security, None);
        let global = spec
            .pointer("/security")
            .and_then(|v| v.as_array())
            .expect("global security present");
        assert!(
            global
                .iter()
                .any(|scheme| scheme.get("bearerAuth").is_some()),
            "global security must require bearerAuth"
        );
    }
}
