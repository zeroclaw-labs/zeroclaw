//! Per-property CRUD endpoints for `/api/config/*`.

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};
use zeroclaw_config::field_visibility;
use zeroclaw_config::sections::section_for_path;
use zeroclaw_config::traits::MaskSecrets;

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

/// One JSON Patch operation. Supports `add`, `remove`, `replace`, `test`, and
/// ZeroClaw's `comment` extension. Every operation requires `path`; `add`,
/// `replace`, and `test` require `value`, while `comment` requires `comment`.
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
    /// Non-fatal validation warnings against the saved configuration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<zeroclaw_config::validation_warnings::ValidationWarning>,
}

/// GET /api/config — compatibility whole-config read for older bundled
/// dashboard pages. New clients should prefer the per-property API, but
/// returning a masked snapshot here avoids a hard 405 when an older page is
/// served by a newer gateway.
pub async fn handle_config_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut cfg = state.config.read().clone();
    cfg.mask_secrets();
    Json(cfg).into_response()
}

fn parse_patch_ops(value: serde_json::Value) -> Result<Vec<PatchOp>, ConfigApiError> {
    let ops = value.as_array().ok_or_else(|| {
        ConfigApiError::new(
            ConfigApiCode::ValueTypeMismatch,
            "JSON Patch body must be a JSON array of operations",
        )
    })?;

    let mut parsed = Vec::with_capacity(ops.len());
    for (idx, op) in ops.iter().enumerate() {
        let object = op.as_object().ok_or_else(|| {
            ConfigApiError::new(
                ConfigApiCode::ValueTypeMismatch,
                format!("JSON Patch op[{idx}] must be an object"),
            )
            .with_op_index(idx)
        })?;
        let op_name = object.get("op").and_then(|v| v.as_str()).ok_or_else(|| {
            ConfigApiError::new(
                ConfigApiCode::ValueTypeMismatch,
                format!("JSON Patch op[{idx}] requires string `op` field"),
            )
            .with_op_index(idx)
        })?;
        let path = object.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ConfigApiError::new(
                ConfigApiCode::ValueTypeMismatch,
                format!("JSON Patch op[{idx}] requires string `path` field"),
            )
            .with_op_index(idx)
        })?;
        let comment = match object.get("comment") {
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| {
                        ConfigApiError::new(
                            ConfigApiCode::ValueTypeMismatch,
                            format!("JSON Patch op[{idx}] `comment` field must be a string"),
                        )
                        .with_path(json_pointer_to_dotted(path))
                        .with_op_index(idx)
                    })?
                    .to_string(),
            ),
            None => None,
        };

        parsed.push(PatchOp {
            op: op_name.to_string(),
            path: path.to_string(),
            value: object.get("value").cloned(),
            comment,
        });
    }

    Ok(parsed)
}

/// Response for a non-secret GET / PUT / DELETE.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PropResponse {
    pub path: String,
    pub value: serde_json::Value,
    /// Non-fatal validation warnings against the current config state.
    /// On GET this surfaces warnings present in the loaded config; on PUT
    /// this surfaces warnings against the post-save state. Empty when
    /// nothing is flagged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<zeroclaw_config::validation_warnings::ValidationWarning>,
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

/// Single entry in the list response for configuration properties.
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
    /// Whether this field was populated by a `ZEROCLAW_*` env-var override
    /// at load time. The dashboard renders the 💉 badge and a persistent
    /// warning *"Edits here won't take effect — overridden by ZEROCLAW_..."*
    /// when this is `true`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_env_overridden: bool,
    /// Variants for `enum`-kind fields — non-empty means the frontend should
    /// render a `<select>` with these options. Empty for non-enum fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_variants: Vec<String>,
    /// Onboard section name derived from the path's first segment via
    /// `Section::from_path`. `None` for paths that aren't part of any wizard
    /// section. The dashboard groups list entries by this for per-section
    /// rendering — same source the CLI wizard uses, no schema attribute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<&'static str>,
    /// Tab grouping label from the field's `#[tab(...)]` annotation
    /// (`ConfigTab::label`). Absent for `ConfigTab::None`. Surfaces group
    /// list entries into a tab bar by this; the agent edit form depends on
    /// it to split General / Providers / Channels / etc.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub tab: &'static str,
    /// Surface hint from `#[multiline]`: render a multi-line text area
    /// (e.g. a PEM key body) instead of a single-line input.
    #[serde(default, skip_serializing_if = "is_false")]
    pub multiline: bool,
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
        PropKind::AliasRef => "alias-ref",
        PropKind::StringArray => "string-array",
        PropKind::ObjectArray => "object-array",
        PropKind::Object => "object",
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

/// One drift entry surfaced when in-memory Config diverges from the on-disk file.
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
        .or_else(|| {
            zeroclaw_config::schema::Config::prop_is_secret(path).then(|| {
                zeroclaw_config::traits::PropFieldInfo {
                    name: path.to_string(),
                    category: "Secrets",
                    display_value: zeroclaw_config::traits::UNSET_DISPLAY.to_string(),
                    type_hint: "String",
                    kind: zeroclaw_config::traits::PropKind::String,
                    is_secret: true,
                    enum_variants: None,
                    description: "",
                    derived_from_secret: false,
                    credential_class: Some(
                        zeroclaw_config::traits::CredentialSurfaceClass::EncryptedSecret,
                    ),
                    tab: zeroclaw_config::traits::ConfigTab::None,
                    alias_source: None,
                    multiline: false,
                }
            })
        })
}

fn scoped_validate(
    working: &zeroclaw_config::schema::Config,
) -> Result<Vec<zeroclaw_config::validation_warnings::ValidationWarning>, ConfigApiError> {
    if let Err(e) = working.validate() {
        let api_err = ConfigApiError::from_validation(e);
        let err_path = api_err.path.as_deref().unwrap_or("");
        let touches_dirty = !err_path.is_empty()
            && working.dirty_paths.iter().any(|d| {
                err_path == d.as_str()
                    || err_path.starts_with(&format!("{d}."))
                    || d.starts_with(&format!("{err_path}."))
            });
        if touches_dirty || err_path.is_empty() {
            return Err(api_err);
        }
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"path": err_path})),
            &format!(
                "validate() failed on a path outside this PATCH's dirty set; saving anyway and \
             surfacing as a warning: {}",
                api_err.message
            )
        );
        return Ok(vec![
            zeroclaw_config::validation_warnings::ValidationWarning::new(
                "pre_existing_validation_error",
                api_err.message,
                err_path.to_string(),
            ),
        ]);
    }
    Ok(Vec::new())
}

async fn persist_and_swap(
    state: &AppState,
    mut new_config: zeroclaw_config::schema::Config,
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

    if let Err(e) = new_config.save_dirty().await {
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

    *state.config.write() = new_config;
    state
        .pending_reload
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// `POST /api/channels/bind` request body. The GUI/HTTP equivalent of
/// `zeroclaw channel bind-<type> <identity> --alias <alias>`: authorize an
/// operator-named identity on one channel alias without the in-chat
/// `/bind <code>` round trip.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ChannelBindBody {
    pub channel_type: String,
    pub alias: String,
    pub identity: String,
}

/// POST /api/channels/bind — add an inbound identity to a pairing channel's
/// allowlist. Shares the exact bind core the CLI uses
/// (`bind_channel_identity_into`), writes ONLY to
/// `peer_groups.<type>_<alias>.external_peers`, and is gated by the same
/// bearer auth as every other config write. Because the gateway and the
/// running channels share one `Arc<RwLock<Config>>`, the swap makes the new
/// peer live immediately — no daemon restart, and no `/bind` message.
pub async fn handle_api_channel_bind(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChannelBindBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let channel_type = body.channel_type.trim();
    let alias = body.alias.trim();

    // Closed-set gate: only telegram/wechat/line have an operator-bind surface.
    if zeroclaw_channels::orchestrator::channel_identity_normalizer(channel_type).is_none() {
        return error_response(ConfigApiError::new(
            ConfigApiCode::ValidationFailed,
            format!(
                "channel type `{channel_type}` does not support identity binding \
                 (supported: telegram, wechat, line)"
            ),
        ));
    }

    let mut working = state.config.read().clone();

    // Reject a phantom alias loudly (404) rather than minting a peer group the
    // runtime never reads.
    if !zeroclaw_channels::orchestrator::channel_alias_configured(&working, channel_type, alias) {
        return error_response(ConfigApiError::new(
            ConfigApiCode::PathNotFound,
            format!("channel `{channel_type}.{alias}` is not configured"),
        ));
    }

    let newly = match zeroclaw_channels::orchestrator::bind_channel_identity_into(
        &mut working,
        channel_type,
        alias,
        &body.identity,
    ) {
        Ok(added) => added,
        Err(e) => {
            return error_response(ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                e.to_string(),
            ));
        }
    };

    let group = format!("{channel_type}_{alias}");
    let channel = format!("{channel_type}.{alias}");

    if !newly {
        return Json(serde_json::json!({
            "saved": false,
            "already_bound": true,
            "group": group,
            "channel": channel,
        }))
        .into_response();
    }

    // Persist with a full `save` (the same path the CLI bind uses), NOT the
    // incremental `save_dirty` behind `persist_and_swap`: a direct peer-group
    // mutation isn't dirty-tracked, so `save_dirty` would never write it to
    // disk (the bind would vanish on restart), and `save` also correctly
    // materializes a brand-new peer-group table. Then swap the shared
    // in-memory config so the running channel authorizes the peer live.
    if let Err(e) = working.save().await {
        return error_response(ConfigApiError::new(
            ConfigApiCode::ReloadFailed,
            format!("save failed: {e}"),
        ));
    }
    *state.config.write() = working;
    state
        .pending_reload
        .store(true, std::sync::atomic::Ordering::Relaxed);

    Json(serde_json::json!({
        "saved": true,
        "already_bound": false,
        "group": group,
        "channel": channel,
    }))
    .into_response()
}

/// Fields the gateway owns end-to-end (mints, rotates, persists itself).
/// They're skipped by [`compute_drift`] so the dashboard doesn't surface a
/// banner the operator can't act on. Add new entries here when a similar
/// gateway-managed field lands (e.g. webhook secret rotation).
fn is_gateway_managed_field(name: &str) -> bool {
    // Match the prop-field name actually emitted by the `Configurable` derive,
    // which preserves the Rust field's snake_case (`paired_tokens`), not kebab.
    matches!(name, "gateway.paired_tokens")
}

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
    let mut all_names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    all_names.extend(in_memory_props.keys().map(String::as_str));
    all_names.extend(on_disk_props.keys().map(String::as_str));
    for name in all_names {
        if is_gateway_managed_field(name) {
            continue;
        }
        // Env overrides (`ZEROCLAW_<path>`) apply in memory but never persist to
        // disk, so a disk comparison always reports drift the operator can't fix.
        if in_memory.prop_is_env_overridden(name) {
            continue;
        }
        let mem = in_memory_props.get(name);
        let disk = on_disk_props.get(name);
        let mem_display = mem
            .map(|p| p.display_value.as_str())
            .unwrap_or(zeroclaw_config::traits::UNSET_DISPLAY);
        let disk_display = disk
            .map(|p| p.display_value.as_str())
            .unwrap_or(zeroclaw_config::traits::UNSET_DISPLAY);
        if mem_display == disk_display {
            continue;
        }
        let is_sensitive = mem
            .or(disk)
            .map(|p| p.is_secret || p.derived_from_secret)
            .unwrap_or(false);
        if is_sensitive {
            use sha2::{Digest, Sha256};
            let mem_hash = Sha256::digest(mem_display.as_bytes());
            let disk_hash = Sha256::digest(disk_display.as_bytes());
            if mem_hash == disk_hash {
                continue;
            }
            drift.push(DriftEntry {
                path: name.to_string(),
                secret: true,
                drifted: true,
                in_memory_value: None,
                on_disk_value: None,
            });
        } else {
            drift.push(DriftEntry {
                path: name.to_string(),
                secret: false,
                drifted: true,
                in_memory_value: Some(serde_json::Value::String(mem_display.to_string())),
                on_disk_value: Some(serde_json::Value::String(disk_display.to_string())),
            });
        }
    }

    // Stable order so callers can diff snapshots.
    drift.sort_by(|a, b| a.path.cmp(&b.path));
    drift
}

// ── Handlers ────────────────────────────────────────────────────────

pub async fn handle_prop_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PropQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    let info = match lookup_prop_field(&config, &q.path) {
        Some(info) => info,
        None => return error_response(ConfigApiError::path_not_found(&q.path)),
    };

    if info.is_secret || info.derived_from_secret {
        let populated = info.display_value != zeroclaw_config::traits::UNSET_DISPLAY;
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
            let warnings = config.collect_warnings();
            axum::Json(PropResponse {
                path: q.path,
                value: serde_json::Value::String(value_str),
                warnings,
            })
            .into_response()
        }
        Err(e) => error_response(map_prop_error(e, &q.path)),
    }
}

pub async fn handle_prop_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<PropPutBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut new_config = state.config.read().clone();
    if new_config.ensure_map_key_for_path(&body.path) {
        // Refused to vivify the reserved `default` agent: surface the same
        // reserved error the explicit create surfaces do, not a generic 404.
        return error_response(
            ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                "alias `default` is reserved and cannot be created",
            )
            .with_path(&body.path),
        );
    }
    let info = match lookup_prop_field(&new_config, &body.path) {
        Some(info) => info,
        None => return error_response(ConfigApiError::path_not_found(&body.path)),
    };

    let value_str = match json_to_setprop_string(&body.value, Some(info.kind)) {
        Ok(s) => s,
        Err(e) => return error_response(e.with_path(&body.path)),
    };

    // Reject the masked sentinel for secrets — surfaces occasionally
    // echo the masked display value back when no real edit happened.
    // Letting that through would overwrite the live secret with the
    // literal masked string.
    let is_sensitive = info.is_secret || info.derived_from_secret;
    if is_sensitive
        && (value_str == zeroclaw_config::traits::MASKED_SECRET
            || value_str == "****"
            || value_str.is_empty())
    {
        return error_response(
            ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!(
                    "Refusing to overwrite secret `{}` with a masked or empty value",
                    body.path
                ),
            )
            .with_path(&body.path),
        );
    }

    if let Err(e) = new_config.set_prop_persistent(&body.path, &value_str) {
        return error_response(map_prop_error(e, &body.path));
    }

    let scoped_validation_warnings = match scoped_validate(&new_config) {
        Ok(ws) => ws,
        Err(err) => return error_response(err),
    };

    let config_path = new_config.config_path.clone();
    let mut warnings = new_config.collect_warnings();
    warnings.extend(scoped_validation_warnings);
    if let Err(e) = persist_and_swap(&state, new_config).await {
        return error_response(e);
    }
    if let Some(comment) = body.comment.as_ref() {
        let annotations = [(body.path.clone(), comment.clone())];
        if let Err(e) =
            zeroclaw_config::comment_writer::apply_comments(&config_path, &annotations).await
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "failed to apply PUT comment to config.toml"
            );
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
            warnings,
        })
        .into_response()
    }
}

pub async fn handle_prop_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<PropQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut new_config = state.config.read().clone();
    let info = match lookup_prop_field(&new_config, &q.path) {
        Some(info) => info,
        None => return error_response(ConfigApiError::path_not_found(&q.path)),
    };

    if let Err(e) = new_config.set_prop_persistent(&q.path, "") {
        return error_response(map_prop_error(e, &q.path));
    }

    let scoped_validation_warnings = match scoped_validate(&new_config) {
        Ok(ws) => ws,
        Err(err) => return error_response(err),
    };

    let mut warnings = new_config.collect_warnings();
    warnings.extend(scoped_validation_warnings);
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
            warnings,
        })
        .into_response()
    }
}

pub async fn handle_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.read().clone();
    let prefix = q.prefix.as_deref();

    // Drop fields that don't apply to the current shape of the config —
    // azure_* on a non-azure model_provider, qdrant.* when memory.backend is
    // sqlite, etc. Keeps the form scoped to relevant inputs only.
    let excluded = field_visibility::excluded_paths(&config, prefix.unwrap_or(""));

    let entries: Vec<ListEntry> = config
        .prop_fields()
        .into_iter()
        .filter(|info| match prefix {
            Some(p) => field_visibility::path_matches_prefix(&info.name, p),
            None => true,
        })
        .filter(|info| !field_visibility::is_excluded(&info.name, &excluded))
        .map(|info| {
            let populated = info.display_value != zeroclaw_config::traits::UNSET_DISPLAY;
            let is_sensitive = info.is_secret || info.derived_from_secret;
            let value = if is_sensitive {
                None
            } else {
                Some(serde_json::Value::String(info.display_value.clone()))
            };
            let section = section_for_path(&info.name).map(|s| s.as_str());
            let enum_variants = info.enum_variants.map(|f| f()).unwrap_or_default();
            let is_env_overridden = config.prop_is_env_overridden(&info.name);
            ListEntry {
                path: info.name,
                category: info.category.to_string(),
                kind: prop_kind_wire(info.kind),
                type_hint: info.type_hint,
                value,
                populated,
                is_secret: is_sensitive,
                is_env_overridden,
                enum_variants,
                section,
                tab: info.tab.label(),
                multiline: info.multiline,
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
    let config = state.config.read().clone();
    let drifted = compute_drift(&config).await;
    axum::Json(DriftResponse { drifted }).into_response()
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ReloadStatusResponse {
    /// Whether any config write has landed since the last admin reload and may
    /// still require subsystem re-instantiation to take effect.
    pub pending_reload: bool,
}

/// `GET /api/config/reload-status` — pending-reload flag for the dashboard's
/// reload banner. Goes true on any config write, false on `/admin/reload`.
pub async fn handle_reload_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let pending_reload = state
        .pending_reload
        .load(std::sync::atomic::Ordering::Relaxed);
    axum::Json(ReloadStatusResponse { pending_reload }).into_response()
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MapKeyQuery {
    /// Map-keyed section path, e.g. `providers.models`, `agents`, `risk_profiles`.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,
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

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct MapPathQuery {
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AliasSourceQuery {
    pub source: zeroclaw_config::traits::AliasSource,
}

/// `GET /api/config/resolve-alias-source?source=<source>` — list the configured
/// alias values valid for an alias-reference field, resolved from the live
/// config via the shared `Config::resolve_alias_source`.
pub async fn handle_resolve_alias_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AliasSourceQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.read().clone();
    let values = cfg.resolve_alias_source(q.source);
    axum::Json(serde_json::json!({ "source": q.source, "values": values })).into_response()
}

/// `GET /api/config/map-keys?path=<section>` — list the current alias keys at
/// a map-keyed section path, e.g. `channels.discord` → `["default","work"]`.
pub async fn handle_get_map_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MapPathQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.read().clone();
    match cfg.get_map_keys(&q.path) {
        Some(keys) => {
            axum::Json(serde_json::json!({ "path": q.path, "keys": keys })).into_response()
        }
        None => error_response(
            ConfigApiError::new(
                ConfigApiCode::PathNotFound,
                format!("no map-keyed section at `{}`", q.path),
            )
            .with_path(&q.path),
        ),
    }
}

/// `DELETE /api/config/map-key?path=<section>&key=<alias>` — remove an alias
/// from a map-keyed section. Aliased config sections with executable delete
/// support route through the same cascade engine as the delete preview;
/// non-aliased sections keep the generic raw key removal. Persists on success.
pub async fn handle_delete_map_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MapKeyQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let working = state.config.read().clone();
    match zeroclaw_config::alias_refs::alias_kind_for_map_path(&q.path) {
        Some(zeroclaw_config::alias_refs::AliasKind::Agent) => {
            // Agent deletion is special: it must scrub config references
            // (heartbeat, peer-groups, delegates, workspace.access, …) via
            // `delete_with_cascade` and cascade owned non-config state (memory /
            // cron / acp / session).
            return delete_agent_cascade(&state, working, &q.key).await;
        }
        Some(kind) => {
            return delete_config_cascade(&state, working, &kind, &q.path, &q.key).await;
        }
        None => {}
    }
    let mut working = working;
    let removed = match working.delete_map_key(&q.path, &q.key) {
        Ok(b) => b,
        Err(msg) => {
            return error_response(
                ConfigApiError::new(ConfigApiCode::PathNotFound, msg).with_path(&q.path),
            );
        }
    };
    if removed {
        working.mark_dirty(&format!("{}.{}", q.path, q.key));
        if let Err(e) = persist_and_swap(&state, working).await {
            return error_response(e);
        }
    }
    axum::Json(MapKeyResponse {
        path: q.path,
        key: q.key,
        created: false,
        warnings: None,
    })
    .into_response()
}

/// Agent-deletion cascade: refuse on HARD references (enabled `heartbeat.agent`
/// or live ACP sessions), else scrub config refs + remove the entry via
/// `delete_with_cascade`, archive the workspace, run the owned-state cascade
/// (export-then-delete memory/cron/acp + clear session attribution), and persist.
async fn delete_agent_cascade(
    state: &AppState,
    mut working: zeroclaw_config::schema::Config,
    alias: &str,
) -> Response {
    use zeroclaw_config::alias_refs::{self, AliasKind, CascadePolicy};

    if !working.agents.contains_key(alias) {
        return error_response(
            ConfigApiError::new(
                ConfigApiCode::PathNotFound,
                format!("agents.{alias} is not configured"),
            )
            .with_path("agents"),
        );
    }

    // Refuse on HARD: config blockers (e.g. enabled heartbeat.agent) OR live ACP
    // sessions (the operator must end those first). The ACP gate FAILS CLOSED:
    // if the session store can't be read we refuse rather than risk orphaning
    // live sessions.
    let plan = alias_refs::plan_delete(&working, &AliasKind::Agent, alias);
    let live_acp = match crate::agent_owned_state::live_acp_session_count(&working, alias) {
        Ok(n) => n,
        Err(e) => {
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::ValidationFailed,
                    format!(
                        "cannot delete agent `{alias}`: could not verify live ACP sessions ({e}); refusing to avoid orphaning active sessions"
                    ),
                )
                .with_path(format!("agents.{alias}")),
            );
        }
    };
    if !plan.allowed || live_acp > 0 {
        let mut reasons: Vec<String> = plan
            .blockers
            .iter()
            .map(|b| format!("{} (hard config reference)", b.path))
            .collect();
        if live_acp > 0 {
            reasons.push(format!("{live_acp} live ACP session(s) — end them first"));
        }
        return error_response(
            ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!("cannot delete agent `{alias}`: {}", reasons.join("; ")),
            )
            .with_path(format!("agents.{alias}")),
        );
    }

    let workspace = working.agent_workspace_dir(alias);

    // Config cascade: scrub soft refs + remove the agents entry.
    let cascade = match alias_refs::delete_with_cascade(
        &mut working,
        &AliasKind::Agent,
        alias,
        CascadePolicy::RefuseOnHard,
    ) {
        Ok(report) => report,
        Err(e) => {
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::ValidationFailed,
                    format!("agent config cascade failed: {e}"),
                )
                .with_path(format!("agents.{alias}")),
            );
        }
    };

    for path in cascade.dirty_paths() {
        working.mark_dirty(&path);
    }
    if let Err(e) = persist_and_swap(state, working).await {
        return error_response(e);
    }
    // Config is durably committed: the agent is GONE from the persisted config.
    // Read it back from the (now-swapped) AppState for the side-effects below.
    let committed = state.config.read().clone();

    let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let archive_dir = committed
        .data_dir
        .join("agents")
        .join("_deleted")
        .join(format!("{alias}-{ts}"));
    let mut warnings: Vec<String> = Vec::new();
    if let Err(err) = tokio::fs::create_dir_all(&archive_dir).await {
        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": alias, "archive": archive_dir.display().to_string(), "err": err.to_string()})), "agent delete: archive dir creation failed");
        warnings.push(format!(
            "archive dir creation failed ({}): {err}",
            archive_dir.display()
        ));
    }
    if workspace.exists() {
        let dest = archive_dir.join("workspace");
        if let Err(err) = tokio::fs::rename(&workspace, &dest).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"agent": alias, "from": workspace.display().to_string(), "to": dest.display().to_string(), "err": err.to_string()})),
                "agent delete: workspace archive failed"
            );
            warnings.push(format!(
                "workspace archive failed ({} -> {}): {err}",
                workspace.display(),
                dest.display()
            ));
        }
    }

    // Owned-state cascade (export-then-delete memory/cron/acp + clear sessions).
    let owned = crate::agent_owned_state::cascade_owned_state(
        &committed,
        &state.mem,
        state.session_backend.as_ref(),
        alias,
        &archive_dir,
    )
    .await;
    // Combine per-side-effect failures (archive dir / workspace rename) with
    // the per-store failures surfaced by `cascade_owned_state`, so the operator
    // sees the FULL partial-failure picture in the response, not just the
    // server log.
    warnings.extend(owned.warnings.iter().cloned());
    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"agent": alias, "memory": owned.memory_purged, "cron": owned.cron_removed, "acp": owned.acp_removed, "sessions_cleared": owned.sessions_cleared, "archive": archive_dir.display().to_string(), "warnings": warnings.len()})), "agent deleted with owned-state cascade");

    axum::Json(MapKeyResponse {
        path: "agents".to_string(),
        key: alias.to_string(),
        created: false,
        warnings: if warnings.is_empty() {
            None
        } else {
            Some(warnings)
        },
    })
    .into_response()
}

/// Config-only delete cascade for providers/channels (no owned state): refuse
/// on hard refs, scrub soft refs, mark every touched path dirty, persist.
async fn delete_config_cascade(
    state: &AppState,
    mut working: zeroclaw_config::schema::Config,
    kind: &zeroclaw_config::alias_refs::AliasKind,
    path: &str,
    key: &str,
) -> Response {
    let report = match zeroclaw_config::alias_refs::delete_with_cascade(
        &mut working,
        kind,
        key,
        zeroclaw_config::alias_refs::CascadePolicy::RefuseOnHard,
    ) {
        Ok(r) => r,
        Err(e) => return delete_error_response(path, key, e),
    };
    let dirty_paths = report.dirty_paths();
    for dirty_path in &dirty_paths {
        working.mark_dirty(dirty_path);
    }
    if let Err(e) = persist_and_swap(state, working).await {
        return error_response(e);
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({"path": path, "key": key, "dirty_paths": dirty_paths.len()})
        ),
        "alias deleted with config-ref cascade"
    );
    axum::Json(MapKeyResponse {
        path: path.to_string(),
        key: key.to_string(),
        created: false,
        warnings: None,
    })
    .into_response()
}

pub async fn handle_map_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MapKeyQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut working = state.config.read().clone();
    let path = q.path.clone();
    let key = q.key.clone();

    // Create through the shared guarded boundary so the reserved-agent rule (the
    // `default` runtime fallback) is enforced once for every surface. Reserved ->
    // 400 (validation_failed), symmetric with the rename guard; an unknown
    // section or invalid key stays 404 (path_not_found) as before.
    let created =
        match zeroclaw_config::alias_refs::create_map_key_checked(&mut working, &path, &key) {
            Ok(b) => b,
            Err(zeroclaw_config::alias_refs::CreateError::Reserved(a)) => {
                return error_response(
                    ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!("alias `{a}` is reserved and cannot be created"),
                    )
                    .with_path(format!("{path}.{key}")),
                );
            }
            Err(zeroclaw_config::alias_refs::CreateError::Invalid(msg)) => {
                return error_response(
                    ConfigApiError::new(ConfigApiCode::PathNotFound, msg).with_path(&path),
                );
            }
        };

    if created {
        // skill-bundles: materialize the bundle's resolved directory so
        // skills have a home immediately. Run before persist so a failed
        // mkdir surfaces in logs alongside the config write.
        if path == "skill_bundles" {
            let install_root = working.install_root_dir();
            if let Ok(dir) =
                zeroclaw_config::skill_bundles::resolve_directory(&working, &install_root, &key)
                && let Err(e) = tokio::fs::create_dir_all(&dir).await
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "skill-bundle '{key}' directory creation failed at {}: {e}",
                        dir.display().to_string()
                    )
                );
            }
        }

        working.mark_dirty(&format!("{path}.{key}"));
        if let Err(e) = persist_and_swap(&state, working).await {
            return error_response(e);
        }
    }

    axum::Json(MapKeyResponse {
        path,
        key,
        created,
        warnings: None,
    })
    .into_response()
}

/// A single config reference site to an aliased entry, for the delete preview.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RefSiteDto {
    /// Dotted config path that references the alias, e.g.
    /// `agents.forge.model_provider` or `heartbeat.agent`.
    pub path: String,
    /// The stored reference text, e.g. `anthropic.default`.
    pub raw_value: String,
}

/// Dry-run impact of deleting an aliased entry — the cascade preview a surface
/// renders before confirming. Pure/read-only: computed from `plan_delete` (the
/// same reference walk the real delete uses) plus the live-ACP gate for agents.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DeletePlanResponse {
    pub path: String,
    pub key: String,
    /// True iff nothing HARD blocks the delete (no hard config reference and,
    /// for agents, no live ACP session). Mirrors the real delete's refusal gate.
    pub allowed: bool,
    /// HARD references that block the delete — the operator must change these
    /// first (e.g. an enabled `heartbeat.agent`).
    pub blockers: Vec<RefSiteDto>,
    /// SOFT references the delete would scrub automatically.
    pub scrubs: Vec<RefSiteDto>,
    /// Agent delete only: number of live ACP sessions (a non-zero count blocks
    /// the delete; `null` for non-agent sections or if the count couldn't be
    /// read — in which case the delete fails closed too).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_acp_sessions: Option<usize>,
    /// Agent delete only: the agent's owned non-config state (memory / cron /
    /// session history) is exported and removed on delete. Counts are not
    /// enumerated in the preview.
    pub cascades_owned_state: bool,
}

/// `GET /api/config/delete-plan?path=<section>&key=<alias>` — dry-run the delete
/// cascade for an aliased entry. Read-only; never mutates.
pub async fn handle_delete_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MapKeyQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let to_dto = |s: &zeroclaw_config::alias_refs::RefSite| RefSiteDto {
        path: s.path.clone(),
        raw_value: s.raw_value.clone(),
    };
    let Some(kind) = zeroclaw_config::alias_refs::alias_kind_for_map_path(&q.path) else {
        // Non-aliased section (e.g. `mcp.servers`): generic key removal with no
        // reference cascade — nothing to preview.
        return axum::Json(DeletePlanResponse {
            path: q.path,
            key: q.key,
            allowed: true,
            blockers: Vec::new(),
            scrubs: Vec::new(),
            live_acp_sessions: None,
            cascades_owned_state: false,
        })
        .into_response();
    };
    if let Some(message) = unsupported_delete_cascade_message(&kind) {
        return error_response(
            ConfigApiError::new(ConfigApiCode::OpNotSupported, message)
                .with_path(format!("{}.{}", q.path, q.key)),
        );
    }
    let plan = zeroclaw_config::alias_refs::plan_delete(&config, &kind, &q.key);
    let is_agent = matches!(kind, zeroclaw_config::alias_refs::AliasKind::Agent);
    // For agents the live-ACP gate also blocks; it fails closed (an error
    // counting sessions ⇒ "not allowed"), matching the real delete.
    let live_acp = if is_agent {
        crate::agent_owned_state::live_acp_session_count(&config, &q.key).ok()
    } else {
        None
    };
    let allowed = plan.allowed && (!is_agent || live_acp == Some(0));
    axum::Json(DeletePlanResponse {
        path: q.path,
        key: q.key,
        allowed,
        blockers: plan.blockers.iter().map(to_dto).collect(),
        scrubs: plan.scrubs.iter().map(to_dto).collect(),
        live_acp_sessions: live_acp,
        cascades_owned_state: is_agent,
    })
    .into_response()
}

fn unsupported_delete_cascade_message(
    kind: &zeroclaw_config::alias_refs::AliasKind,
) -> Option<&'static str> {
    use zeroclaw_config::alias_refs::{AliasKind, ProviderCategory};
    match kind {
        AliasKind::Provider {
            category: ProviderCategory::Tts | ProviderCategory::Transcription,
            ..
        } => Some("TTS/transcription provider delete-with-cascade is not yet implemented"),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RenameMapKeyBody {
    /// Section path, e.g. `channels.discord` or `model_providers.anthropic`.
    pub path: String,
    /// Current alias name.
    pub from: String,
    /// New alias name.
    pub to: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RenameMapKeyResponse {
    pub path: String,
    pub from: String,
    pub to: String,
    pub renamed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Map a [`RenameError`](zeroclaw_config::alias_refs::RenameError) to the HTTP
/// error response (NotFound→404, InvalidName/Reserved→400, PostCondition→500).
fn rename_error_response(
    path: &str,
    from: &str,
    err: zeroclaw_config::alias_refs::RenameError,
) -> Response {
    use zeroclaw_config::alias_refs::RenameError;
    let (code, msg) = match err {
        RenameError::NotFound(p) => (
            ConfigApiCode::PathNotFound,
            format!("{p} is not configured"),
        ),
        RenameError::InvalidName(m) => (ConfigApiCode::ValidationFailed, m),
        RenameError::Reserved(a) => (
            ConfigApiCode::ValidationFailed,
            format!("alias `{a}` is reserved and cannot be renamed"),
        ),
        RenameError::PostCondition(m) => (
            ConfigApiCode::InternalError,
            format!("rename cascade post-condition failed: {m}"),
        ),
    };
    error_response(ConfigApiError::new(code, msg).with_path(format!("{path}.{from}")))
}

/// Map a [`CascadeError`](zeroclaw_config::alias_refs::CascadeError) to the
/// HTTP error response for config-only alias deletes. `Refused` and
/// `NotImplemented` are expected operator-facing outcomes, while
/// `PostCondition` is an internal guard failure and must not be persisted.
fn delete_error_response(
    path: &str,
    key: &str,
    err: zeroclaw_config::alias_refs::CascadeError,
) -> Response {
    use zeroclaw_config::alias_refs::CascadeError;
    let (code, msg) = match err {
        CascadeError::Refused(report) => {
            let blockers: Vec<_> = report.blockers.iter().map(|b| b.path.as_str()).collect();
            let detail = if blockers.is_empty() {
                "hard references remain".to_string()
            } else {
                format!("hard reference(s) remain: {}", blockers.join(", "))
            };
            (
                ConfigApiCode::ValidationFailed,
                format!("cannot delete alias `{key}`: {detail}"),
            )
        }
        CascadeError::NotFound(p) => (
            ConfigApiCode::PathNotFound,
            format!("{p} is not configured"),
        ),
        CascadeError::NotImplemented(m) => (ConfigApiCode::OpNotSupported, m),
        CascadeError::PostCondition(m) => (
            ConfigApiCode::InternalError,
            format!("delete cascade post-condition failed: {m}"),
        ),
    };
    error_response(ConfigApiError::new(code, msg).with_path(format!("{path}.{key}")))
}

pub async fn handle_rename_map_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<RenameMapKeyBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let working = state.config.read().clone();

    match zeroclaw_config::alias_refs::alias_kind_for_map_path(&body.path) {
        Some(zeroclaw_config::alias_refs::AliasKind::Agent) => {
            rename_agent_cascade(&state, working, &body).await
        }
        Some(kind) => rename_config_cascade(&state, working, &kind, &body).await,
        None => {
            // Non-aliased section: the generic key-swap rename (unchanged).
            let mut working = working;
            let renamed = match working.rename_map_key(&body.path, &body.from, &body.to) {
                Ok(b) => b,
                Err(msg) => {
                    return error_response(
                        ConfigApiError::new(ConfigApiCode::ValidationFailed, msg)
                            .with_path(&body.path),
                    );
                }
            };
            if renamed {
                working.mark_dirty(&format!("{}.{}", body.path, body.from));
                working.mark_dirty(&format!("{}.{}", body.path, body.to));
                if let Err(e) = persist_and_swap(&state, working).await {
                    return error_response(e);
                }
            }
            axum::Json(RenameMapKeyResponse {
                path: body.path,
                from: body.from,
                to: body.to,
                renamed,
                warnings: Vec::new(),
            })
            .into_response()
        }
    }
}

/// Config-only rename cascade for providers/channels (no owned state): rewrite
/// references, mark every touched path dirty, persist.
async fn rename_config_cascade(
    state: &AppState,
    mut working: zeroclaw_config::schema::Config,
    kind: &zeroclaw_config::alias_refs::AliasKind,
    body: &RenameMapKeyBody,
) -> Response {
    let report = match zeroclaw_config::alias_refs::rename_with_cascade(
        &mut working,
        kind,
        &body.from,
        &body.to,
    ) {
        Ok(r) => r,
        Err(e) => return rename_error_response(&body.path, &body.from, e),
    };
    for path in &report.dirty_paths {
        working.mark_dirty(path);
    }
    if let Err(e) = persist_and_swap(state, working).await {
        return error_response(e);
    }
    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"path": body.path, "from": body.from, "to": body.to, "dirty_paths": report.dirty_paths.len()})), "alias renamed with config-ref cascade");
    axum::Json(RenameMapKeyResponse {
        path: body.path.clone(),
        from: body.from.clone(),
        to: body.to.clone(),
        renamed: true,
        warnings: Vec::new(),
    })
    .into_response()
}

async fn move_renamed_workspace(
    old_ws: &std::path::Path,
    new_ws: &std::path::Path,
) -> Option<String> {
    if old_ws == new_ws || !old_ws.exists() {
        return None;
    }
    if let Some(parent) = new_ws.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    match tokio::fs::rename(old_ws, new_ws).await {
        Ok(()) => None,
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "old": old_ws.display().to_string(),
                        "new": new_ws.display().to_string(),
                        "err": err.to_string()
                    })),
                "agent rename: workspace move failed"
            );
            Some(format!(
                "workspace move {} -> {} failed: {err}",
                old_ws.display(),
                new_ws.display()
            ))
        }
    }
}

async fn rename_residue_exists(
    state: &AppState,
    working: &zeroclaw_config::schema::Config,
    from: &str,
) -> bool {
    // Workspace: the default per-alias dir for `from`. A custom/alias-independent
    // path is not moved by the cascade, so it is not residue.
    if working.agent_workspace_dir(from).exists() {
        return true;
    }

    // Short-lived clone for the DB-backed stores - never hold the lock across an
    // `.await`.
    let cfg = state.config.read().clone();

    // Cron jobs still owned by `from`.
    if zeroclaw_runtime::cron::list_jobs_by_agent(&cfg, from)
        .map(|jobs| !jobs.is_empty())
        .unwrap_or(false)
    {
        return true;
    }

    // ACP sessions (live OR killed) still owned by `from`.
    if let Ok(store) = zeroclaw_infra::acp_session_store::AcpSessionStore::new(&cfg.data_dir)
        && store
            .list_sessions_by_agent(from)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    {
        return true;
    }

    // Memory rows still attributed to `from`.
    if state.mem.count_agent(from).await.unwrap_or(0) > 0 {
        return true;
    }

    // Session-metadata attribution still pointing at `from`.
    if let Some(backend) = state.session_backend.as_ref()
        && backend.count_agent_attribution(from).unwrap_or(0) > 0
    {
        return true;
    }

    false
}

async fn rename_agent_cascade(
    state: &AppState,
    mut working: zeroclaw_config::schema::Config,
    body: &RenameMapKeyBody,
) -> Response {
    use zeroclaw_config::alias_refs::{self, AliasKind};
    let (from, to) = (&body.from, &body.to);

    // Capture the OLD workspace path while the entry still lives under `from`
    // (custom paths are read off the entry, which is about to move).
    let old_ws = working.agent_workspace_dir(from);

    let committed_to = working.agent(from).is_none() && working.agent(to).is_some();
    let dirty_count = if committed_to && rename_residue_exists(state, &working, from).await {
        0
    } else {
        match alias_refs::rename_with_cascade(&mut working, &AliasKind::Agent, from, to) {
            Ok(report) => {
                for path in &report.dirty_paths {
                    working.mark_dirty(path);
                }
                let dirty_count = report.dirty_paths.len();
                if let Err(e) = persist_and_swap(state, working).await {
                    return error_response(e);
                }
                dirty_count
            }
            Err(e) => return rename_error_response(&body.path, from, e),
        }
    };

    let cfg = state.config.read().clone();
    // The NEW workspace path off the committed config (the rewritten `to`).
    let new_ws = cfg.agent_workspace_dir(to);

    // Move the workspace dir. For the default per-alias location this is
    // `<install>/agents/<from>/workspace` → `…/<to>/workspace`. A custom
    // workspace path is alias-independent, so `old_ws == new_ws` and we skip.
    let ws_existed = old_ws != new_ws && old_ws.exists();
    let move_warning = move_renamed_workspace(&old_ws, &new_ws).await;
    let workspace_moved = ws_existed && move_warning.is_none();
    let mut warnings: Vec<String> = Vec::new();
    warnings.extend(move_warning);

    // Re-point owned DB state (memory/cron/acp/session). Best-effort + reported.
    let owned = crate::agent_owned_state::cascade_rename_agent(
        &cfg,
        &state.mem,
        state.session_backend.as_ref(),
        from,
        to,
    )
    .await;
    // Combine the workspace-move warning (if any) with the owned-store warnings
    // so every partial failure reaches the caller, not just the server log.
    warnings.extend(owned.warnings);

    // The config rename committed. A non-empty `warnings` means a post-persist
    // side-effect did not follow (config is `to`, some follower lags at `from`,
    // re-runnable) - escalate to WARN so that degraded outcome is visible
    // operationally instead of buried at INFO.
    if warnings.is_empty() {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"from": from, "to": to, "memory": owned.memory_rows, "cron": owned.cron_jobs, "acp": owned.acp_sessions, "sessions": owned.sessions_repointed, "workspace_moved": workspace_moved, "dirty_paths": dirty_count})), "agent renamed with owned-state cascade");
    } else {
        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"from": from, "to": to, "memory": owned.memory_rows, "cron": owned.cron_jobs, "acp": owned.acp_sessions, "sessions": owned.sessions_repointed, "workspace_moved": workspace_moved, "dirty_paths": dirty_count, "warnings": warnings})), "agent rename persisted but a post-persist side-effect did not follow; re-issue the rename to converge");
    }

    // Persisted rename. `warnings` carries any post-persist side-effect that did
    // not follow, so the split can be remediated rather than reported as a clean
    // success (207-style partial success).
    axum::Json(RenameMapKeyResponse {
        path: body.path.clone(),
        from: from.clone(),
        to: to.clone(),
        renamed: true,
        warnings,
    })
    .into_response()
}

pub async fn handle_refresh_context_window(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path((provider_type, alias)): axum::extract::Path<(String, String)>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut working = state.config.read().clone();
    let path = format!("providers.models.{provider_type}.{alias}");

    // Verify the entry exists
    if working.get_prop(&format!("{path}.model")).is_err() {
        return error_response(
            ConfigApiError::new(
                ConfigApiCode::PathNotFound,
                format!("model provider '{provider_type}.{alias}' not found"),
            )
            .with_path(&path),
        );
    }

    // Build minimal provider config for fetch
    let model = working
        .get_prop(&format!("{path}.model"))
        .ok()
        .unwrap_or_default();
    let uri = working.get_prop(&format!("{path}.uri")).ok();
    // Read api_key via JSON serialization to bypass #[secret] masking in get_prop.
    let api_key = serde_json::to_value(&working.providers).ok().and_then(|v| {
        v.pointer(&format!("/models/{provider_type}/{alias}/api_key"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && *s != "<unset>")
            .map(String::from)
    });

    let provider_config = zeroclaw_config::schema::ModelProviderConfig {
        model: Some(model),
        uri,
        api_key,
        ..Default::default()
    };

    // Fetch context window from provider
    let context_window = match zeroclaw_providers::fetch_context_window(
        &provider_type,
        &provider_config,
    )
    .await
    {
        Some(ctx) => ctx,
        None => {
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::InvalidFormat,
                    format!("provider '{provider_type}' does not support context window auto-detection or fetch failed"),
                )
                .with_path(&path),
            );
        }
    };

    // Update config
    if let Err(e) = working.set_prop_persistent(
        &format!("{path}.context_window"),
        &context_window.to_string(),
    ) {
        return error_response(
            ConfigApiError::new(
                ConfigApiCode::InternalError,
                format!("failed to persist context_window: {e}"),
            )
            .with_path(&path),
        );
    }

    working.mark_dirty(&format!("{path}.context_window"));
    if let Err(e) = persist_and_swap(&state, working).await {
        return error_response(e);
    }

    axum::Json(serde_json::json!({
        "path": path,
        "context_window": context_window,
    }))
    .into_response()
}

pub async fn handle_patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let ops = match parse_patch_ops(body) {
        Ok(ops) => ops,
        Err(e) => return error_response(e),
    };

    let working = state.config.read().clone();

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
        if matches!(op.op.as_str(), "add" | "replace") && working.ensure_map_key_for_path(&path) {
            // Refused to vivify the reserved `default` agent: surface the same
            // reserved error the explicit create surfaces do, not a generic 404.
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::ValidationFailed,
                    "alias `default` is reserved and cannot be created",
                )
                .with_path(&path)
                .with_op_index(idx),
            );
        }
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
                let actual_str = match working.get_prop(&path) {
                    Ok(v) => v,
                    Err(e) => return error_response(map_prop_error(e, &path).with_op_index(idx)),
                };
                let want_str = match json_to_setprop_string(&want, info.as_ref().map(|i| i.kind)) {
                    Ok(s) => s,
                    Err(e) => return error_response(e.with_path(&path).with_op_index(idx)),
                };
                if actual_str != want_str {
                    return error_response(
                        ConfigApiError::new(
                            ConfigApiCode::ValidationFailed,
                            format!("`test` op failed: expected {want_str:?}, got {actual_str:?}"),
                        )
                        .with_path(&path)
                        .with_op_index(idx),
                    );
                }
                results.push(PatchOpResult {
                    op: op.op.clone(),
                    path,
                    value: Some(serde_json::Value::String(actual_str)),
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
                if let Err(e) = working.set_prop_persistent(&path, &value_str) {
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
                if let Err(e) = working.set_prop_persistent(&path, "") {
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
            "comment" => {
                // Comment-only update: record the (path, comment) pair
                // for `apply_comments` after the patch commits, but
                // skip `set_prop` entirely. Lets the operator annotate
                // a secret without rotating its ciphertext.
                if info.is_none() {
                    return error_response(
                        ConfigApiError::path_not_found(&path).with_op_index(idx),
                    );
                }
                let Some(comment) = op.comment.clone() else {
                    return error_response(
                        ConfigApiError::new(
                            ConfigApiCode::ValueTypeMismatch,
                            "JSON Patch `comment` op requires `comment` field",
                        )
                        .with_path(&path)
                        .with_op_index(idx),
                    );
                };
                results.push(PatchOpResult {
                    op: op.op.clone(),
                    path,
                    value: None,
                    populated: None,
                    comment: Some(comment),
                });
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

    // Per-PATCH validation is scoped to the dirty paths. See
    // `scoped_validate` for the contract.
    let scoped_validation_warnings = match scoped_validate(&working) {
        Ok(ws) => ws,
        Err(err) => return error_response(err),
    };

    // Collect (path, comment) pairs from any op that supplied a non-None
    // comment. Applied after save() so the comment-preserving sync_table
    // pass doesn't strip them.
    let annotations: Vec<(String, String)> = ops
        .iter()
        .zip(results.iter())
        .filter_map(|(op, res)| op.comment.as_ref().map(|c| (res.path.clone(), c.clone())))
        .collect();

    let config_path = working.config_path.clone();
    // Collect non-fatal validation warnings against the post-save state
    // before working is moved into persist_and_swap. Same signal as
    // `zeroclaw_log::record!` from `validate()`, surfaced structured so dashboard
    // callers see it.
    let mut warnings = working.collect_warnings();
    warnings.extend(scoped_validation_warnings);
    if let Err(e) = persist_and_swap(&state, working).await {
        return error_response(e);
    }
    if !annotations.is_empty()
        && let Err(e) =
            zeroclaw_config::comment_writer::apply_comments(&config_path, &annotations).await
    {
        // Comments are best-effort decoration; surface as a non-fatal warn.
        // The patch itself succeeded — return success but log the failure.
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "failed to apply PATCH op comments to config.toml"
        );
    }

    axum::Json(PatchResponse {
        saved: true,
        results,
        warnings,
    })
    .into_response()
}

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
    /// Optional section prefix to scope the init pass (e.g. `model_providers`).
    /// Without it, every uninitialized nested section gets its defaults.
    #[serde(default)]
    pub section: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct InitResponse {
    pub initialized: Vec<String>,
}

/// POST /api/config/init?section=model_providers — instantiate `None` nested
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

    let mut working = state.config.read().clone();
    let initialized: Vec<String> = working
        .init_defaults(q.section.as_deref())
        .into_iter()
        .map(str::to_string)
        .collect();

    if initialized.is_empty() {
        return axum::Json(InitResponse { initialized }).into_response();
    }

    for section in &initialized {
        working.mark_dirty(section);
    }

    if let Err(err) = scoped_validate(&working) {
        return error_response(err);
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

pub async fn handle_migrate(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config_path = state.config.read().config_path.clone();

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
            let parent = match config_path.parent() {
                Some(p) => p.to_path_buf(),
                None => {
                    return error_response(ConfigApiError::new(
                        ConfigApiCode::InternalError,
                        format!(
                            "config path has no parent: {}",
                            config_path.display().to_string()
                        ),
                    ));
                }
            };
            let file_name = match config_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => {
                    return error_response(ConfigApiError::new(
                        ConfigApiCode::InternalError,
                        format!(
                            "config path has no file name: {}",
                            config_path.display().to_string()
                        ),
                    ));
                }
            };
            let temp_path = parent.join(format!(".{file_name}.tmp-{}", uuid::Uuid::new_v4()));

            // 1. Write migrated content to temp + fsync.
            match tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)
                .await
            {
                Ok(mut temp) => {
                    use tokio::io::AsyncWriteExt;
                    if let Err(e) = temp.write_all(new_content.as_bytes()).await {
                        let _ = tokio::fs::remove_file(&temp_path).await;
                        return error_response(ConfigApiError::new(
                            ConfigApiCode::InternalError,
                            format!("failed to write migrated config to temp: {e}"),
                        ));
                    }
                    if let Err(e) = temp.sync_all().await {
                        let _ = tokio::fs::remove_file(&temp_path).await;
                        return error_response(ConfigApiError::new(
                            ConfigApiCode::InternalError,
                            format!("failed to fsync migrated config temp: {e}"),
                        ));
                    }
                }
                Err(e) => {
                    return error_response(ConfigApiError::new(
                        ConfigApiCode::InternalError,
                        format!("failed to create temp config file: {e}"),
                    ));
                }
            }

            // 2. Backup BEFORE replacing the original.
            if let Err(e) = tokio::fs::copy(&config_path, &backup_path).await {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return error_response(ConfigApiError::new(
                    ConfigApiCode::InternalError,
                    format!("failed to write backup: {e}"),
                ));
            }

            // 3. Atomic rename. On failure, restore from backup.
            if let Err(e) = tokio::fs::rename(&temp_path, &config_path).await {
                let _ = tokio::fs::remove_file(&temp_path).await;
                if backup_path.exists() {
                    let _ = tokio::fs::copy(&backup_path, &config_path).await;
                }
                return error_response(ConfigApiError::new(
                    ConfigApiCode::InternalError,
                    format!("failed to atomically replace config: {e}"),
                ));
            }

            // 4. Fsync the parent directory so the rename is durable.
            #[cfg(unix)]
            if let Ok(dir) = tokio::fs::File::open(&parent).await {
                let _ = dir.sync_all().await;
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
            *state.config.write() = new_cfg;

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
    let config = state.config.read().clone();
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
    use crate::{GatewayRateLimiter, IdempotencyStore, nodes};
    use async_trait::async_trait;
    use axum::http::StatusCode;
    use http_body_util::BodyExt;
    use parking_lot::RwLock;
    use std::sync::Arc;
    use std::time::Duration;
    use zeroclaw_providers::ModelProvider;
    use zeroclaw_runtime::security::pairing::PairingGuard;

    // dirty_entry_for / CascadeReport::dirty_paths tests live in
    // zeroclaw_config::alias_refs — single source of truth (the gateway and CLI
    // both consume the promoted helper).

    #[derive(Default)]
    struct MockModelProvider;

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".into())
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for MockModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "MockModelProvider"
        }
    }

    fn temp_config(tmp: &tempfile::TempDir) -> zeroclaw_config::schema::Config {
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir,
            ..Default::default()
        }
    }

    fn test_state(config: zeroclaw_config::schema::Config) -> AppState {
        let memory: Arc<dyn zeroclaw_memory::Memory> =
            Arc::new(zeroclaw_memory::NoneMemory::new("api-config-test"));
        AppState {
            config: Arc::new(RwLock::new(config)),
            model_provider: Arc::new(MockModelProvider),
            model: "test-model".into(),
            temperature: None,
            mem: memory.clone(),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    memory,
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(GatewayRateLimiter::new(100, 100, 100, 100)),
            auth_limiter: Arc::new(crate::auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1000)),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: std::collections::HashMap::new(),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: std::collections::HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq: std::collections::HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: std::collections::HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: std::collections::HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: std::collections::HashMap::new(),
            #[cfg(feature = "channel-wati")]
            wati: std::collections::HashMap::new(),
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(zeroclaw_runtime::observability::NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            tools_registry_by_agent: Arc::new(std::collections::HashMap::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(crate::sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            reload_tx: None,
            node_registry: Arc::new(nodes::NodeRegistry::new(16)),
            path_prefix: String::new(),
            web_dist_dir: None,
            session_backend: None,
            session_queue: Arc::new(crate::session_queue::SessionActorQueue::new(8, 30, 600)),
            device_registry: None,
            pending_pairings: None,
            canvas_store: zeroclaw_runtime::tools::CanvasStore::new(),
            #[cfg(feature = "webauthn")]
            webauthn: None,
            cancel_tokens: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            sop_engine: None,
            sop_audit: None,
        }
    }

    async fn response_json(response: Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let json = serde_json::from_slice(&body).expect("valid json response");
        (status, json)
    }

    #[tokio::test]
    async fn delete_map_key_handler_cascades_model_provider_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = temp_config(&tmp);
        config
            .providers
            .models
            .ensure("anthropic", "default")
            .unwrap();
        config
            .providers
            .models
            .ensure("openai", "main")
            .unwrap()
            .fallback = vec!["anthropic.default".into()];
        config.agents.insert(
            "triage".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                classifier_provider: "anthropic.default".into(),
                ..Default::default()
            },
        );
        config.save().await.unwrap();

        let state = test_state(config);
        let (status, json) = response_json(
            handle_delete_map_key(
                axum::extract::State(state.clone()),
                axum::http::HeaderMap::new(),
                axum::extract::Query(MapKeyQuery {
                    path: "providers.models.anthropic".to_string(),
                    key: "default".to_string(),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["path"], "providers.models.anthropic");
        assert_eq!(json["key"], "default");
        let cfg = state.config.read();
        assert!(cfg.providers.models.find("anthropic", "default").is_none());
        assert!(cfg.agents["triage"].classifier_provider.is_empty());
        assert!(
            cfg.providers
                .models
                .find("openai", "main")
                .unwrap()
                .fallback
                .is_empty()
        );
        drop(cfg);
        let written = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
        assert!(!written.contains("anthropic.default"));
    }

    #[tokio::test]
    async fn delete_map_key_handler_refuses_model_provider_hard_ref_without_mutation() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = temp_config(&tmp);
        config
            .providers
            .models
            .ensure("anthropic", "default")
            .unwrap();
        config.agents.insert(
            "researcher".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "anthropic.default".into(),
                ..Default::default()
            },
        );
        config.save().await.unwrap();

        let state = test_state(config);
        let (status, json) = response_json(
            handle_delete_map_key(
                axum::extract::State(state.clone()),
                axum::http::HeaderMap::new(),
                axum::extract::Query(MapKeyQuery {
                    path: "providers.models.anthropic".to_string(),
                    key: "default".to_string(),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["code"], "validation_failed");
        let cfg = state.config.read();
        assert!(cfg.providers.models.find("anthropic", "default").is_some());
        assert_eq!(
            cfg.agents["researcher"].model_provider.as_str(),
            "anthropic.default"
        );
    }

    #[tokio::test]
    async fn delete_map_key_handler_cascades_channel_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = temp_config(&tmp);
        config.create_map_key("channels.discord", "main").unwrap();
        config.agents.insert(
            "ops".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                channels: vec!["discord.main".into()],
                ..Default::default()
            },
        );
        config
            .escalation
            .alert_channels
            .push("discord.main".to_string());
        config.save().await.unwrap();

        let state = test_state(config);
        let (status, json) = response_json(
            handle_delete_map_key(
                axum::extract::State(state.clone()),
                axum::http::HeaderMap::new(),
                axum::extract::Query(MapKeyQuery {
                    path: "channels.discord".to_string(),
                    key: "main".to_string(),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["path"], "channels.discord");
        assert_eq!(json["key"], "main");
        let cfg = state.config.read();
        assert!(
            !cfg.get_map_keys("channels.discord")
                .unwrap_or_default()
                .iter()
                .any(|k| k == "main")
        );
        assert!(cfg.agents["ops"].channels.is_empty());
        assert!(cfg.escalation.alert_channels.is_empty());
        drop(cfg);
        let written = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
        assert!(!written.contains("discord.main"));
    }

    #[tokio::test]
    async fn delete_plan_rejects_unsupported_tts_provider_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = temp_config(&tmp);
        config
            .create_map_key("providers.tts.elevenlabs", "default")
            .unwrap();
        config.agents.insert(
            "voice".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                tts_provider: "elevenlabs.default".into(),
                ..Default::default()
            },
        );

        let state = test_state(config);
        let (status, json) = response_json(
            handle_delete_plan(
                axum::extract::State(state),
                axum::http::HeaderMap::new(),
                axum::extract::Query(MapKeyQuery {
                    path: "providers.tts.elevenlabs".to_string(),
                    key: "default".to_string(),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["code"], "op_not_supported");
        assert_eq!(json["path"], "providers.tts.elevenlabs.default");
    }

    #[tokio::test]
    async fn delete_map_key_handler_rejects_unsupported_tts_without_raw_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = temp_config(&tmp);
        config
            .create_map_key("providers.tts.elevenlabs", "default")
            .unwrap();
        config.save().await.unwrap();

        let state = test_state(config);
        let (status, json) = response_json(
            handle_delete_map_key(
                axum::extract::State(state.clone()),
                axum::http::HeaderMap::new(),
                axum::extract::Query(MapKeyQuery {
                    path: "providers.tts.elevenlabs".to_string(),
                    key: "default".to_string(),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["code"], "op_not_supported");
        assert!(
            state
                .config
                .read()
                .get_map_keys("providers.tts.elevenlabs")
                .unwrap_or_default()
                .iter()
                .any(|k| k == "default"),
            "unsupported provider delete must not fall back to raw deletion"
        );
    }

    #[test]
    fn delete_cascade_resolves_custom_workspace_before_removing_entry() {
        let custom = std::path::PathBuf::from("/var/lib/zc-test/custom-victim-ws");
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.agents.insert(
            "victim".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        cfg.agents.get_mut("victim").unwrap().workspace.path = Some(custom.clone());

        // While the entry exists → the custom path (what the handler captures).
        assert_eq!(cfg.agent_workspace_dir("victim"), custom);

        // After the cascade removes the entry → it falls back to the DEFAULT
        // path; that is exactly why resolution must happen before the cascade.
        zeroclaw_config::alias_refs::delete_with_cascade(
            &mut cfg,
            &zeroclaw_config::alias_refs::AliasKind::Agent,
            "victim",
            zeroclaw_config::alias_refs::CascadePolicy::RefuseOnHard,
        )
        .expect("soft-only agent delete succeeds");
        assert!(!cfg.agents.contains_key("victim"));
        assert_ne!(
            cfg.agent_workspace_dir("victim"),
            custom,
            "after removal the custom workspace path defaults — resolve BEFORE the cascade"
        );
    }

    #[tokio::test]
    async fn renamed_workspace_move_failure_is_surfaced() {
        // A failed workspace move during rename must surface a warning (so the
        // caller learns config/DB moved to `to` while the workspace is stranded
        // at `from`), not be swallowed as a clean success.
        let tmp = tempfile::tempdir().unwrap();
        let old_ws = tmp.path().join("from-ws");
        std::fs::create_dir_all(&old_ws).unwrap();
        // Force the move to fail: new_ws's parent is a FILE, so create_dir_all
        // and rename both fail.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let new_ws = blocker.join("to-ws");

        let warning = move_renamed_workspace(&old_ws, &new_ws).await;
        assert!(
            warning.is_some(),
            "a failed workspace move must surface a warning"
        );
        assert!(warning.unwrap().contains("workspace move"));
        assert!(old_ws.exists(), "source dir stays put when the move fails");

        // Nothing-to-move paths return None (no spurious warning).
        assert!(move_renamed_workspace(&old_ws, &old_ws).await.is_none());
        let missing = tmp.path().join("does-not-exist");
        assert!(move_renamed_workspace(&missing, &new_ws).await.is_none());
    }

    #[tokio::test]
    async fn agent_rename_leaves_owned_state_put_when_persist_fails() {
        let tmp = tempfile::tempdir().unwrap();
        // Force config persistence to FAIL by making `config_path` itself a
        // directory - save_dirty's atomic write can't replace a dir. Its parent
        // (the install root) stays a real dir, so the agent-workspace creation
        // and the cron seed below still work. data_dir is separate + writable.
        let cfg_dir = tmp.path().join("config.toml");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let mut config = zeroclaw_config::schema::Config {
            config_path: cfg_dir,
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        // Agent under `from` with a resolvable risk_profile + an allowed cron
        // command, so cron::add_job accepts a job tied to the agent.
        let from_agent = zeroclaw_config::schema::AliasedAgentConfig {
            risk_profile: "default".into(),
            ..Default::default()
        };
        config.agents.insert("from".to_string(), from_agent);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        config.runtime_profiles.entry("default".into()).or_default();

        // Seed an owned-state row (a cron job) under `from` - the move-probe.
        zeroclaw_runtime::cron::add_job(&config, "from", "* * * * *", "echo hi")
            .expect("seed cron job");
        assert_eq!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "from")
                .unwrap()
                .len(),
            1
        );

        let state = crate::api::test_state(config.clone());
        let body = RenameMapKeyBody {
            path: "agents".to_string(),
            from: "from".to_string(),
            to: "to".to_string(),
        };
        let resp = rename_agent_cascade(&state, config.clone(), &body).await;

        // Persist failed -> error response, not a clean rename.
        assert!(
            !resp.status().is_success(),
            "a failed config persist must surface an error"
        );
        // Owned state did NOT move: the cron job stays under `from`.
        assert_eq!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "from")
                .unwrap()
                .len(),
            1,
            "cron must stay under `from` when persist fails (no premature move)"
        );
        assert!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "to")
                .unwrap()
                .is_empty(),
            "cron must NOT have moved to `to` when persist fails"
        );
        // In-memory config was never swapped: still names `from`.
        assert!(state.config.read().agents.contains_key("from"));
        assert!(!state.config.read().agents.contains_key("to"));
    }

    #[tokio::test]
    async fn agent_rename_moves_owned_state_after_successful_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"), // writable -> persist OK
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        // Agent under `from` with a resolvable risk_profile + an allowed cron
        // command, so cron::add_job accepts a job tied to the agent.
        let from_agent = zeroclaw_config::schema::AliasedAgentConfig {
            risk_profile: "default".into(),
            ..Default::default()
        };
        config.agents.insert("from".to_string(), from_agent);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        config.runtime_profiles.entry("default".into()).or_default();
        // Create the agent's default workspace dir so the move has something to move.
        let old_ws = config.agent_workspace_dir("from");
        std::fs::create_dir_all(&old_ws).unwrap();
        zeroclaw_runtime::cron::add_job(&config, "from", "* * * * *", "echo hi")
            .expect("seed cron job");

        let state = crate::api::test_state(config.clone());
        let body = RenameMapKeyBody {
            path: "agents".to_string(),
            from: "from".to_string(),
            to: "to".to_string(),
        };
        let resp = rename_agent_cascade(&state, config.clone(), &body).await;
        assert!(resp.status().is_success(), "a clean rename returns success");

        // Config swapped to `to`.
        assert!(state.config.read().agents.contains_key("to"));
        assert!(!state.config.read().agents.contains_key("from"));
        // Cron re-pointed to `to` - the move happened, after a successful persist.
        assert_eq!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "to")
                .unwrap()
                .len(),
            1,
            "cron moves to `to` once persist succeeds"
        );
        assert!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "from")
                .unwrap()
                .is_empty()
        );
        // Workspace moved to the new alias path.
        assert!(
            state.config.read().agent_workspace_dir("to").exists(),
            "workspace moved to `to`"
        );
        assert!(!old_ws.exists(), "old workspace no longer present");
        // (MockMemory.rename_agent is unsupported, so the response `warnings`
        // carries that one known memory line - cron + workspace prove the move.)
    }

    #[tokio::test]
    async fn agent_rename_resume_converges_when_config_already_to() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"), // writable
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let from_agent = zeroclaw_config::schema::AliasedAgentConfig {
            risk_profile: "default".into(),
            ..Default::default()
        };
        config.agents.insert("from".to_string(), from_agent);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        config.runtime_profiles.entry("default".into()).or_default();

        // Seed the lagging owned state + workspace under `from` (added while
        // `from` is still a known agent so cron::add_job validates).
        zeroclaw_runtime::cron::add_job(&config, "from", "* * * * *", "echo hi")
            .expect("seed lagged cron job under `from`");
        let old_ws = config.agent_workspace_dir("from");
        std::fs::create_dir_all(&old_ws).unwrap();

        // Simulate the post-persist window: config already committed the rename
        // to `to` (so `from` is gone from config), while the cron row + workspace
        // above still lag at `from`. The cron DB lives under data_dir and survives
        // this in-memory config edit.
        config.agents.remove("from");
        config.agents.insert(
            "to".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                risk_profile: "default".into(),
                ..Default::default()
            },
        );

        let state = crate::api::test_state(config.clone());
        let body = RenameMapKeyBody {
            path: "agents".to_string(),
            from: "from".to_string(),
            to: "to".to_string(),
        };
        // Re-issue the SAME rename. Beforethis returned 404 (from absent in
        // the committed config); now it resumes and re-runs the lagging effects.
        let resp = rename_agent_cascade(&state, config.clone(), &body).await;
        assert!(
            resp.status().is_success(),
            "re-issuing a rename after a post-persist lag must converge, not 404"
        );

        // Owned state converged onto `to`.
        assert_eq!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "to")
                .unwrap()
                .len(),
            1,
            "lagged cron re-points to `to` on resume"
        );
        assert!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "from")
                .unwrap()
                .is_empty(),
            "no cron left under `from` after convergence"
        );
        // Workspace converged onto `to`.
        assert!(
            state.config.read().agent_workspace_dir("to").exists(),
            "workspace moved to `to` on resume"
        );
        assert!(!old_ws.exists(), "old `from` workspace no longer present");
        // Config still names `to` and never regained `from` (no double-rename).
        assert!(state.config.read().agents.contains_key("to"));
        assert!(!state.config.read().agents.contains_key("from"));
    }

    #[tokio::test]
    async fn agent_rename_unrelated_collision_is_not_treated_as_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"), // writable
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();

        // Committed-`to` shape: config has `to`, the source `gone` is absent.
        // Crucially there is NO residue under `gone` - no workspace dir, no cron
        // job, no acp/memory/session rows. This is an unrelated request (or an
        // already-fully-converged duplicate), not a partial-failure resume.
        let mut config = config;
        config.agents.insert(
            "to".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                risk_profile: "default".into(),
                ..Default::default()
            },
        );
        config.risk_profiles.entry("default".into()).or_default();
        config.runtime_profiles.entry("default".into()).or_default();
        // Guard the test's own premise: the source workspace must not exist.
        assert!(
            !config.agent_workspace_dir("gone").exists(),
            "precondition: no residue workspace under the absent source"
        );

        let state = crate::api::test_state(config.clone());
        let body = RenameMapKeyBody {
            path: "agents".to_string(),
            from: "gone".to_string(),
            to: "to".to_string(),
        };
        let resp = rename_agent_cascade(&state, config.clone(), &body).await;

        // No residue → NOT a resume → the normal branch runs `rename_with_cascade`
        // with `gone` absent → NotFound → an error response, not a silent success.
        assert!(
            !resp.status().is_success(),
            "an unrelated `gone -> to` with no residue must surface an error, not be silently treated as a resume"
        );
        // Config untouched: no rename happened, `to` still present, `gone` absent.
        assert!(state.config.read().agents.contains_key("to"));
        assert!(!state.config.read().agents.contains_key("gone"));
    }

    #[test]
    fn map_prop_error_classifies_unknown_property() {
        let err = anyhow::Error::msg("Unknown property 'foo.bar'");
        let api_err = map_prop_error(err, "foo.bar");
        assert_eq!(api_err.code, ConfigApiCode::PathNotFound);
    }

    #[test]
    fn map_prop_error_classifies_type_mismatch() {
        // The classifier (config::api_error::classify_validation_message) now
        // matches "type mismatch" → ValueTypeMismatch; was ValidationFailed.
        let err = anyhow::Error::msg("type mismatch: expected u64");
        let api_err = map_prop_error(err, "scheduler.max_concurrent");
        assert_eq!(api_err.code, ConfigApiCode::ValueTypeMismatch);
    }

    #[test]
    fn map_prop_error_falls_back_to_validation_on_unknown_message() {
        let err = anyhow::Error::msg("some completely unrecognized validator message");
        let api_err = map_prop_error(err, "scheduler.max_concurrent");
        assert_eq!(api_err.code, ConfigApiCode::ValidationFailed);
    }

    #[test]
    fn json_pointer_to_dotted_handles_pointer_form() {
        assert_eq!(
            json_pointer_to_dotted("/providers/models/openrouter/api-key"),
            "providers.models.openrouter.api-key"
        );
    }

    #[test]
    fn json_pointer_to_dotted_passes_dotted_through() {
        assert_eq!(
            json_pointer_to_dotted("providers.models.openrouter.api-key"),
            "providers.models.openrouter.api-key"
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

    use zeroclaw_config::traits::PropKind;

    #[test]
    fn test_op_coercion_bool_typed_value_matches_stored() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.risk_profiles.insert(
            "default".into(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        cfg.set_prop("risk_profiles.default.workspace_only", "true")
            .expect("set_prop bool");
        let actual = cfg
            .get_prop("risk_profiles.default.workspace_only")
            .expect("get_prop");
        let want_typed = json_to_setprop_string(&serde_json::json!(true), Some(PropKind::Bool))
            .expect("coerce bool true");
        assert_eq!(
            actual, want_typed,
            "bool field: typed JSON `true` must coerce to the same display string \
             as `get_prop` returns; got actual={actual:?} want_typed={want_typed:?}"
        );

        // Legacy string-form (`Value::String("true")`) for the same bool
        // field must also coerce to the same string — back-compat for
        // clients that send strings instead of booleans.
        let want_string = json_to_setprop_string(&serde_json::json!("true"), Some(PropKind::Bool))
            .expect("coerce bool from string");
        assert_eq!(actual, want_string);
    }

    #[test]
    fn test_op_coercion_integer_typed_value_matches_stored() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.set_prop("gateway.port", "42617")
            .expect("set_prop integer");
        let actual = cfg.get_prop("gateway.port").expect("get_prop");
        let want_typed = json_to_setprop_string(&serde_json::json!(42617), Some(PropKind::Integer))
            .expect("coerce integer");
        assert_eq!(
            actual, want_typed,
            "integer field coercion: actual={actual:?} want_typed={want_typed:?}"
        );

        // Legacy string-form must also coerce equivalently.
        let want_string =
            json_to_setprop_string(&serde_json::json!("42617"), Some(PropKind::Integer))
                .expect("coerce integer from string");
        assert_eq!(actual, want_string);
    }

    #[test]
    fn test_op_coercion_float_typed_value_matches_stored() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        // autonomy doesn't carry floats today; use a model_provider temperature
        // by setting a known model provider entry. The model providers map
        // is set up via map keys, so use a path that's unambiguously float.
        // Fall back to set_prop on a known float location:
        match cfg.set_prop("providers.models.openai.temperature", "0.7") {
            Ok(()) => {
                let actual = cfg
                    .get_prop("providers.models.openai.temperature")
                    .expect("get_prop float");
                let want_typed =
                    json_to_setprop_string(&serde_json::json!(0.7), Some(PropKind::Float))
                        .expect("coerce float typed");
                assert_eq!(
                    actual, want_typed,
                    "float field coercion: actual={actual:?} want_typed={want_typed:?}"
                );
            }
            Err(_) => {
                // Float path not available on default Config — skip without
                // failing. The bool and integer tests cover the same
                // invariant; float just pins the additional case.
            }
        }
    }

    #[test]
    fn test_op_coercion_string_field_no_regression() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.set_prop("gateway.host", "10.0.0.1")
            .expect("set_prop string");
        let actual = cfg.get_prop("gateway.host").expect("get_prop string");
        let want_typed =
            json_to_setprop_string(&serde_json::json!("10.0.0.1"), Some(PropKind::String))
                .expect("coerce string");
        assert_eq!(actual, want_typed);
    }

    #[test]
    fn test_op_coercion_mismatched_value_correctly_fails() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.risk_profiles.insert(
            "default".into(),
            zeroclaw_config::schema::RiskProfileConfig::default(),
        );
        cfg.set_prop("risk_profiles.default.workspace_only", "true")
            .expect("set_prop");
        let actual = cfg
            .get_prop("risk_profiles.default.workspace_only")
            .expect("get_prop");
        let want = json_to_setprop_string(&serde_json::json!(false), Some(PropKind::Bool))
            .expect("coerce bool false");
        assert_ne!(
            actual, want,
            "bool true must not match bool false after coercion — \
             a mismatched test op should fail with ValidationFailed"
        );
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
        use zeroclaw_runtime::agent::loop_::scrub_credentials;

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
    fn lookup_prop_field_synthesizes_dynamic_http_request_secret_metadata() {
        let cfg = zeroclaw_config::schema::Config::default();
        let field = lookup_prop_field(&cfg, "http_request.secrets.api_token")
            .expect("dynamic http_request secret metadata");

        assert_eq!(field.kind, PropKind::String);
        assert!(field.is_secret);
        assert_eq!(
            field.credential_class,
            Some(zeroclaw_config::traits::CredentialSurfaceClass::EncryptedSecret)
        );
    }

    #[test]
    fn list_entry_for_secret_omits_value_field() {
        let entry = ListEntry {
            path: "providers.models.ollama.api-key".into(),
            category: "providers.models".into(),
            kind: "string",
            type_hint: "Option<String>",
            value: None,
            populated: true,
            is_secret: true,
            is_env_overridden: false,
            enum_variants: vec![],
            section: Some("providers.models"),
            tab: "",
            multiline: false,
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
    fn gateway_paired_tokens_is_gateway_managed() {
        // The `Configurable` derive emits prop-field names in the field's
        // snake_case form, so the canonical name is `gateway.paired_tokens`
        // (underscore). The matcher must use that exact string, otherwise the
        // guard never fires and the secret keeps surfacing as drift.
        assert!(
            is_gateway_managed_field("gateway.paired_tokens"),
            "gateway.paired_tokens must be treated as gateway-managed"
        );
        // The old hyphenated form never matched a real prop-field name.
        assert!(!is_gateway_managed_field("gateway.paired-tokens"));

        // Guard against the field being renamed or the derive changing its
        // naming convention out from under the matcher.
        let cfg = zeroclaw_config::schema::Config::default();
        assert!(
            cfg.prop_fields()
                .iter()
                .any(|p| p.name == "gateway.paired_tokens"),
            "expected a prop-field named gateway.paired_tokens"
        );
    }

    #[tokio::test]
    async fn compute_drift_excludes_gateway_paired_tokens() {
        let (_tmp, path) = temp_config_path();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: path.clone(),
            ..Default::default()
        };
        cfg.save().await.expect("initial save");

        // Mutate the gateway-managed secret in memory without saving. Drift
        // detection must not surface it because the gateway owns it.
        cfg.gateway.paired_tokens = vec!["minted-by-the-gateway".into()];

        let drift = compute_drift(&cfg).await;
        assert!(
            !drift.iter().any(|d| d.path == "gateway.paired_tokens"),
            "gateway.paired_tokens must never appear in drift, got {drift:?}"
        );
    }

    #[tokio::test]
    async fn compute_drift_excludes_env_overridden_secret() {
        let (_tmp, path) = temp_config_path();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: path.clone(),
            ..Default::default()
        };
        cfg.save().await.expect("initial save");

        cfg.composio.api_key = Some("injected-via-env".into());
        cfg.env_overridden_paths =
            std::collections::HashSet::from(["composio.api_key".to_string()]);

        let drift = compute_drift(&cfg).await;
        assert!(
            !drift.iter().any(|d| d.path == "composio.api_key"),
            "env-overridden secret must never appear in drift, got {drift:?}"
        );
    }

    #[test]
    fn every_gateway_secret_is_classified() {
        const OPERATOR_EDITED_GATEWAY_SECRETS: &[&str] = &[];

        let cfg = zeroclaw_config::schema::Config::default();
        let unclassified: Vec<String> = cfg
            .prop_fields()
            .iter()
            .filter(|p| p.is_secret && p.name.starts_with("gateway."))
            .map(|p| p.name.clone())
            .filter(|name| {
                !is_gateway_managed_field(name)
                    && !OPERATOR_EDITED_GATEWAY_SECRETS.contains(&name.as_str())
            })
            .collect();

        assert!(
            unclassified.is_empty(),
            "new [gateway] secret field(s) {unclassified:?} are not classified.\n\
             If the gateway mints/rotates/persists this field itself, add it to \
             `is_gateway_managed_field`.\n\
             If operators edit it directly in config.toml, add it to the \
             OPERATOR_EDITED_GATEWAY_SECRETS list in this test."
        );
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

    #[tokio::test]
    async fn agent_delete_leaves_owned_state_intact_when_persist_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("config.toml");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let mut config = zeroclaw_config::schema::Config {
            config_path: cfg_dir,
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        // Real default-workspace dir for the agent so the archive step has
        // something to act on (and so a buggy pre-fix run would visibly move
        // it under `agents/_deleted/`).
        let agent = zeroclaw_config::schema::AliasedAgentConfig {
            risk_profile: "default".into(),
            ..Default::default()
        };
        config.agents.insert("victim".to_string(), agent);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        config.runtime_profiles.entry("default".into()).or_default();
        let old_ws = config.agent_workspace_dir("victim");
        std::fs::create_dir_all(&old_ws).unwrap();
        // Seed an owned-state row (a cron job) under `victim` — the delete probe.
        zeroclaw_runtime::cron::add_job(&config, "victim", "* * * * *", "echo hi")
            .expect("seed cron job");
        assert_eq!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "victim")
                .unwrap()
                .len(),
            1
        );

        let state = crate::api::test_state(config.clone());
        let resp = delete_agent_cascade(&state, config.clone(), "victim").await;

        // Persist failed -> error response, not a clean delete.
        assert!(
            !resp.status().is_success(),
            "a failed config persist must surface an error"
        );
        // Owned state did NOT move: the cron job stays under `victim`.
        assert_eq!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "victim")
                .unwrap()
                .len(),
            1,
            "cron must stay under `victim` when persist fails (no premature purge)"
        );
        // Workspace was NOT archived: still on disk at the original path.
        assert!(
            old_ws.exists(),
            "workspace must NOT have been archived when persist fails"
        );
        let archive_root = config.data_dir.join("agents").join("_deleted");
        assert!(
            !archive_root.exists(),
            "no archive directory must be created when persist fails"
        );
        // In-memory config was never swapped: still names `victim`.
        assert!(state.config.read().agents.contains_key("victim"));
    }

    #[tokio::test]
    async fn agent_delete_purges_owned_state_after_successful_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"), // writable -> persist OK
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let agent = zeroclaw_config::schema::AliasedAgentConfig {
            risk_profile: "default".into(),
            ..Default::default()
        };
        config.agents.insert("victim".to_string(), agent);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        config.runtime_profiles.entry("default".into()).or_default();
        let old_ws = config.agent_workspace_dir("victim");
        std::fs::create_dir_all(&old_ws).unwrap();
        zeroclaw_runtime::cron::add_job(&config, "victim", "* * * * *", "echo hi")
            .expect("seed cron job");

        let state = crate::api::test_state(config.clone());
        let resp = delete_agent_cascade(&state, config.clone(), "victim").await;
        assert!(resp.status().is_success(), "a clean delete returns success");

        // Config swapped: `victim` is GONE.
        assert!(
            !state.config.read().agents.contains_key("victim"),
            "agent removed from persisted config"
        );
        // Cron job purged: the cascade ran after a successful persist.
        assert!(
            zeroclaw_runtime::cron::list_jobs_by_agent(&config, "victim")
                .unwrap()
                .is_empty(),
            "cron purged once persist succeeds"
        );
        // Workspace archived: source dir gone, archive dir populated.
        assert!(
            !old_ws.exists(),
            "old workspace no longer at the original path"
        );
        let archive_root = config.data_dir.join("agents").join("_deleted");
        assert!(archive_root.exists(), "archive directory was created");
        let archived_ws = std::fs::read_dir(&archive_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.path().join("workspace").exists())
            .expect("an archive entry for `victim` with a workspace/ subdir");
        assert!(
            archived_ws
                .file_name()
                .to_string_lossy()
                .starts_with("victim-"),
            "archive entry name must start with `victim-`"
        );
    }

    #[tokio::test]
    async fn agent_delete_response_carries_partial_failure_warnings() {
        use axum::body::to_bytes;

        let tmp = tempfile::tempdir().unwrap();
        let mut config = zeroclaw_config::schema::Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let agent = zeroclaw_config::schema::AliasedAgentConfig {
            risk_profile: "default".into(),
            ..Default::default()
        };
        config.agents.insert("victim".to_string(), agent);
        config
            .risk_profiles
            .entry("default".into())
            .or_default()
            .allowed_commands = vec!["echo".into()];
        config.runtime_profiles.entry("default".into()).or_default();
        let agents_dir = config.data_dir.join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let deleted_marker = agents_dir.join("_deleted");
        std::fs::write(&deleted_marker, b"").expect("seed _deleted blocker file");
        let old_ws = config.agent_workspace_dir("victim");
        std::fs::create_dir_all(&old_ws).unwrap();
        // Drop a real file inside the workspace so the cascade has something
        // to archive (and so we can detect a successful archive).
        std::fs::write(old_ws.join("marker.txt"), b"hi").unwrap();
        zeroclaw_runtime::cron::add_job(&config, "victim", "* * * * *", "echo hi")
            .expect("seed cron job");

        let state = crate::api::test_state(config.clone());
        let resp = delete_agent_cascade(&state, config.clone(), "victim").await;

        // The HTTP call is still 200 OK — partial failure is not an error
        // response, it is a successful response with `warnings` populated.
        assert_eq!(resp.status(), axum::http::StatusCode::OK);

        // Parse the response body and assert the `warnings` field is present
        // and non-empty. We assert the SPECIFIC shape the operator sees:
        // an array of strings, one per failed side-effect.
        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let warnings = json
            .get("warnings")
            .and_then(|v| v.as_array())
            .expect("response must carry a `warnings` array");
        assert!(
            !warnings.is_empty(),
            "partial-failure response must surface at least one warning, got: {warnings:?}"
        );
        // At least one warning should mention the archive dir (creation or rename).
        let joined = warnings
            .iter()
            .map(|v| v.as_str().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("archive"),
            "warnings should mention archive-side failures, got: {joined}"
        );
    }

    fn config_with_telegram_alias(
        tmp: &tempfile::TempDir,
        alias: &str,
    ) -> zeroclaw_config::schema::Config {
        let mut config = temp_config(tmp);
        config.channels.telegram.insert(
            alias.to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                bot_token: "test-token".to_string(),
                api_base_url: zeroclaw_config::schema::TELEGRAM_OFFICIAL_API_BASE_URL.to_string(),
                ..Default::default()
            },
        );
        config
    }

    /// Trust-boundary regression: the bind route must reject an
    /// unauthenticated request before any config mutation. Pairing is
    /// required and no token is presented, so the handler returns 401 and
    /// leaves the peer group untouched.
    #[tokio::test]
    async fn channel_bind_rejects_unauthenticated_request() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config_with_telegram_alias(&tmp, "alerts");
        let mut state = test_state(config);
        state.pairing = Arc::new(PairingGuard::new(true, &[]));

        let (status, _json) = response_json(
            handle_api_channel_bind(
                axum::extract::State(state.clone()),
                axum::http::HeaderMap::new(),
                axum::Json(ChannelBindBody {
                    channel_type: "telegram".to_string(),
                    alias: "alerts".to_string(),
                    identity: "123456789".to_string(),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            state
                .config
                .read()
                .channel_external_peers("telegram", "alerts")
                .is_empty(),
            "a rejected bind must not mutate the peer group"
        );
    }

    /// Trust-boundary regression: binding into a `[channels.telegram.<alias>]`
    /// that does not exist must 404 rather than mint a peer group the runtime
    /// never resolves authorization from.
    #[tokio::test]
    async fn channel_bind_phantom_alias_is_404() {
        let tmp = tempfile::tempdir().unwrap();
        let config = config_with_telegram_alias(&tmp, "alerts");
        let state = test_state(config);

        let (status, _json) = response_json(
            handle_api_channel_bind(
                axum::extract::State(state.clone()),
                axum::http::HeaderMap::new(),
                axum::Json(ChannelBindBody {
                    channel_type: "telegram".to_string(),
                    alias: "ghost".to_string(),
                    identity: "123456789".to_string(),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            state
                .config
                .read()
                .channel_external_peers("telegram", "ghost")
                .is_empty(),
            "a phantom-alias bind must not create a peer group"
        );
    }

    #[tokio::test]
    async fn refresh_context_window_forwards_api_key() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer test-api-key-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{
                    "id": "llama-3.1-70b",
                    "context_length": 4096
                }]
            })))
            .expect(1)
            .mount(&mock)
            .await;

        let (_tmp, path) = temp_config_path();
        let mut cfg = zeroclaw_config::schema::Config {
            config_path: path.clone(),
            ..Default::default()
        };
        cfg.providers.models.groq.insert(
            "test".to_string(),
            zeroclaw_config::schema::GroqModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("llama-3.1-70b".into()),
                    api_key: Some("test-api-key-123".into()),
                    uri: Some(mock.uri()),
                    ..Default::default()
                },
            },
        );
        cfg.save().await.expect("initial save");

        let state = crate::api::test_state(cfg);

        let app = axum::Router::new()
            .route(
                "/api/config/model-providers/{type}/{alias}/refresh-context-window",
                axum::routing::post(handle_refresh_context_window),
            )
            .with_state(state);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/config/model-providers/groq/test/refresh-context-window")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            response.status().is_success(),
            "expected 200, got {}",
            response.status()
        );

        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body_str).unwrap();

        assert_eq!(json["path"], "providers.models.groq.test");
        assert_eq!(json["context_window"], 4096);
        assert!(
            !body_str.contains("test-api-key-123"),
            "API key leaked in response body"
        );

        let requests = mock.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "expected exactly one request to mock");
        assert_eq!(
            requests[0]
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer test-api-key-123"
        );
    }
}
