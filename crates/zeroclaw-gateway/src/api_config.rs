//! Per-property CRUD endpoints for `/api/config/*`.
//!
//! These endpoints expose the same `Config::get_prop` / `set_prop` core that
//! `zeroclaw config get/set/list/init/migrate` uses on the CLI. Both are thin
//! frontends over the same mutation primitive.
//!
//! Returns structured `ConfigApiError` responses with stable codes the
//! dashboard / scripts can match programmatically. Secret fields are
//! write-only over HTTP per the secrets-handling boundary defined in
//! the issue body.
//!
//! See #6175 for the full surface and acceptance checklist.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};
use zeroclaw_runtime::onboard::Section;

use super::AppState;
use super::api::require_auth;

// ── Request / response shapes ───────────────────────────────────────

/// `?path=...` query parameter shared by GET / DELETE / OPTIONS-with-path.
#[derive(Debug, Deserialize)]
pub struct PropQuery {
    pub path: String,
}

/// `?prefix=...` query parameter for list.
#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    pub prefix: Option<String>,
}

/// PUT body. Value is `serde_json::Value` so typed values (booleans, arrays,
/// numbers) round-trip correctly without going through the CLI's
/// comma-delimited string parser.
#[derive(Debug, Deserialize)]
pub struct PropPutBody {
    pub path: String,
    pub value: serde_json::Value,
    #[serde(default)]
    #[allow(dead_code)] // honored once comment-preserving save is wired in step 7
    pub comment: Option<String>,
}

/// Response for a non-secret GET.
#[derive(Debug, Serialize)]
pub struct PropResponse {
    pub path: String,
    pub value: serde_json::Value,
}

/// Response for a secret GET / PUT / DELETE — never carries the value or its
/// length. `populated: true` means the secret has a non-empty value on disk;
/// `populated: false` means the field is unset or empty.
#[derive(Debug, Serialize)]
pub struct SecretResponse {
    pub path: String,
    pub populated: bool,
}

/// Single entry in the list response. Secrets carry only `path + populated`;
/// non-secrets additionally carry `value`.
#[derive(Debug, Serialize)]
pub struct ListEntry {
    pub path: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    pub populated: bool,
    pub is_secret: bool,
    /// Onboard section name derived from the path's first segment via
    /// `Section::from_path`. `None` for paths that aren't part of any wizard
    /// section. The dashboard groups list entries by this for per-section
    /// rendering — same source the CLI wizard uses, no schema attribute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onboard_section: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub entries: Vec<ListEntry>,
}

// ── Error helpers ───────────────────────────────────────────────────

/// Convert a `ConfigApiError` into an axum `Response` with the correct status.
fn error_response(err: ConfigApiError) -> Response {
    let status =
        StatusCode::from_u16(err.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, axum::Json(err)).into_response()
}

/// Wrap an `anyhow::Error` from `Config::set_prop` / `get_prop` into a
/// `ConfigApiError`. Path-not-found errors get the specific code; everything
/// else falls through to ValidationFailed.
fn map_prop_error(err: anyhow::Error, path: &str) -> ConfigApiError {
    let msg = err.to_string();
    if msg.starts_with("Unknown property") {
        ConfigApiError::path_not_found(path)
    } else {
        ConfigApiError::from_validation(err).with_path(path)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Coerce a JSON value to the string representation `Config::set_prop` expects.
/// `set_prop` parses based on the field's PropKind, so for scalars we hand it
/// the raw display string; for arrays / objects we hand it the JSON encoding.
fn json_to_setprop_string(value: &serde_json::Value) -> Result<String, ConfigApiError> {
    match value {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Null => Ok(String::new()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => serde_json::to_string(value)
            .map_err(|e| {
                ConfigApiError::new(
                    ConfigApiCode::ValueTypeMismatch,
                    format!("could not serialize JSON value: {e}"),
                )
            }),
    }
}

/// Look up the prop_field metadata for a path. Used by the per-prop GET / PUT
/// handlers to decide whether the field is a secret.
fn lookup_prop_field(
    config: &zeroclaw_config::schema::Config,
    path: &str,
) -> Option<zeroclaw_config::traits::PropFieldInfo> {
    config
        .prop_fields()
        .into_iter()
        .find(|info| info.name == path)
}

/// Save the config and refresh in-memory state, returning a structured error
/// on failure. Mirrors the pattern in handle_api_config_put.
async fn persist_and_swap(
    state: &AppState,
    new_config: zeroclaw_config::schema::Config,
) -> Result<(), ConfigApiError> {
    if let Err(e) = new_config.save().await {
        return Err(ConfigApiError::new(
            ConfigApiCode::ReloadFailed,
            format!("save failed: {e}"),
        ));
    }
    *state.config.lock() = new_config;
    Ok(())
}

// ── Handlers ────────────────────────────────────────────────────────

/// GET /api/config/prop?path=providers.fallback
///
/// Returns the user's current value for non-secret fields. For secret fields,
/// returns `{path, populated}` only — the value, length, and any encoded form
/// are deliberately withheld per the secrets-handling boundary.
pub async fn handle_prop_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PropQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let info = match lookup_prop_field(&config, &q.path) {
        Some(info) => info,
        None => return error_response(ConfigApiError::path_not_found(&q.path)),
    };

    if info.is_secret || info.derived_from_secret {
        let populated = info.display_value != "<unset>";
        return axum::Json(SecretResponse {
            path: q.path,
            populated,
        })
        .into_response();
    }

    match config.get_prop(&q.path) {
        Ok(value_str) => {
            // get_prop returns the display string; surface it as JSON.
            // For typed-value fidelity, callers should hit OPTIONS to learn
            // the type and parse client-side. Future iterations can route
            // typed values through serde directly.
            axum::Json(PropResponse {
                path: q.path,
                value: serde_json::Value::String(value_str),
            })
            .into_response()
        }
        Err(e) => error_response(map_prop_error(e, &q.path)),
    }
}

/// PUT /api/config/prop with body `{path, value, comment?}`
///
/// Sets the value via `Config::set_prop`, validates the resulting whole-config
/// state, persists, and swaps in-memory. For secret fields, response carries
/// only `{path, populated: true}` — never echoes the value back.
pub async fn handle_prop_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<PropPutBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut new_config = state.config.lock().clone();
    let info = match lookup_prop_field(&new_config, &body.path) {
        Some(info) => info,
        None => return error_response(ConfigApiError::path_not_found(&body.path)),
    };

    let value_str = match json_to_setprop_string(&body.value) {
        Ok(s) => s,
        Err(e) => return error_response(e.with_path(&body.path)),
    };

    if let Err(e) = new_config.set_prop(&body.path, &value_str) {
        return error_response(map_prop_error(e, &body.path));
    }

    if let Err(e) = new_config.validate() {
        return error_response(ConfigApiError::from_validation(e).with_path(&body.path));
    }

    if let Err(e) = persist_and_swap(&state, new_config).await {
        return error_response(e);
    }

    if info.is_secret || info.derived_from_secret {
        axum::Json(SecretResponse {
            path: body.path,
            populated: !value_str.is_empty(),
        })
        .into_response()
    } else {
        axum::Json(PropResponse {
            path: body.path,
            value: serde_json::Value::String(value_str),
        })
        .into_response()
    }
}

/// DELETE /api/config/prop?path=channels.matrix.allowed-users
///
/// Resets the field to its declared default. For `Option<T>` fields, this
/// sets to `None`. For secrets, response carries only `{path, populated: false}`.
///
/// The current implementation routes through `set_prop` with an empty string,
/// which exercises the same validator path. A more semantically pure reset
/// (re-deriving the field's literal default) is a refinement for a later step.
pub async fn handle_prop_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PropQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut new_config = state.config.lock().clone();
    let info = match lookup_prop_field(&new_config, &q.path) {
        Some(info) => info,
        None => return error_response(ConfigApiError::path_not_found(&q.path)),
    };

    if let Err(e) = new_config.set_prop(&q.path, "") {
        return error_response(map_prop_error(e, &q.path));
    }

    if let Err(e) = new_config.validate() {
        return error_response(ConfigApiError::from_validation(e).with_path(&q.path));
    }

    if let Err(e) = persist_and_swap(&state, new_config).await {
        return error_response(e);
    }

    if info.is_secret || info.derived_from_secret {
        axum::Json(SecretResponse {
            path: q.path,
            populated: false,
        })
        .into_response()
    } else {
        axum::Json(PropResponse {
            path: q.path,
            value: serde_json::Value::Null,
        })
        .into_response()
    }
}

/// GET /api/config/list?prefix=providers
///
/// Enumerates every property the schema exposes. Secret entries appear as
/// `{path, populated}` with `value: None`; non-secrets carry the display
/// value. Optional `prefix` query filters entries whose path starts with it.
pub async fn handle_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let prefix = q.prefix.as_deref();

    let entries: Vec<ListEntry> = config
        .prop_fields()
        .into_iter()
        .filter(|info| match prefix {
            Some(p) => info.name.starts_with(p),
            None => true,
        })
        .map(|info| {
            let populated = info.display_value != "<unset>";
            let is_sensitive = info.is_secret || info.derived_from_secret;
            let value = if is_sensitive {
                None
            } else {
                Some(serde_json::Value::String(info.display_value.clone()))
            };
            let section = Section::from_path(&info.name).and_then(Section::as_path_prefix);
            ListEntry {
                path: info.name,
                category: info.category.to_string(),
                value,
                populated,
                is_secret: is_sensitive,
                onboard_section: section,
            }
        })
        .collect();

    axum::Json(ListResponse { entries }).into_response()
}

/// OPTIONS /api/config — whole-config schema (capabilities, not values)
///
/// Returns the JSON Schema document for the `Config` type. Distinguishes CORS
/// preflight (carries `Access-Control-Request-Method`) from schema-discovery
/// requests; preflight gets the standard CORS response only.
///
/// Static per build — clients should cache via the build-time ETag.
pub async fn handle_options_config(headers: HeaderMap) -> Response {
    // CORS preflight short-circuit
    if headers.contains_key("access-control-request-method") {
        let mut response = StatusCode::NO_CONTENT.into_response();
        let h = response.headers_mut();
        h.insert(
            "Access-Control-Allow-Methods",
            HeaderValue::from_static("GET, PUT, PATCH, OPTIONS"),
        );
        h.insert(
            "Access-Control-Allow-Headers",
            HeaderValue::from_static("Authorization, Content-Type, If-None-Match"),
        );
        return response;
    }

    schema_response("zeroclaw_config_schema_full")
}

/// OPTIONS /api/config/prop?path=providers.fallback — per-field schema fragment
pub async fn handle_options_prop(headers: HeaderMap, Query(q): Query<PropQuery>) -> Response {
    if headers.contains_key("access-control-request-method") {
        let mut response = StatusCode::NO_CONTENT.into_response();
        let h = response.headers_mut();
        h.insert(
            "Access-Control-Allow-Methods",
            HeaderValue::from_static("GET, PUT, DELETE, OPTIONS"),
        );
        h.insert(
            "Access-Control-Allow-Headers",
            HeaderValue::from_static("Authorization, Content-Type, If-None-Match"),
        );
        return response;
    }

    // For now, return the whole-config schema with the path embedded as a
    // hint. Per-path subtree extraction is a follow-up that walks the schema
    // tree by JSON Pointer; the response shape is correct, the content is
    // over-broad for one round-trip's worth of work.
    let mut body = schema_body_value();
    body["x-zeroclaw-requested-path"] = serde_json::Value::String(q.path);
    let etag = build_etag();
    let mut response = (StatusCode::OK, axum::Json(body)).into_response();
    response.headers_mut().insert(
        header::ALLOW,
        HeaderValue::from_static("GET, PUT, DELETE, OPTIONS"),
    );
    response
        .headers_mut()
        .insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    response
}

fn schema_response(_label: &'static str) -> Response {
    let body = schema_body_value();
    let etag = build_etag();
    let mut response = (StatusCode::OK, axum::Json(body)).into_response();
    response.headers_mut().insert(
        header::ALLOW,
        HeaderValue::from_static("GET, PUT, PATCH, OPTIONS"),
    );
    response
        .headers_mut()
        .insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    response
}

#[cfg(feature = "schema-export")]
fn schema_body_value() -> serde_json::Value {
    let schema = schemars::schema_for!(zeroclaw_config::schema::Config);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

#[cfg(not(feature = "schema-export"))]
fn schema_body_value() -> serde_json::Value {
    serde_json::json!({
        "error": "schema-export feature not enabled in this build",
    })
}

/// Stable ETag: schemars output is deterministic per build; we hash the
/// rendered JSON. Cheap because the OPTIONS response is cached client-side
/// via `If-None-Match` after the first request.
fn build_etag() -> String {
    use std::hash::{Hash, Hasher};
    let body = schema_body_value().to_string();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    format!("\"{:016x}\"", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_to_setprop_string_handles_scalars() {
        assert_eq!(
            json_to_setprop_string(&serde_json::Value::Bool(true)).unwrap(),
            "true"
        );
        assert_eq!(
            json_to_setprop_string(&serde_json::Value::String("hello".into())).unwrap(),
            "hello"
        );
        assert_eq!(
            json_to_setprop_string(&serde_json::Value::Null).unwrap(),
            ""
        );
    }

    #[test]
    fn json_to_setprop_string_serializes_arrays() {
        let arr = serde_json::json!(["a", "b"]);
        let s = json_to_setprop_string(&arr).unwrap();
        assert!(s.contains("a"));
        assert!(s.contains("b"));
    }

    #[test]
    fn map_prop_error_classifies_unknown_property() {
        let err = anyhow::anyhow!("Unknown property 'foo.bar'");
        let api_err = map_prop_error(err, "foo.bar");
        assert_eq!(api_err.code, ConfigApiCode::PathNotFound);
    }

    #[test]
    fn map_prop_error_falls_back_to_validation() {
        let err = anyhow::anyhow!("type mismatch: expected u64");
        let api_err = map_prop_error(err, "scheduler.max_concurrent");
        assert_eq!(api_err.code, ConfigApiCode::ValidationFailed);
    }
}
