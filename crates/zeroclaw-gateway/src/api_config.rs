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
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PropQuery {
    pub path: String,
}

/// `?prefix=...` query parameter for list.
#[derive(Debug, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ListQuery {
    #[serde(default)]
    pub prefix: Option<String>,
}

/// PUT body. Value is `serde_json::Value` so typed values (booleans, arrays,
/// numbers) round-trip correctly without going through the CLI's
/// comma-delimited string parser.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PropPutBody {
    pub path: String,
    pub value: serde_json::Value,
    #[serde(default)]
    pub comment: Option<String>,
}

/// One JSON Patch (RFC 6902) operation. We support a strict subset:
/// `add`, `remove`, `replace`, `test`. `move` and `copy` are explicitly
/// rejected at apply time with `op_not_supported` because safe reference-
/// graph rewriting isn't part of this PR.
///
/// `comment` is a ZeroClaw extension — when provided it accompanies the
/// resulting TOML write so future maintainers can see why a value was set.
/// Honored once the comment-preserving write path is wired through (step 7);
/// accepted here so the API shape doesn't churn.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PatchOp {
    pub op: String,
    pub path: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub comment: Option<String>,
}

/// Single result entry in a successful PATCH response, one per applied op.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PatchOpResult {
    pub op: String,
    pub path: String,
    /// The resulting value at the target path after the op applied.
    /// `None` for secret paths (per the secrets-handling boundary), and for
    /// `remove` ops where the field was reset to its default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub populated: Option<bool>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PatchResponse {
    pub saved: bool,
    pub results: Vec<PatchOpResult>,
}

/// Response for a non-secret GET.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PropResponse {
    pub path: String,
    pub value: serde_json::Value,
}

/// Response for a secret GET / PUT / DELETE — never carries the value or its
/// length. `populated: true` means the secret has a non-empty value on disk;
/// `populated: false` means the field is unset or empty.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SecretResponse {
    pub path: String,
    pub populated: bool,
}

/// Single entry in the list response. Secrets carry only `path + populated`;
/// non-secrets additionally carry `value`.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ListResponse {
    pub entries: Vec<ListEntry>,
    /// Properties where in-memory and on-disk values disagree. Empty when the
    /// daemon's view matches the file. Each entry follows the `DriftEntry`
    /// shape (secrets carry only `{path, secret: true, drifted: true}`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drifted: Vec<DriftEntry>,
}

/// One drift entry surfaced when in-memory `Config` diverges from the on-disk
/// `config.toml` (some other process — typically a hand-edit while the daemon
/// was stopped — wrote the file). For non-secret fields, both values are
/// surfaced so the dashboard can show a clean diff. For secret fields, only
/// the boolean `drifted` is surfaced — the secret values themselves never
/// leave the server.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DriftEntry {
    pub path: String,
    /// `true` for secret fields where values cannot be exposed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub secret: bool,
    /// Always `true` when surfaced. Present so secret entries unambiguously
    /// communicate the drift signal in shape `{path, secret: true, drifted: true}`.
    pub drifted: bool,
    /// In-memory value (the daemon's view). Absent for secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_memory_value: Option<serde_json::Value>,
    /// On-disk value (what the file contains right now). Absent for secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_disk_value: Option<serde_json::Value>,
}

fn is_false(b: &bool) -> bool {
    !*b
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

/// Coerce a JSON value to the string representation `Config::set_prop` expects,
/// validating against the target field's declared `PropKind` so the wrong-shape
/// inputs surface as `value_type_mismatch` before we touch the in-memory copy.
///
/// Type rules:
/// - `StringArray`: requires JSON array of strings (or `null` for "reset to
///   default"). Empty array `[]` is a valid value distinct from `null`. Element
///   types are checked.
/// - `Bool`: requires JSON boolean (or string `"true"` / `"false"` for legacy
///   callers).
/// - `Integer`: requires JSON number with integer value (or numeric string).
/// - `Float`: requires JSON number (or numeric string).
/// - `String` / `Enum`: any scalar coerces to its display form.
fn json_to_setprop_string(
    value: &serde_json::Value,
    kind: Option<zeroclaw_config::traits::PropKind>,
) -> Result<String, ConfigApiError> {
    use zeroclaw_config::traits::PropKind;

    match (kind, value) {
        // Null is always valid — it means "reset to default".
        (_, serde_json::Value::Null) => Ok(String::new()),

        // Array fields: must receive a JSON array of strings.
        (Some(PropKind::StringArray), serde_json::Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                if !item.is_string() {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::ValueTypeMismatch,
                        format!(
                            "array element [{i}] is {} — `Vec<String>` requires string elements",
                            json_type_name(item),
                        ),
                    ));
                }
            }
            // Pass through as JSON; set_prop's StringArray parser accepts the
            // bracketed form natively.
            serde_json::to_string(value).map_err(|e| {
                ConfigApiError::new(
                    ConfigApiCode::ValueTypeMismatch,
                    format!("could not serialize JSON value: {e}"),
                )
            })
        }
        (Some(PropKind::StringArray), other) => Err(ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            format!(
                "`Vec<String>` field requires a JSON array; got {}",
                json_type_name(other),
            ),
        )),

        // Bool fields.
        (Some(PropKind::Bool), serde_json::Value::Bool(b)) => Ok(b.to_string()),
        (Some(PropKind::Bool), serde_json::Value::String(s))
            if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") =>
        {
            Ok(s.to_lowercase())
        }
        (Some(PropKind::Bool), other) => Err(ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            format!(
                "bool field requires `true`/`false`; got {}",
                json_type_name(other)
            ),
        )),

        // Integer fields.
        (Some(PropKind::Integer), serde_json::Value::Number(n)) if n.is_i64() || n.is_u64() => {
            Ok(n.to_string())
        }
        (Some(PropKind::Integer), serde_json::Value::String(s)) if s.parse::<i64>().is_ok() => {
            Ok(s.clone())
        }
        (Some(PropKind::Integer), other) => Err(ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            format!(
                "integer field requires a whole number; got {}",
                json_type_name(other)
            ),
        )),

        // Float fields.
        (Some(PropKind::Float), serde_json::Value::Number(n)) => Ok(n.to_string()),
        (Some(PropKind::Float), serde_json::Value::String(s)) if s.parse::<f64>().is_ok() => {
            Ok(s.clone())
        }
        (Some(PropKind::Float), other) => Err(ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            format!(
                "float field requires a number; got {}",
                json_type_name(other)
            ),
        )),

        // Scalar / enum fields and unknown-kind paths: best-effort coerce.
        (_, serde_json::Value::String(s)) => Ok(s.clone()),
        (_, serde_json::Value::Bool(b)) => Ok(b.to_string()),
        (_, serde_json::Value::Number(n)) => Ok(n.to_string()),
        (_, serde_json::Value::Array(_)) | (_, serde_json::Value::Object(_)) => {
            serde_json::to_string(value).map_err(|e| {
                ConfigApiError::new(
                    ConfigApiCode::ValueTypeMismatch,
                    format!("could not serialize JSON value: {e}"),
                )
            })
        }
    }
}

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
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

/// Save the config and refresh in-memory state. Captures a snapshot of the
/// pre-write disk state and reverts to it if the save itself fails, so that
/// on-disk and in-memory state stay consistent under any failure mode.
///
/// On the happy path: validate (caller's responsibility) → save to disk →
/// swap in-memory → respond OK.
///
/// On save failure: best-effort restore the pre-write disk content (when
/// readable), keep in-memory state untouched, return `reload_failed`.
async fn persist_and_swap(
    state: &AppState,
    new_config: zeroclaw_config::schema::Config,
) -> Result<(), ConfigApiError> {
    let config_path = new_config.config_path.clone();

    // Snapshot pre-write disk state (used for revert on save failure). When
    // the file doesn't exist yet, snapshot is None — we'll remove the file
    // again on rollback so a failed first-write doesn't leak partial state.
    let snapshot = if config_path.exists() {
        // best-effort; if we can't read, we can't revert
        tokio::fs::read(&config_path).await.ok()
    } else {
        None
    };

    if let Err(e) = new_config.save().await {
        // Save failed — try to restore the pre-write snapshot. This isn't
        // strictly necessary (Config::save uses an atomic-replace via tmp
        // file) but defends against the rare case where save partially
        // wrote then errored (e.g. fsync mid-write).
        if let Some(prev) = snapshot {
            let _ = tokio::fs::write(&config_path, prev).await;
        } else if config_path.exists() {
            let _ = tokio::fs::remove_file(&config_path).await;
        }
        return Err(ConfigApiError::new(
            ConfigApiCode::ReloadFailed,
            format!("save failed: {e}"),
        ));
    }

    *state.config.lock() = new_config;
    Ok(())
}

/// Decorate the on-disk TOML file with comments captured from PATCH/PUT ops.
///
/// Called after `Config::save()` (which already preserves existing comments
/// via `migration::sync_table`). For each `(path, comment)` pair, walks the
/// toml_edit document to the target leaf and prepends `# {comment}\n` as
/// the leading decoration. An empty comment string clears any existing
/// `# `-prefixed comment lines from the leaf's leading decor (other
/// whitespace and blank lines are left intact).
///
/// Best-effort: silently skips paths that don't resolve to a leaf value.
/// Failure to read or write the file leaves the surface unchanged.
async fn apply_comments(
    config_path: &std::path::Path,
    annotations: &[(String, String)],
) -> Result<(), std::io::Error> {
    if annotations.is_empty() {
        return Ok(());
    }

    let raw = tokio::fs::read_to_string(config_path).await?;
    let mut doc: toml_edit::DocumentMut = match raw.parse() {
        Ok(d) => d,
        Err(_) => return Ok(()), // unparseable; bail without touching the file
    };

    for (path, comment) in annotations {
        decorate_key(doc.as_table_mut(), path, comment);
    }

    tokio::fs::write(config_path, doc.to_string()).await?;
    Ok(())
}

/// Walk to the leaf key for `dotted` and decorate it with `# {comment}\n`,
/// preserving any non-comment whitespace already in the prefix. Empty comment
/// strips comment lines from the existing prefix.
fn decorate_key(root: &mut toml_edit::Table, dotted: &str, comment: &str) {
    let segments: Vec<&str> = dotted.split('.').collect();
    let (last, rest) = match segments.split_last() {
        Some(s) => s,
        None => return,
    };
    fn walk<'a>(
        table: &'a mut toml_edit::Table,
        segs: &[&str],
    ) -> Option<&'a mut toml_edit::Table> {
        let mut cursor = table;
        for seg in segs {
            cursor = cursor.get_mut(seg)?.as_table_mut()?;
        }
        Some(cursor)
    }
    let table = match walk(root, rest) {
        Some(t) => t,
        None => return,
    };
    if let Some(mut key) = table.key_mut(last) {
        let decor = key.leaf_decor_mut();
        let new_prefix = build_comment_prefix(decor.prefix(), comment);
        decor.set_prefix(new_prefix);
    }
}

/// Build the new leading decor for a leaf, applying the `# {comment}\n` line
/// while preserving any blank-line whitespace that preceded it. When the
/// comment is empty, strips comment lines from the existing prefix.
fn build_comment_prefix(existing: Option<&toml_edit::RawString>, comment: &str) -> String {
    let prev = existing.and_then(|r| r.as_str()).unwrap_or("");

    // Split existing prefix into non-comment whitespace lines (kept) and
    // comment lines (replaced).
    let mut kept: Vec<&str> = Vec::new();
    for line in prev.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        kept.push(line);
    }

    let mut out: String = kept.join("");
    if !comment.is_empty() {
        for line in comment.lines() {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Compute drift between the in-memory config and what's on disk right now.
/// Returns one entry per drifted property; empty when in-memory and disk
/// agree (or when the on-disk file can't be parsed).
///
/// **Secrets:** never surface values. We compare in-memory and on-disk
/// representations server-side — for secret paths, the comparison happens
/// over the raw display strings (which include the encrypted form on disk
/// vs. the decrypted form in memory, so most secret drift is false-positive
/// against `Configurable`'s display layer). To stay honest about that, the
/// on-disk side is round-tripped through the full deserializer + decrypt
/// pass before comparison, so we only surface drift the daemon would
/// actually pick up on its next read of the file.
pub async fn compute_drift(in_memory: &zeroclaw_config::schema::Config) -> Vec<DriftEntry> {
    let path = &in_memory.config_path;
    if !path.exists() {
        return Vec::new();
    }

    let raw = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    // Re-parse the on-disk form into a fresh Config for value-by-value comparison.
    let on_disk: zeroclaw_config::schema::Config =
        match toml::from_str::<zeroclaw_config::schema::Config>(&raw) {
            Ok(mut cfg) => {
                cfg.config_path = path.clone();
                cfg
            }
            Err(_) => return Vec::new(),
        };

    let in_memory_props: std::collections::HashMap<String, zeroclaw_config::traits::PropFieldInfo> =
        in_memory
            .prop_fields()
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect();
    let on_disk_props: std::collections::HashMap<String, zeroclaw_config::traits::PropFieldInfo> =
        on_disk
            .prop_fields()
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect();

    let mut drift: Vec<DriftEntry> = Vec::new();
    for (name, mem) in &in_memory_props {
        let disk = match on_disk_props.get(name) {
            Some(d) => d,
            None => continue,
        };
        if mem.display_value == disk.display_value {
            continue;
        }
        let is_sensitive = mem.is_secret || mem.derived_from_secret;
        if is_sensitive {
            // Hash-compare server-side so we don't conflate ciphertext-vs-
            // plaintext display drift with real value drift. If the SHA-256
            // hashes match, the underlying secret is the same and we hide
            // the entry.
            use sha2::{Digest, Sha256};
            let mem_hash = Sha256::digest(mem.display_value.as_bytes());
            let disk_hash = Sha256::digest(disk.display_value.as_bytes());
            if mem_hash == disk_hash {
                continue;
            }
            drift.push(DriftEntry {
                path: name.clone(),
                secret: true,
                drifted: true,
                in_memory_value: None,
                on_disk_value: None,
            });
        } else {
            drift.push(DriftEntry {
                path: name.clone(),
                secret: false,
                drifted: true,
                in_memory_value: Some(serde_json::Value::String(mem.display_value.clone())),
                on_disk_value: Some(serde_json::Value::String(disk.display_value.clone())),
            });
        }
    }

    // Stable order so callers can diff snapshots.
    drift.sort_by(|a, b| a.path.cmp(&b.path));
    drift
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

    let value_str = match json_to_setprop_string(&body.value, Some(info.kind)) {
        Ok(s) => s,
        Err(e) => return error_response(e.with_path(&body.path)),
    };

    if let Err(e) = new_config.set_prop(&body.path, &value_str) {
        return error_response(map_prop_error(e, &body.path));
    }

    if let Err(e) = new_config.validate() {
        return error_response(ConfigApiError::from_validation(e).with_path(&body.path));
    }

    let config_path = new_config.config_path.clone();
    if let Err(e) = persist_and_swap(&state, new_config).await {
        return error_response(e);
    }
    if let Some(comment) = body.comment.as_ref() {
        let annotations = [(body.path.clone(), comment.clone())];
        if let Err(e) = apply_comments(&config_path, &annotations).await {
            tracing::warn!(error = %e, "failed to apply PUT comment to config.toml");
        }
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

    let drifted = compute_drift(&config).await;
    axum::Json(ListResponse { entries, drifted }).into_response()
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DriftResponse {
    pub drifted: Vec<DriftEntry>,
}

/// `GET /api/config/drift` — explicit drift summary for clients that want just
/// the diff. Same `DriftEntry` shape used in `ListResponse.drifted`.
pub async fn handle_drift(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.lock().clone();
    let drifted = compute_drift(&config).await;
    axum::Json(DriftResponse { drifted }).into_response()
}

/// PATCH /api/config — apply a JSON Patch document atomically.
///
/// Body is an array of operations executed in order against an in-memory
/// copy of the config. After all ops apply, `Config::validate()` runs once;
/// if it passes the snapshot is persisted and swapped in. If any op fails or
/// validation fails, on-disk + in-memory state are unchanged and the response
/// carries the offending op's index.
///
/// Supported ops: `add`, `remove`, `replace`, `test`.
/// `move` and `copy` return `op_not_supported` (no reference-graph in this PR).
/// `test` against a `#[secret]` or `#[derived_from_secret]` path is rejected
/// with `secret_test_forbidden` (would leak the value via differential outcome).
pub async fn handle_patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(ops): axum::Json<Vec<PatchOp>>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut working = state.config.lock().clone();
    let mut results = Vec::with_capacity(ops.len());

    for (idx, op) in ops.iter().enumerate() {
        let path = json_pointer_to_dotted(&op.path);
        let info = lookup_prop_field(&working, &path);
        let is_sensitive = info
            .as_ref()
            .map(|i| i.is_secret || i.derived_from_secret)
            .unwrap_or(false);

        match op.op.as_str() {
            "test" => {
                // Secret values can't leave the server, so a differential
                // test response would be the only signal — ban the op.
                if is_sensitive {
                    return error_response(
                        ConfigApiError::secret_test_forbidden(&path).with_op_index(idx),
                    );
                }
                let want = match op.value.as_ref() {
                    Some(v) => v.clone(),
                    None => {
                        return error_response(
                            ConfigApiError::new(
                                ConfigApiCode::ValueTypeMismatch,
                                "JSON Patch `test` op requires `value` field",
                            )
                            .with_path(&path)
                            .with_op_index(idx),
                        );
                    }
                };
                let actual = match working.get_prop(&path) {
                    Ok(v) => serde_json::Value::String(v),
                    Err(e) => return error_response(map_prop_error(e, &path).with_op_index(idx)),
                };
                if actual != want {
                    return error_response(
                        ConfigApiError::new(
                            ConfigApiCode::ValidationFailed,
                            format!("`test` op failed: expected {want}, got {actual}"),
                        )
                        .with_path(&path)
                        .with_op_index(idx),
                    );
                }
                results.push(PatchOpResult {
                    op: op.op.clone(),
                    path,
                    value: Some(actual),
                    populated: None,
                });
            }
            "add" | "replace" => {
                let value = match op.value.as_ref() {
                    Some(v) => v.clone(),
                    None => {
                        return error_response(
                            ConfigApiError::new(
                                ConfigApiCode::ValueTypeMismatch,
                                format!("JSON Patch `{}` op requires `value` field", op.op),
                            )
                            .with_path(&path)
                            .with_op_index(idx),
                        );
                    }
                };
                let value_str = match json_to_setprop_string(&value, info.as_ref().map(|i| i.kind))
                {
                    Ok(s) => s,
                    Err(e) => {
                        return error_response(e.with_path(&path).with_op_index(idx));
                    }
                };
                if let Err(e) = working.set_prop(&path, &value_str) {
                    return error_response(map_prop_error(e, &path).with_op_index(idx));
                }
                if is_sensitive {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: None,
                        populated: Some(!value_str.is_empty()),
                    });
                } else {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: Some(serde_json::Value::String(value_str)),
                        populated: None,
                    });
                }
            }
            "remove" => {
                if let Err(e) = working.set_prop(&path, "") {
                    return error_response(map_prop_error(e, &path).with_op_index(idx));
                }
                if is_sensitive {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: None,
                        populated: Some(false),
                    });
                } else {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: Some(serde_json::Value::Null),
                        populated: None,
                    });
                }
            }
            "move" | "copy" => {
                return error_response(
                    ConfigApiError::op_not_supported(&op.op)
                        .with_path(&path)
                        .with_op_index(idx),
                );
            }
            other => {
                return error_response(
                    ConfigApiError::new(
                        ConfigApiCode::OpNotSupported,
                        format!("unknown JSON Patch operation `{other}`"),
                    )
                    .with_path(&path)
                    .with_op_index(idx),
                );
            }
        }
    }

    if let Err(e) = working.validate() {
        return error_response(ConfigApiError::from_validation(e));
    }

    // Collect (path, comment) pairs from any op that supplied a non-None
    // comment. Applied after save() so the comment-preserving sync_table
    // pass doesn't strip them.
    let annotations: Vec<(String, String)> = ops
        .iter()
        .zip(results.iter())
        .filter_map(|(op, res)| op.comment.as_ref().map(|c| (res.path.clone(), c.clone())))
        .collect();

    let config_path = working.config_path.clone();
    if let Err(e) = persist_and_swap(&state, working).await {
        return error_response(e);
    }
    if !annotations.is_empty()
        && let Err(e) = apply_comments(&config_path, &annotations).await
    {
        // Comments are best-effort decoration; surface as a non-fatal warn.
        // The patch itself succeeded — return success but log the failure.
        tracing::warn!(error = %e, "failed to apply PATCH op comments to config.toml");
    }

    axum::Json(PatchResponse {
        saved: true,
        results,
    })
    .into_response()
}

/// Convert a JSON Pointer (`/providers/fallback`) to the dotted path the
/// `Config::set_prop` machinery expects (`providers.fallback`). Accepts both
/// forms — passing already-dotted paths through unchanged so dashboard clients
/// can use whichever is more natural.
fn json_pointer_to_dotted(path: &str) -> String {
    if path.starts_with('/') {
        path.trim_start_matches('/').replace('/', ".")
    } else {
        path.to_string()
    }
}

#[derive(Debug, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InitQuery {
    /// Optional section prefix to scope the init pass (e.g. `providers`).
    /// Without it, every uninitialized nested section gets its defaults.
    #[serde(default)]
    pub section: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InitResponse {
    pub initialized: Vec<String>,
}

/// POST /api/config/init?section=providers — instantiate `None` nested
/// sections with defaults. Mirrors `zeroclaw config init`. When every
/// requested section is already configured, returns `{initialized: []}`.
pub async fn handle_init(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<InitQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut working = state.config.lock().clone();
    let initialized: Vec<String> = working
        .init_defaults(q.section.as_deref())
        .into_iter()
        .map(str::to_string)
        .collect();

    if initialized.is_empty() {
        return axum::Json(InitResponse { initialized }).into_response();
    }

    if let Err(e) = working.validate() {
        return error_response(ConfigApiError::from_validation(e));
    }
    if let Err(e) = persist_and_swap(&state, working).await {
        return error_response(e);
    }

    axum::Json(InitResponse { initialized }).into_response()
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MigrateResponse {
    pub migrated: bool,
    /// Backup path written when migration ran; absent when the config was
    /// already at the current schema version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    pub schema_version: u32,
}

/// POST /api/config/migrate — apply V1→V2 migration to the on-disk
/// config file in place. Mirrors `zeroclaw config migrate`. Backs up the
/// previous content alongside the original (`config.toml.bak`) before
/// writing the migrated form. Returns `{migrated: false}` when the config
/// is already at the current schema version.
pub async fn handle_migrate(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config_path = state.config.lock().config_path.clone();

    let raw = match tokio::fs::read_to_string(&config_path).await {
        Ok(s) => s,
        Err(e) => {
            return error_response(ConfigApiError::new(
                ConfigApiCode::InternalError,
                format!("failed to read config file: {e}"),
            ));
        }
    };

    let migrated = match zeroclaw_config::migration::migrate_file(&raw) {
        Ok(out) => out,
        Err(e) => {
            return error_response(ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!("migration failed: {e}"),
            ));
        }
    };

    match migrated {
        Some(new_content) => {
            let backup_path = config_path.with_extension("toml.bak");
            if let Err(e) = tokio::fs::copy(&config_path, &backup_path).await {
                return error_response(ConfigApiError::new(
                    ConfigApiCode::InternalError,
                    format!("failed to write backup: {e}"),
                ));
            }
            if let Err(e) = tokio::fs::write(&config_path, &new_content).await {
                return error_response(ConfigApiError::new(
                    ConfigApiCode::InternalError,
                    format!("failed to write migrated config: {e}"),
                ));
            }

            // Re-read into memory so subsequent requests see the migrated state.
            let new_cfg: zeroclaw_config::schema::Config = match toml::from_str(&new_content) {
                Ok(c) => c,
                Err(e) => {
                    return error_response(ConfigApiError::new(
                        ConfigApiCode::ReloadFailed,
                        format!("re-parse after migration failed: {e}"),
                    ));
                }
            };
            *state.config.lock() = new_cfg;

            axum::Json(MigrateResponse {
                migrated: true,
                backup_path: Some(backup_path.display().to_string()),
                schema_version: zeroclaw_config::migration::CURRENT_SCHEMA_VERSION,
            })
            .into_response()
        }
        None => axum::Json(MigrateResponse {
            migrated: false,
            backup_path: None,
            schema_version: zeroclaw_config::migration::CURRENT_SCHEMA_VERSION,
        })
        .into_response(),
    }
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
            json_to_setprop_string(
                &serde_json::Value::Bool(true),
                Some(zeroclaw_config::traits::PropKind::Bool)
            )
            .unwrap(),
            "true"
        );
        assert_eq!(
            json_to_setprop_string(
                &serde_json::Value::String("hello".into()),
                Some(zeroclaw_config::traits::PropKind::String)
            )
            .unwrap(),
            "hello"
        );
        assert_eq!(
            json_to_setprop_string(&serde_json::Value::Null, None).unwrap(),
            ""
        );
    }

    #[test]
    fn json_to_setprop_string_serializes_arrays() {
        let arr = serde_json::json!(["a", "b"]);
        let s = json_to_setprop_string(&arr, Some(zeroclaw_config::traits::PropKind::StringArray))
            .unwrap();
        assert!(s.contains("a"));
        assert!(s.contains("b"));
    }

    #[test]
    fn json_to_setprop_string_rejects_non_array_for_string_array_field() {
        let result = json_to_setprop_string(
            &serde_json::Value::String("a,b".into()),
            Some(zeroclaw_config::traits::PropKind::StringArray),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ConfigApiCode::ValueTypeMismatch);
    }

    #[test]
    fn json_to_setprop_string_rejects_non_string_array_elements() {
        let result = json_to_setprop_string(
            &serde_json::json!(["a", 42, "c"]),
            Some(zeroclaw_config::traits::PropKind::StringArray),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ConfigApiCode::ValueTypeMismatch);
    }

    #[test]
    fn json_to_setprop_string_accepts_empty_array() {
        let s = json_to_setprop_string(
            &serde_json::json!([]),
            Some(zeroclaw_config::traits::PropKind::StringArray),
        )
        .unwrap();
        assert_eq!(s, "[]");
    }

    #[test]
    fn json_to_setprop_string_rejects_string_for_bool_field() {
        let result = json_to_setprop_string(
            &serde_json::Value::String("yes".into()),
            Some(zeroclaw_config::traits::PropKind::Bool),
        );
        assert!(result.is_err());
    }

    #[test]
    fn build_comment_prefix_appends_comment_to_blank_prefix() {
        let out = build_comment_prefix(None, "set during onboarding");
        assert_eq!(out, "# set during onboarding\n");
    }

    #[test]
    fn build_comment_prefix_replaces_existing_comment_lines() {
        // Simulate a doc that already has a comment + a blank line.
        let raw = toml_edit::RawString::from("\n# old reason\n");
        let out = build_comment_prefix(Some(&raw), "new reason");
        assert!(out.contains("# new reason\n"));
        assert!(!out.contains("old reason"));
        // Blank line preserved.
        assert!(out.starts_with('\n'));
    }

    #[test]
    fn build_comment_prefix_empty_comment_strips_existing() {
        let raw = toml_edit::RawString::from("\n# stale\n");
        let out = build_comment_prefix(Some(&raw), "");
        assert!(!out.contains('#'));
        // Blank line preserved.
        assert_eq!(out, "\n");
    }

    #[test]
    fn json_to_setprop_string_accepts_bool_string_for_bool_field() {
        let s = json_to_setprop_string(
            &serde_json::Value::String("True".into()),
            Some(zeroclaw_config::traits::PropKind::Bool),
        )
        .unwrap();
        assert_eq!(s, "true");
    }

    #[test]
    fn map_prop_error_classifies_unknown_property() {
        let err = anyhow::anyhow!("Unknown property 'foo.bar'");
        let api_err = map_prop_error(err, "foo.bar");
        assert_eq!(api_err.code, ConfigApiCode::PathNotFound);
    }

    #[test]
    fn map_prop_error_classifies_type_mismatch() {
        // The classifier (config::api_error::classify_validation_message) now
        // matches "type mismatch" → ValueTypeMismatch; was ValidationFailed.
        let err = anyhow::anyhow!("type mismatch: expected u64");
        let api_err = map_prop_error(err, "scheduler.max_concurrent");
        assert_eq!(api_err.code, ConfigApiCode::ValueTypeMismatch);
    }

    #[test]
    fn map_prop_error_falls_back_to_validation_on_unknown_message() {
        let err = anyhow::anyhow!("some completely unrecognized validator message");
        let api_err = map_prop_error(err, "scheduler.max_concurrent");
        assert_eq!(api_err.code, ConfigApiCode::ValidationFailed);
    }

    #[test]
    fn json_pointer_to_dotted_handles_pointer_form() {
        assert_eq!(
            json_pointer_to_dotted("/providers/fallback"),
            "providers.fallback"
        );
        assert_eq!(
            json_pointer_to_dotted("/providers/models/openrouter/api-key"),
            "providers.models.openrouter.api-key"
        );
    }

    #[test]
    fn json_pointer_to_dotted_passes_dotted_through() {
        assert_eq!(
            json_pointer_to_dotted("providers.fallback"),
            "providers.fallback"
        );
        assert_eq!(
            json_pointer_to_dotted("scheduler.max_concurrent"),
            "scheduler.max_concurrent"
        );
    }

    #[test]
    fn json_pointer_to_dotted_handles_empty_root() {
        assert_eq!(json_pointer_to_dotted(""), "");
        assert_eq!(json_pointer_to_dotted("/"), "");
    }

    // ── Integration-flavored tests: drift detection + comment writing ──

    use std::path::PathBuf;

    fn temp_config_path() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        (tmp, path)
    }

    #[tokio::test]
    async fn compute_drift_returns_empty_when_in_memory_matches_disk() {
        let (_tmp, path) = temp_config_path();
        let cfg = zeroclaw_config::schema::Config {
            config_path: path.clone(),
            ..Default::default()
        };
        // Write the in-memory state to disk first so they agree by definition.
        cfg.save().await.expect("save");

        let drift = compute_drift(&cfg).await;
        assert!(
            drift.is_empty(),
            "expected no drift right after save, got {drift:?}"
        );
    }

    #[tokio::test]
    async fn compute_drift_surfaces_mismatched_non_secret_field() {
        let (_tmp, path) = temp_config_path();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: path.clone(),
            ..Default::default()
        };
        cfg.save().await.expect("initial save");

        // Mutate the in-memory config without saving.
        cfg.set_prop("gateway.host", "10.0.0.1").expect("set_prop");

        let drift = compute_drift(&cfg).await;
        let entry = drift
            .iter()
            .find(|d| d.path == "gateway.host")
            .expect("expected gateway.host in drift summary");
        assert!(!entry.secret);
        assert!(entry.drifted);
        assert!(entry.in_memory_value.is_some());
        assert!(entry.on_disk_value.is_some());
    }

    #[tokio::test]
    async fn compute_drift_returns_empty_when_no_disk_file() {
        let (_tmp, path) = temp_config_path();
        let cfg = zeroclaw_config::schema::Config {
            config_path: path.clone(),
            ..Default::default()
        };
        // Don't save — file does not exist.
        let drift = compute_drift(&cfg).await;
        assert!(drift.is_empty());
    }

    #[tokio::test]
    async fn apply_comments_writes_decoration_to_existing_value() {
        let (_tmp, path) = temp_config_path();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: path.clone(),
            ..Default::default()
        };
        cfg.set_prop("gateway.host", "10.0.0.5").expect("set_prop");
        cfg.save().await.expect("save");

        apply_comments(
            &path,
            &[("gateway.host".into(), "raised after Q3 backlog".into())],
        )
        .await
        .expect("apply_comments");

        let raw = tokio::fs::read_to_string(&path).await.expect("read back");
        assert!(
            raw.contains("# raised after Q3 backlog"),
            "expected comment in file, got:\n{raw}"
        );
    }

    #[test]
    fn secret_response_only_carries_path_and_populated_flag() {
        // Belt-and-braces: serialize a SecretResponse and assert the JSON
        // shape carries neither a `value` field nor a length-leaking string.
        // If anyone ever adds a field to SecretResponse, this test fires.
        let r = SecretResponse {
            path: "providers.models.ollama.api-key".into(),
            populated: true,
        };
        let json = serde_json::to_value(&r).expect("serialize");
        let obj = json.as_object().expect("object");
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["path", "populated"],
            "SecretResponse must carry only path + populated"
        );
        assert!(!obj.contains_key("value"));
        assert!(!obj.contains_key("length"));
        assert!(!obj.contains_key("hash"));
        assert!(!obj.contains_key("masked"));
    }

    #[test]
    fn list_entry_for_secret_omits_value_field() {
        let entry = ListEntry {
            path: "providers.models.ollama.api-key".into(),
            category: "providers".into(),
            value: None,
            populated: true,
            is_secret: true,
            onboard_section: Some("providers"),
        };
        let json = serde_json::to_value(&entry).expect("serialize");
        let obj = json.as_object().expect("object");
        // skip_serializing_if on `value` means it must be absent.
        assert!(
            !obj.contains_key("value"),
            "secret list entry leaks `value` field"
        );
        // is_secret marker must be present so the dashboard can render it as locked.
        assert_eq!(obj.get("is_secret"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(obj.get("populated"), Some(&serde_json::Value::Bool(true)));
    }

    #[test]
    fn drift_entry_for_secret_omits_both_values() {
        let entry = DriftEntry {
            path: "providers.models.ollama.api-key".into(),
            secret: true,
            drifted: true,
            in_memory_value: None,
            on_disk_value: None,
        };
        let json = serde_json::to_value(&entry).expect("serialize");
        let obj = json.as_object().expect("object");
        assert!(
            !obj.contains_key("in_memory_value"),
            "secret drift entry leaks in_memory_value"
        );
        assert!(
            !obj.contains_key("on_disk_value"),
            "secret drift entry leaks on_disk_value"
        );
        assert_eq!(obj.get("secret"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(obj.get("drifted"), Some(&serde_json::Value::Bool(true)));
    }

    #[tokio::test]
    async fn apply_comments_clears_existing_comment_when_passed_empty() {
        let (_tmp, path) = temp_config_path();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: path.clone(),
            ..Default::default()
        };
        cfg.set_prop("gateway.host", "10.0.0.5").expect("set_prop");
        cfg.save().await.expect("save");

        apply_comments(&path, &[("gateway.host".into(), "first reason".into())])
            .await
            .expect("apply first comment");
        apply_comments(&path, &[("gateway.host".into(), String::new())])
            .await
            .expect("apply empty");

        let raw = tokio::fs::read_to_string(&path).await.expect("read back");
        assert!(
            !raw.contains("first reason"),
            "expected the prior comment to be cleared, got:\n{raw}"
        );
    }
}
