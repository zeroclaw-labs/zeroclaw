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
//! for the full surface and acceptance checklist.

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
    /// Non-fatal validation warnings against the post-save config state.
    /// Empty when nothing is flagged. Surfaces what the CLI prints on
    /// stderr so dashboard callers see the same signal — e.g. an
    /// `agents.<x>.model_provider` referencing an unconfigured model_provider
    /// returns HTTP 200 with the save committed, plus a structured
    /// validation warning here.
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
                }
            })
        })
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
/// Run `validate()` and partition errors: if the failure path overlaps
/// a dirty path on the working config, the save is rejected
/// (`Err(Response)`); otherwise the error is downgraded to a
/// non-fatal warning attached to the response. Saving a single field
/// shouldn't be blocked by an unrelated pre-existing validation
/// problem elsewhere in the config.
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

/// Fields the gateway owns end-to-end (mints, rotates, persists itself).
/// They're skipped by [`compute_drift`] so the dashboard doesn't surface a
/// banner the operator can't act on. Add new entries here when a similar
/// gateway-managed field lands (e.g. webhook secret rotation).
fn is_gateway_managed_field(name: &str) -> bool {
    // Match the prop-field name actually emitted by the `Configurable` derive,
    // which preserves the Rust field's snake_case (`paired_tokens`), not kebab.
    matches!(name, "gateway.paired_tokens")
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
    let mut all_names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    all_names.extend(in_memory_props.keys().map(String::as_str));
    all_names.extend(on_disk_props.keys().map(String::as_str));
    for name in all_names {
        // Gateway-managed internal state isn't operator-edited and the
        // gateway persists it itself via `persist_pairing_tokens` /
        // similar paths. Surfacing it as drift confuses operators who
        // can't fix it from the dashboard and the banner sticks until
        // the daemon happens to rewrite the file.
        if is_gateway_managed_field(name) {
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

/// GET /api/config/prop?path=agents.researcher.model_provider
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

    let mut new_config = state.config.read().clone();
    new_config.ensure_map_key_for_path(&body.path);
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

/// GET /api/config/list?prefix=model_providers
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
    /// `true` when one or more config writes have landed since the last
    /// `/admin/reload`. Distinct from disk-vs-memory drift: this fires on
    /// in-process PATCHes even though `persist_and_swap` updates the
    /// in-memory config, because some subsystems (channels, providers,
    /// scheduler) need to be re-instantiated to actually apply the change.
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
/// from a map-keyed section. Persists on success.
pub async fn handle_delete_map_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MapKeyQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let working = state.config.read().clone();
    // Agent deletion is special: it must scrub config references (heartbeat,
    // peer-groups, delegates, workspace.access, …) via `delete_with_cascade`
    // and cascade owned non-config state (memory / cron / acp / session). The
    // generic map-key delete below handles every other section unchanged.
    if q.path == "agents" {
        return delete_agent_cascade(&state, working, &q.key).await;
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

    // Resolve the workspace dir BEFORE the config cascade removes the agents
    // entry: `agent_workspace_dir` only returns an operator-set custom
    // `workspace.path` while the entry exists; after removal it silently falls
    // back to the default `install_root/agents/<alias>/workspace`, so a
    // custom-workspace agent's real dir would otherwise never be archived.
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

    // Archive into the shared graveyard `<data_dir>/agents/_deleted/<alias>-<ts>/`
    // (not inside the deleted agent's own dir), and give the owned-state exports
    // a home there even if the agent had no workspace dir. (`workspace` was
    // resolved above, before the cascade removed the entry.)
    let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let archive_dir = working
        .data_dir
        .join("agents")
        .join("_deleted")
        .join(format!("{alias}-{ts}"));
    if let Err(err) = tokio::fs::create_dir_all(&archive_dir).await {
        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": alias, "archive": archive_dir.display().to_string(), "err": err.to_string()})), "agent delete: archive dir creation failed");
    }
    if workspace.exists() {
        let dest = archive_dir.join("workspace");
        if let Err(err) = tokio::fs::rename(&workspace, &dest).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"agent": alias, "err": err.to_string()})),
                "agent delete: workspace archive failed"
            );
        }
    }

    // Owned-state cascade (export-then-delete memory/cron/acp + clear sessions).
    let owned = crate::agent_owned_state::cascade_owned_state(
        &working,
        &state.mem,
        state.session_backend.as_ref(),
        alias,
        &archive_dir,
    )
    .await;
    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"agent": alias, "memory": owned.memory_purged, "cron": owned.cron_removed, "acp": owned.acp_removed, "sessions_cleared": owned.sessions_cleared, "archive": archive_dir.display().to_string()})), "agent deleted with owned-state cascade");

    // Persist: mark EVERY entry the cascade touched dirty — the removed agent
    // entry AND each other entry whose soft-ref was scrubbed. `save_dirty` writes
    // only marked paths, so marking just `agents.<alias>` would leave a scrubbed
    // referrer (another agent's `delegates`, a peer group's `agents`) correct in
    // memory but STALE on disk, reappearing as a dangling reference on the next
    // config reload (which `validate()` then rejects). Mirrors rename's
    // `RenameReport.dirty_paths`.
    for path in cascade.dirty_paths() {
        working.mark_dirty(&path);
    }
    if let Err(e) = persist_and_swap(state, working).await {
        return error_response(e);
    }
    axum::Json(MapKeyResponse {
        path: "agents".to_string(),
        key: alias.to_string(),
        created: false,
    })
    .into_response()
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

    let mut working = state.config.read().clone();
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

    axum::Json(MapKeyResponse { path, key, created }).into_response()
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
    let Some(kind) = parse_alias_kind(&q.path) else {
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
    /// Owned-state cascade warnings (agent rename only): a non-empty list means
    /// the config rename succeeded but one or more owned stores (memory / cron /
    /// acp / session) did **not** follow the rename, so they need operator
    /// attention. Omitted from the JSON when empty (back-compat for the generic
    /// and provider/channel rename paths, which have no owned state).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Parse a rename `path` (the map-keyed *section*) into the typed
/// [`AliasKind`](zeroclaw_config::alias_refs::AliasKind) whose rename needs the
/// reference-rewrite cascade. Returns `None` for non-aliased sections (e.g.
/// `mcp.servers`), which fall back to the generic key-swap rename.
fn parse_alias_kind(path: &str) -> Option<zeroclaw_config::alias_refs::AliasKind> {
    use zeroclaw_config::alias_refs::{AliasKind, ProviderCategory};
    if path == "agents" {
        return Some(AliasKind::Agent);
    }
    if let Some(rest) = path.strip_prefix("providers.") {
        let (cat, family) = rest.split_once('.')?;
        if family.is_empty() || family.contains('.') {
            return None;
        }
        let category = match cat {
            "models" => ProviderCategory::Models,
            "tts" => ProviderCategory::Tts,
            "transcription" => ProviderCategory::Transcription,
            _ => return None,
        };
        return Some(AliasKind::Provider {
            category,
            family: family.to_string(),
        });
    }
    if let Some(ty) = path.strip_prefix("channels.") {
        if ty.is_empty() || ty.contains('.') {
            return None;
        }
        return Some(AliasKind::Channel {
            channel_type: ty.to_string(),
        });
    }
    None
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

/// `POST /api/config/rename-map-key` — rename an alias within a map-keyed
/// section, preserving the entry's value. Atomic: persists only on success.
///
/// Aliased sections (agents / providers / channels) route through
/// `rename_with_cascade`, which rewrites every config reference to follow the new
/// name (the generic key-swap alone would leave them dangling). Agent rename also
/// re-points owned state (memory / cron / acp / session rows + the workspace
/// dir). A missing source alias returns **404** for these. Non-aliased sections
/// keep the generic key-swap behaviour.
pub async fn handle_rename_map_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<RenameMapKeyBody>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let working = state.config.read().clone();

    match parse_alias_kind(&body.path) {
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

/// Agent rename cascade: rewrite config refs (`rename_with_cascade`), move the
/// workspace dir, re-point owned DB state (memory/cron/acp/session), mark the
/// touched paths dirty, persist. Mirrors `delete_agent_cascade` but in-place —
/// no archive, no live-session refusal (a live ACP session follows the rename).
/// Move the agent workspace dir for a rename. Returns `Some(warning)` when a
/// move was attempted and FAILED — surfaced to the caller so a config/DB rename
/// to `to` with the workspace stranded at `from` isn't reported as a clean
/// success. Returns `None` on success or when there is nothing to move (a custom
/// alias-independent path, or a source dir that doesn't exist).
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

    let report = match alias_refs::rename_with_cascade(&mut working, &AliasKind::Agent, from, to) {
        Ok(r) => r,
        Err(e) => return rename_error_response(&body.path, from, e),
    };
    for path in &report.dirty_paths {
        working.mark_dirty(path);
    }

    // Move the workspace dir. For the default per-alias location this is
    // `<install>/agents/<from>/workspace` → `…/<to>/workspace`. A custom
    // workspace path is alias-independent, so `old_ws == new_ws` and we skip.
    // A failed move is surfaced (like the owned-DB failures below), not just
    // logged — otherwise config+DB point at `to` while the workspace is stranded
    // at `from` and the caller sees a clean success.
    let new_ws = working.agent_workspace_dir(to);
    let move_warning = move_renamed_workspace(&old_ws, &new_ws).await;
    let workspace_moved = old_ws != new_ws && old_ws.exists() && move_warning.is_none();
    let mut warnings: Vec<String> = Vec::new();
    warnings.extend(move_warning);

    // Re-point owned DB state (memory/cron/acp/session). Best-effort + reported.
    let owned = crate::agent_owned_state::cascade_rename_agent(
        &working,
        &state.mem,
        state.session_backend.as_ref(),
        from,
        to,
    )
    .await;
    // Combine the workspace-move warning (if any) with the owned-store warnings
    // so every partial failure reaches the caller, not just the server log.
    warnings.extend(owned.warnings);
    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"from": from, "to": to, "memory": owned.memory_rows, "cron": owned.cron_jobs, "acp": owned.acp_sessions, "sessions": owned.sessions_repointed, "workspace_moved": workspace_moved, "dirty_paths": report.dirty_paths.len(), "warnings": warnings})), "agent renamed with owned-state cascade");

    if let Err(e) = persist_and_swap(state, working).await {
        return error_response(e);
    }
    // Config rename complete and persisted. `warnings` is non-empty only if a
    // partial step (the workspace move, or an owned store) did NOT follow —
    // surface it to the caller (not just the server log) so the split can be
    // remediated, rather than reporting a clean success.
    axum::Json(RenameMapKeyResponse {
        path: body.path.clone(),
        from: from.clone(),
        to: to.clone(),
        renamed: true,
        warnings,
    })
    .into_response()
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
        if matches!(op.op.as_str(), "add" | "replace") {
            working.ensure_map_key_for_path(&path);
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

/// Convert a JSON Pointer (`/agents/researcher/model_provider`) to the
/// dotted path the `Config::set_prop` machinery expects
/// (`agents.researcher.model_provider`). Accepts both forms — passing
/// already-dotted paths through unchanged so dashboard clients can use
/// whichever is more natural.
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

/// POST /api/config/migrate — apply the schema migration chain to the
/// on-disk config file in place. Mirrors `zeroclaw config migrate`. Backs
/// up the previous content alongside the original (`config.toml.bak`)
/// before writing the migrated form. Returns `{migrated: false}` when the
/// config is already at the current schema version.
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
            // Atomic write path mirrors `Config::save()` and `migration::migrate_file_in_place`
            //: write temp + fsync → backup → atomic rename → fsync directory.
            // Without this sequence the documented durability guarantee on the comment above
            // doesn't hold: a copy-then-write window leaves both the original and the new
            // content vulnerable to power loss.
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

/// OPTIONS /api/config/prop?path=agents.researcher.model_provider — per-field schema fragment.
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

    // dirty_entry_for / CascadeReport::dirty_paths tests live in
    // zeroclaw_config::alias_refs — single source of truth (the gateway and CLI
    // both consume the promoted helper).

    #[test]
    fn delete_cascade_resolves_custom_workspace_before_removing_entry() {
        // Regression: `delete_agent_cascade` must resolve `agent_workspace_dir`
        // BEFORE `delete_with_cascade` removes the agents entry. The method only
        // returns an operator-set custom `workspace.path` while the entry exists;
        // resolving it after removal silently yields the DEFAULT path, so a
        // custom-workspace agent's real dir would never be archived.
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

    // ── `test` op type-coercion invariants ─────────────────────────────
    //
    // The `test` JSON Patch op compares the incoming `value` against the
    // current property value. `Config::get_prop` always returns a display
    // string, regardless of the underlying field's PropKind. Before the
    // fix, the handler wrapped that string in `Value::String(...)` and
    // compared against the raw incoming `Value::Bool(true)` /
    // `Value::Number(42)` / etc. — never equal even when the test should
    // pass. The fix normalizes both sides to display strings via
    // `json_to_setprop_string` (the same helper `add`/`replace` use).
    //
    // These tests pin the invariant: for every PropKind that surfaces on
    // the API, `json_to_setprop_string(<typed JSON>, Some(kind))` equals
    // the string `Config::get_prop` returns.
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
        // `gateway.host` is a String, but [scheduler] / autonomy carry floats
        // for things like temperatures. Pick a path that's a float field on
        // the default config. If the schema gains/loses a float field this
        // test will need updating; that's fine — we just need one float to
        // pin the contract.
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

    /// Guardrail against the original #7156 bug class: a new `#[secret]` field
    /// added under `[gateway]` that the gateway also mints/rotates itself will
    /// reproduce the permanent-banner symptom unless it is explicitly listed
    /// in `is_gateway_managed_field` (or whitelisted below as operator-edited).
    /// This test fails when such a field lands without a corresponding matcher
    /// entry, forcing the author to make a deliberate decision instead of
    /// silently re-introducing the bug.
    #[test]
    fn every_gateway_secret_is_classified() {
        // Secrets under `[gateway]` that are OPERATOR-EDITED (not gateway-
        // managed). Add the field's prop-field name here only if the gateway
        // does NOT mint/rotate/persist it itself, so legitimate drift between
        // disk and memory IS surfaceable. Empty for now — `paired_tokens` is
        // the only `[gateway]` secret and it's gateway-managed.
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
}
