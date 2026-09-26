//! Alias delete preview and cascade shared by the gateway's
//! `/api/config/delete-plan` and `DELETE /api/config/map-key` routes and the
//! RPC `config/delete-plan` and `config/map-key-delete` methods.
//!
//! A delete runs in two halves. [`prepare_alias_delete`] mutates a working
//! copy (refusing on hard references and, for agents, on live ACP sessions)
//! and reports the paths it touched; the caller authorizes and commits those
//! through its own path. After the commit, [`finish_agent_delete`] archives the
//! agent workspace and removes its owned non-config state.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_config::alias_refs::{self, AliasKind, CascadeError, CascadePolicy, RefSite};
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};
use zeroclaw_config::schema::Config;
use zeroclaw_infra::session_backend::SessionBackend;

use super::agent_owned_state::{cascade_owned_state, live_acp_session_count};

/// A single config reference site to an aliased entry, for the delete preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_acp_sessions: Option<usize>,
    /// Agent delete only: the agent's owned non-config state (memory / cron /
    /// session history) is exported and removed on delete. Counts are not
    /// enumerated in the preview.
    pub cascades_owned_state: bool,
}

fn to_dto(site: &RefSite) -> RefSiteDto {
    RefSiteDto {
        path: site.path.clone(),
        raw_value: site.raw_value.clone(),
    }
}

/// Refusal for alias kinds whose executable delete cascade does not exist yet.
pub fn unsupported_delete_cascade_message(kind: &AliasKind) -> Option<&'static str> {
    use zeroclaw_config::alias_refs::ProviderCategory;
    match kind {
        AliasKind::Provider {
            category: ProviderCategory::Tts | ProviderCategory::Transcription,
            ..
        } => Some("TTS/transcription provider delete-with-cascade is not yet implemented"),
        _ => None,
    }
}

/// Dry-run the delete cascade for `<path>.<key>`. Read-only; never mutates.
pub fn build_delete_plan(
    config: &Config,
    path: &str,
    key: &str,
) -> Result<DeletePlanResponse, ConfigApiError> {
    let Some(kind) = alias_refs::alias_kind_for_map_path(path) else {
        // Non-aliased section (e.g. `mcp.servers`): generic key removal with no
        // reference cascade — nothing to preview.
        return Ok(DeletePlanResponse {
            path: path.to_string(),
            key: key.to_string(),
            allowed: true,
            blockers: Vec::new(),
            scrubs: Vec::new(),
            live_acp_sessions: None,
            cascades_owned_state: false,
        });
    };
    if let Some(message) = unsupported_delete_cascade_message(&kind) {
        return Err(ConfigApiError::new(ConfigApiCode::OpNotSupported, message)
            .with_path(format!("{path}.{key}")));
    }
    let plan = alias_refs::plan_delete(config, &kind, key);
    let is_agent = matches!(kind, AliasKind::Agent);
    // For agents the live-ACP gate also blocks; it fails closed (an error
    // counting sessions ⇒ "not allowed"), matching the real delete.
    let live_acp = if is_agent {
        live_acp_session_count(config, key).ok()
    } else {
        None
    };
    let allowed = plan.allowed && (!is_agent || live_acp == Some(0));
    Ok(DeletePlanResponse {
        path: path.to_string(),
        key: key.to_string(),
        allowed,
        blockers: plan.blockers.iter().map(to_dto).collect(),
        scrubs: plan.scrubs.iter().map(to_dto).collect(),
        live_acp_sessions: live_acp,
        cascades_owned_state: is_agent,
    })
}

/// Map a config-cascade failure onto the shared config error vocabulary.
pub fn cascade_error(path: &str, key: &str, err: CascadeError) -> ConfigApiError {
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
    ConfigApiError::new(code, msg).with_path(format!("{path}.{key}"))
}

/// What [`prepare_alias_delete`] did to the working copy.
pub struct PreparedAliasDelete {
    /// Every config path the cascade touched, already marked dirty on the
    /// working copy. Callers that authorize per path check each one.
    pub dirty_paths: Vec<String>,
    /// Set for an agent delete: the workspace to archive after the commit.
    pub agent_workspace: Option<PathBuf>,
}

/// Remove the aliased entry `<path>.<key>` from `working`, scrubbing soft
/// references and refusing on hard ones. Agent deletes also refuse while the
/// agent has live ACP sessions, and fail closed when the session store cannot
/// be read. `kind` comes from [`alias_refs::alias_kind_for_map_path`].
pub fn prepare_alias_delete(
    working: &mut Config,
    kind: &AliasKind,
    path: &str,
    key: &str,
) -> Result<PreparedAliasDelete, ConfigApiError> {
    let is_agent = matches!(kind, AliasKind::Agent);
    let mut agent_workspace = None;
    if is_agent {
        let alias = key;
        if !working.agents.contains_key(alias) {
            return Err(ConfigApiError::new(
                ConfigApiCode::PathNotFound,
                format!("agents.{alias} is not configured"),
            )
            .with_path("agents"));
        }
        // Refuse on HARD: config blockers (e.g. enabled heartbeat.agent) OR live
        // ACP sessions (the operator must end those first). The ACP gate FAILS
        // CLOSED: if the session store can't be read we refuse rather than risk
        // orphaning live sessions.
        let plan = alias_refs::plan_delete(working, &AliasKind::Agent, alias);
        let live_acp = live_acp_session_count(working, alias).map_err(|e| {
            ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!(
                    "cannot delete agent `{alias}`: could not verify live ACP sessions ({e}); refusing to avoid orphaning active sessions"
                ),
            )
            .with_path(format!("agents.{alias}"))
        })?;
        if !plan.allowed || live_acp > 0 {
            let mut reasons: Vec<String> = plan
                .blockers
                .iter()
                .map(|b| format!("{} (hard config reference)", b.path))
                .collect();
            if live_acp > 0 {
                reasons.push(format!("{live_acp} live ACP session(s) — end them first"));
            }
            return Err(ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!("cannot delete agent `{alias}`: {}", reasons.join("; ")),
            )
            .with_path(format!("agents.{alias}")));
        }
        agent_workspace = Some(working.agent_workspace_dir(alias));
    }

    let report = alias_refs::delete_with_cascade(working, kind, key, CascadePolicy::RefuseOnHard)
        .map_err(|e| {
        if is_agent {
            ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!("agent config cascade failed: {e}"),
            )
            .with_path(format!("agents.{key}"))
        } else {
            cascade_error(path, key, e)
        }
    })?;
    let dirty_paths = report.dirty_paths();
    for dirty in &dirty_paths {
        working.mark_dirty(dirty);
    }
    Ok(PreparedAliasDelete {
        dirty_paths,
        agent_workspace,
    })
}

/// Post-commit half of an agent delete: archive the workspace under
/// `<data_dir>/agents/_deleted/<alias>-<ts>` and export-then-delete the agent's
/// memory, cron, ACP and session state. Run it after releasing the config
/// write lock; it can be slow. Returns every partial failure so the caller
/// surfaces them instead of reporting a clean delete.
pub async fn finish_agent_delete(
    committed: &Config,
    mem: &Arc<dyn Memory>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
    workspace: &std::path::Path,
) -> Vec<String> {
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
        if let Err(err) = tokio::fs::rename(workspace, &dest).await {
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

    let owned = cascade_owned_state(committed, mem, session_backend, alias, &archive_dir).await;
    // Combine per-side-effect failures (archive dir / workspace rename) with
    // the per-store failures surfaced by `cascade_owned_state`, so the operator
    // sees the FULL partial-failure picture in the response, not just the
    // server log.
    warnings.extend(owned.warnings.iter().cloned());
    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"agent": alias, "memory": owned.memory_purged, "cron": owned.cron_removed, "acp": owned.acp_removed, "sessions_cleared": owned.sessions_cleared, "archive": archive_dir.display().to_string(), "warnings": warnings.len()})), "agent deleted with owned-state cascade");
    warnings
}
