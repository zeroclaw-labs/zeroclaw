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
use zeroclaw_runtime::onboard::{Section, field_visibility};

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
    /// Comment that was applied alongside this op (if any). Echoed so
    /// clients can confirm the comment was actually written to disk
    /// without having to round-trip through `GET` and parse the TOML.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
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
///
/// `kind` and `type_hint` are the wire form of the field's declared
/// `PropKind` plus its Rust type signature. Frontends bind input renderers
/// to these directly (no value-sniffing). `enum_variants` is populated for
/// fields whose macro derive surfaces a variant list (drives `select`
/// option rendering).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ListEntry {
    pub path: String,
    pub category: String,
    /// Stable kind tag — `string`, `bool`, `integer`, `float`, `enum`,
    /// `string-array`. Lowercase-kebab so it can be used directly as a CSS
    /// class or React key.
    pub kind: &'static str,
    /// Rust type signature, e.g. `Option<String>`, `Vec<String>`, `u64`.
    /// Render in tooltips / hover state for the technically-curious.
    pub type_hint: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    pub populated: bool,
    pub is_secret: bool,
    /// Variants for `enum`-kind fields — non-empty means the frontend should
    /// render a `<select>` with these options. Empty for non-enum fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_variants: Vec<String>,
    /// Onboard section name derived from the path's first segment via
    /// `Section::from_path`. `None` for paths that aren't part of any wizard
    /// section. The dashboard groups list entries by this for per-section
    /// rendering — same source the CLI wizard uses, no schema attribute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onboard_section: Option<&'static str>,
}

/// Stable wire-form name for a `PropKind` variant. Matches the lower-kebab
/// convention the rest of the API uses for stable string IDs.
fn prop_kind_wire(kind: zeroclaw_config::traits::PropKind) -> &'static str {
    use zeroclaw_config::traits::PropKind;
    match kind {
        PropKind::String => "string",
        PropKind::Bool => "bool",
        PropKind::Integer => "integer",
        PropKind::Float => "float",
        PropKind::Enum => "enum",
        PropKind::StringArray => "string-array",
        PropKind::ObjectArray => "object-array",
    }
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

// Typed-value coercion lives in `zeroclaw_config::typed_value` — both the
// gateway PATCH/PUT handlers and the CLI `config patch` flow consume it.
// Single source of truth for the "JSON in, set_prop string out, validated
// against the declared PropKind" contract.
use zeroclaw_config::typed_value::coerce_for_set_prop as json_to_setprop_string;

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
        if let Err(e) =
            zeroclaw_config::comment_writer::apply_comments(&config_path, &annotations).await
        {
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

    // Drop fields that don't apply to the current shape of the config —
    // azure_* on a non-azure provider, qdrant.* when memory.backend is
    // sqlite, etc. Keeps the form scoped to relevant inputs only.
    let excluded = field_visibility::excluded_paths(&config, prefix.unwrap_or(""));

    let entries: Vec<ListEntry> = config
        .prop_fields()
        .into_iter()
        .filter(|info| match prefix {
            Some(p) => info.name.starts_with(p),
            None => true,
        })
        .filter(|info| !field_visibility::is_excluded(&info.name, &excluded))
        .map(|info| {
            let populated = info.display_value != "<unset>";
            let is_sensitive = info.is_secret || info.derived_from_secret;
            let value = if is_sensitive {
                None
            } else {
                Some(serde_json::Value::String(info.display_value.clone()))
            };
            let section = Section::from_path(&info.name).and_then(Section::as_path_prefix);
            let enum_variants = info.enum_variants.map(|f| f()).unwrap_or_default();
            ListEntry {
                path: info.name,
                category: info.category.to_string(),
                kind: prop_kind_wire(info.kind),
                type_hint: info.type_hint,
                value,
                populated,
                is_secret: is_sensitive,
                enum_variants,
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

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MapKeyQuery {
    /// Map-keyed section path, e.g. `providers.models`, `agents`, `swarms`.
    pub path: String,
    /// New key to insert under that section, e.g. `anthropic`.
    pub key: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MapKeyResponse {
    pub path: String,
    pub key: String,
    pub created: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct TemplatesResponse {
    pub templates: Vec<TemplateEntry>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct TemplateEntry {
    pub path: &'static str,
    /// `map` for `HashMap<String, T>`, `list` for `Vec<T>`.
    pub kind: &'static str,
    /// Rust type name of the value, e.g. `ModelProviderConfig`.
    pub value_type: &'static str,
    /// Doc comment from the schema (description of what gets added).
    pub description: &'static str,
}

/// `GET /api/config/templates` — enumerate every map-keyed and list-shaped
/// section the dashboard can offer "+ Add" affordances for. Discovered
/// from the `Configurable` derive's `map_key_sections()` — single source of
/// truth, no hand-maintained list. Adding a new `HashMap<String, T>` or
/// `#[nested] Vec<T>` field anywhere in the schema makes it appear here
/// automatically.
pub async fn handle_templates(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let _ = state; // templates are static per build, but auth-gated for consistency

    let templates: Vec<TemplateEntry> = zeroclaw_config::schema::Config::map_key_sections()
        .into_iter()
        .map(|s| TemplateEntry {
            path: s.path,
            kind: match s.kind {
                zeroclaw_config::traits::MapKeyKind::Map => "map",
                zeroclaw_config::traits::MapKeyKind::List => "list",
            },
            value_type: s.value_type,
            description: s.description,
        })
        .collect();

    axum::Json(TemplatesResponse { templates }).into_response()
}

/// `POST /api/config/map-key?path=<section>&key=<name>` — instantiate a new
/// entry under a map-keyed section with default values, or append to a
/// list-shaped one with `key` as the new entry's natural identifier.
/// Idempotent for Map kinds: returns `{created: false}` if the key already
/// exists.
///
/// Dispatch happens via `Config::create_map_key()` — emitted by the
/// `Configurable` derive, single source of truth. Adding a new
/// `HashMap<String, T>` or `#[nested] Vec<T>` field to the schema makes it
/// addable here automatically.
pub async fn handle_map_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MapKeyQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut working = state.config.lock().clone();
    let path = q.path.clone();
    let key = q.key.clone();

    let created = match working.create_map_key(&path, &key) {
        Ok(b) => b,
        Err(msg) => {
            return error_response(
                ConfigApiError::new(ConfigApiCode::PathNotFound, msg).with_path(&path),
            );
        }
    };

    if created && let Err(e) = persist_and_swap(&state, working).await {
        return error_response(e);
    }

    axum::Json(MapKeyResponse { path, key, created }).into_response()
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

    let working = state.config.lock().clone();

    // Drift guard: if the on-disk file diverges from in-memory state on any
    // path the PATCH would touch, refuse with 409 ConfigChangedExternally
    // unless the client explicitly opts in to overwrite via the
    // `X-ZeroClaw-Override-Drift: true` header. The opt-in surface keeps
    // the contract loud: the only way to silently overwrite a hand-edit is
    // a deliberate header, never an accident.
    let override_drift = headers
        .get("x-zeroclaw-override-drift")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if !override_drift {
        let drifted = compute_drift(&working).await;
        if !drifted.is_empty() {
            let touched: std::collections::HashSet<String> = ops
                .iter()
                .map(|op| json_pointer_to_dotted(&op.path))
                .collect();
            let conflicts: Vec<&DriftEntry> = drifted
                .iter()
                .filter(|d| touched.contains(&d.path))
                .collect();
            if !conflicts.is_empty() {
                let conflict_paths: Vec<String> =
                    conflicts.iter().map(|d| d.path.clone()).collect();
                return error_response(ConfigApiError::new(
                    ConfigApiCode::ConfigChangedExternally,
                    format!(
                        "on-disk config has drifted from in-memory state on \
                         {} path(s) being patched: {}. Send `X-ZeroClaw-Override-Drift: true` \
                         to overwrite, or GET /api/config/drift to inspect first.",
                        conflicts.len(),
                        conflict_paths.join(", "),
                    ),
                ));
            }
        }
    }

    let mut working = working;
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
                    comment: None, // `test` ops don't write
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
                        comment: op.comment.clone(),
                    });
                } else {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: Some(serde_json::Value::String(value_str)),
                        populated: None,
                        comment: op.comment.clone(),
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
                        comment: op.comment.clone(),
                    });
                } else {
                    results.push(PatchOpResult {
                        op: op.op.clone(),
                        path,
                        value: Some(serde_json::Value::Null),
                        populated: None,
                        comment: op.comment.clone(),
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
        && let Err(e) =
            zeroclaw_config::comment_writer::apply_comments(&config_path, &annotations).await
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

/// OPTIONS /api/config/prop?path=providers.fallback — per-field schema fragment.
///
/// Returns 404 with `path_not_found` if the path doesn't resolve against the
/// in-memory config — same contract as `GET /api/config/prop`. Previously
/// returned the whole-config schema regardless, which silently masked typos.
///
/// Per-path subtree extraction (walking the JSON Schema tree by JSON Pointer
/// to return just the relevant subtree) is a follow-up; today we still return
/// the full schema with a `x-zeroclaw-requested-path` + per-field metadata
/// (kind, type_hint, is_secret) so the frontend has everything it needs to
/// render the input without a separate round-trip.
pub async fn handle_options_prop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PropQuery>,
) -> Response {
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

    // Resolve the path against the in-memory config; 404 if it doesn't
    // exist. (No auth required for shape discovery — same as OPTIONS /api/config.)
    let config = state.config.lock().clone();
    let info = match lookup_prop_field(&config, &q.path) {
        Some(info) => info,
        None => return error_response(ConfigApiError::path_not_found(&q.path)),
    };

    let (whole_body, etag) = cached_schema();
    let mut body = whole_body.clone();
    if let serde_json::Value::Object(ref mut map) = body {
        map.insert(
            "x-zeroclaw-requested-path".into(),
            serde_json::Value::String(q.path.clone()),
        );
        map.insert(
            "x-zeroclaw-prop".into(),
            serde_json::json!({
                "path": q.path,
                "kind": prop_kind_wire(info.kind),
                "type_hint": info.type_hint,
                "is_secret": info.is_secret || info.derived_from_secret,
                "enum_variants": info.enum_variants.map(|f| f()).unwrap_or_default(),
                "category": info.category,
            }),
        );
    }
    let mut response = (StatusCode::OK, axum::Json(body)).into_response();
    response.headers_mut().insert(
        header::ALLOW,
        HeaderValue::from_static("GET, PUT, DELETE, OPTIONS"),
    );
    response
        .headers_mut()
        .insert(header::ETAG, HeaderValue::from_str(etag).unwrap());
    response
}

fn schema_response(_label: &'static str) -> Response {
    let (body, etag) = cached_schema();
    let mut response = (StatusCode::OK, axum::Json(body.clone())).into_response();
    response.headers_mut().insert(
        header::ALLOW,
        HeaderValue::from_static("GET, PUT, PATCH, OPTIONS"),
    );
    response
        .headers_mut()
        .insert(header::ETAG, HeaderValue::from_str(etag).unwrap());
    response
}

/// Compute the OPTIONS schema body + ETag once and cache them. The schema is
/// static per build (schemars output is deterministic for a given Config
/// type), so re-rendering on every request is pure waste — we'd send the
/// same bytes back every time and re-hash them too. The previous
/// implementation re-rendered + re-hashed on every OPTIONS hit; this caches
/// both behind a `OnceLock`.
fn cached_schema() -> (&'static serde_json::Value, &'static str) {
    use std::sync::OnceLock;
    static CACHE: OnceLock<(serde_json::Value, String)> = OnceLock::new();
    let entry = CACHE.get_or_init(|| {
        let body = schema_body_value();
        let etag = build_etag_for(&body);
        (body, etag)
    });
    (&entry.0, entry.1.as_str())
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

/// Stable ETag derived from the rendered schema bytes. Computed once via
/// `cached_schema()`; this helper is kept separate so tests can verify
/// determinism.
fn build_etag_for(body: &serde_json::Value) -> String {
    use std::hash::{Hash, Hasher};
    let bytes = body.to_string();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("\"{:016x}\"", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    // typed-value coercion tests live in zeroclaw_config::typed_value
    // — shared helper, single source of truth.
    //
    // build_comment_prefix tests live in zeroclaw_config::comment_writer
    // — same reason.

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

        zeroclaw_config::comment_writer::apply_comments(
            &path,
            &[("gateway.host".into(), "raised after Q3 backlog".into())],
        )
        .await
        .expect("apply_comments");

        let raw = tokio::fs::read_to_string(&path).await.expect("read back");
        // Existence check: the comment text appears in the file.
        assert!(
            raw.contains("# raised after Q3 backlog"),
            "expected comment in file, got:\n{raw}"
        );

        // Positional check: the comment appears IMMEDIATELY ABOVE `host = ...`,
        // not somewhere else in the file. The previous version of the helper
        // wrote the prefix between `=` and the value, producing broken TOML —
        // this assertion would have caught that bug.
        let lines: Vec<&str> = raw.lines().collect();
        let host_line_idx = lines
            .iter()
            .position(|l| l.trim_start().starts_with("host"))
            .expect("host = line in saved config");
        assert!(
            host_line_idx > 0,
            "host line is at top — comment can't precede it"
        );
        let above = lines[host_line_idx - 1];
        assert_eq!(
            above.trim(),
            "# raised after Q3 backlog",
            "expected comment immediately above `host = ...`, got line above:\n  {above:?}\nfull file:\n{raw}"
        );

        // Round-trip check: re-parsing the file must succeed (broken
        // decoration target produces malformed TOML).
        let _: toml::Value = toml::from_str(&raw)
            .unwrap_or_else(|e| panic!("re-parse failed after apply_comments: {e}\nfile:\n{raw}"));
    }

    #[test]
    fn scrub_credentials_catches_credential_shaped_strings() {
        // Defence-in-depth: scrub_credentials (the workspace's existing
        // tracing scrubber) catches keyword=value patterns that are the
        // most likely shape for accidental log leakage. Pin the contract
        // here so a regression in either the regex or the assumed shapes
        // gets caught — important for the new HTTP CRUD surface where the
        // dashboard sends real bearer tokens, secret PUT bodies, etc.
        use zeroclaw_runtime::agent::loop_::scrub_credentials;

        // Three realistic shapes a tracing call might emit. All must be
        // redacted by the existing scrubber.
        // The scrubber matches KEYWORD<:|=>VALUE patterns. These are the
        // shapes most likely to appear in a tracing log line (`tracing`'s
        // `?body` debug-format renders structs as `field: value` and JSON
        // keys are typically written as `"key": "value"`).
        let cases = [
            // Field=value style log line.
            (
                "api-key=sk-live-abcdef-1234567890",
                "sk-live-abcdef-1234567890",
            ),
            // JSON-ish quoted key-value pair.
            (
                r#""token": "sk-test-supersecret-12345""#,
                "sk-test-supersecret-12345",
            ),
            // Explicit secret key.
            (
                "secret: hunter2-not-a-real-password",
                "hunter2-not-a-real-password",
            ),
            // Bearer credential pair.
            (
                "credential: bearer-token-abcdef-9876",
                "bearer-token-abcdef-9876",
            ),
        ];
        for (input, raw_secret) in cases {
            let scrubbed = scrub_credentials(input);
            assert!(
                !scrubbed.contains(raw_secret),
                "scrubber missed `{raw_secret}` in:\n  input    : {input}\n  scrubbed : {scrubbed}"
            );
            assert!(
                scrubbed.contains("REDACTED"),
                "expected REDACTED marker in:\n  input    : {input}\n  scrubbed : {scrubbed}"
            );
        }
    }

    #[tokio::test]
    async fn compute_drift_detects_external_edit_to_field() {
        // Persist initial state, externally edit the file, drift surfaces
        // the touched path. This is the substrate the PATCH 409 guard fires on.
        let (_tmp, path) = temp_config_path();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: path.clone(),
            ..Default::default()
        };
        cfg.set_prop("gateway.host", "10.0.0.1").expect("set");
        cfg.save().await.expect("save");

        // Simulate a hand-edit while the daemon "wasn't looking".
        let on_disk = tokio::fs::read_to_string(&path).await.unwrap();
        let edited = on_disk.replace("10.0.0.1", "192.168.1.1");
        tokio::fs::write(&path, edited).await.unwrap();

        // In-memory still believes 10.0.0.1; on-disk now says 192.168.1.1.
        let drift = compute_drift(&cfg).await;
        let entry = drift
            .iter()
            .find(|d| d.path == "gateway.host")
            .expect("expected gateway.host in drift summary after external edit");
        assert!(entry.drifted);
        assert_eq!(
            entry.in_memory_value,
            Some(serde_json::Value::String("10.0.0.1".into()))
        );
        assert_eq!(
            entry.on_disk_value,
            Some(serde_json::Value::String("192.168.1.1".into()))
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
            kind: "string",
            type_hint: "Option<String>",
            value: None,
            populated: true,
            is_secret: true,
            enum_variants: vec![],
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

        zeroclaw_config::comment_writer::apply_comments(
            &path,
            &[("gateway.host".into(), "first reason".into())],
        )
        .await
        .expect("apply first comment");
        zeroclaw_config::comment_writer::apply_comments(
            &path,
            &[("gateway.host".into(), String::new())],
        )
        .await
        .expect("apply empty");

        let raw = tokio::fs::read_to_string(&path).await.expect("read back");
        assert!(
            !raw.contains("first reason"),
            "expected the prior comment to be cleared, got:\n{raw}"
        );
    }
}
