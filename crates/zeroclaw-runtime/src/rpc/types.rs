//! Shared request/response types for the ZeroClaw RPC + gateway API surface.
//!
//! The wire-stable types live in `zeroclaw_rpc_proto::types` and are
//! re-exported here, so `use zeroclaw_runtime::rpc::types::*` resolves every
//! name a dispatcher or client needs. The types defined below are the ones
//! whose fields are runtime-owned (cron jobs, doctor diagnostics, skill
//! frontmatter, quickstart descriptors); they stay in the runtime until
//! those field types have a foundation-crate home.

use serde::{Deserialize, Serialize};

pub use zeroclaw_rpc_proto::types::*;

// ── Re-exports: runtime types that already derive Serialize + Deserialize ──

pub use crate::cron::{CronJob, CronJobPatch, CronRun, DeliveryConfig, Schedule};
pub use crate::doctor::{DiagResult, Severity as DoctorSeverity};
pub use crate::quickstart::{
    AppliedAgent, FieldDescriptor, FieldSection, QuickstartError, QuickstartStep, Surface,
};
pub use crate::skills::frontmatter::SkillFrontmatter;

// ── Derive helper ────────────────────────────────────────────────────
//
// Same shape as the proto crate's helper, minus the schema derive: the types
// below embed runtime-owned field types that carry no `JsonSchema`.

macro_rules! rpc_type {
    (
        $(#[$meta:meta])*
        pub struct $name:ident { $($body:tt)* }
    ) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        $(#[$meta])*
        pub struct $name { $($body)* }
    };
}

// ══════════════════════════════════════════════════════════════════════
// ── Core ─────────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct DoctorRunResult {
        pub results: Vec<DiagResult>,
        pub summary: DoctorSummary,
        /// Resolved active log persistence path, if available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub log_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timed_out_phase: Option<String>,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Sessions ─────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct SessionNewParams {
        pub agent_alias: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub session_id: Option<String>,
        /// Accepted for wire compatibility and ignored. The session's shell
        /// environment is resolved from the calling connection's own TUI
        /// registration, so naming another connection's id here has no
        /// effect.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tui_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub exclude_memory: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub chat_mode: Option<ChatMode>,
        /// Closed user-facing harness identifier. The daemon validates this
        /// value and resolves all descriptive claims from host-owned state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub interaction_surface: Option<crate::agent::prompt::InteractionSurface>,
        /// When true, skip the same-mode idle-sibling eviction normally
        /// performed on `session/new` for the calling TUI. Sent by
        /// multi-session-aware clients that manage sibling session lifecycle
        /// themselves. Absent or false preserves the eviction sweep.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub keep_siblings: Option<bool>,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Cron ─────────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct CronListResult {
        pub jobs: Vec<CronJob>,
    }
}

rpc_type! {
    /// Params for `cron/add`. Consolidates gateway `CronAddBody`.
    pub struct CronAddParams {
        pub agent: String,
        pub schedule: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tz: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub command: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub job_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub delivery: Option<DeliveryConfig>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub session_target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub allowed_tools: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub delete_after_run: Option<bool>,
    }
}

rpc_type! {
    pub struct CronRunsResult {
        pub runs: Vec<CronRun>,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Skills ───────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    /// Wire representation of a skill in a list. Consolidates gateway `SkillEntry`.
    pub struct SkillListEntry {
        pub bundle: String,
        pub name: String,
        pub directory: String,
        pub frontmatter: SkillFrontmatter,
    }
}

rpc_type! {
    pub struct SkillsListResult {
        pub skills: Vec<SkillListEntry>,
    }
}

rpc_type! {
    /// Consolidates gateway `SkillReadResponse`.
    pub struct SkillsReadResult {
        pub bundle: String,
        pub name: String,
        pub frontmatter: SkillFrontmatter,
        pub body: String,
    }
}

rpc_type! {
    pub struct SkillsWriteParams {
        pub bundle: String,
        pub name: String,
        pub frontmatter: SkillFrontmatter,
        #[serde(default)]
        pub body: String,
    }
}

// ══════════════════════════════════════════════════════════════════════
// ── Quickstart ───────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════

rpc_type! {
    pub struct QuickstartFieldsParams {
        pub section: FieldSection,
        pub type_key: String,
    }
}

rpc_type! {
    pub struct QuickstartFieldsResult {
        pub fields: Vec<FieldDescriptor>,
    }
}

/// Tagged enum — matches the HTTP route's `ValidateResult` shape so
/// the drift test can compare bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuickstartValidateResult {
    Ok,
    Errors { errors: Vec<QuickstartError> },
}

/// Tagged enum — matches the HTTP route's `ApplyResult` shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuickstartApplyResult {
    Applied {
        agent: AppliedAgent,
        /// `true` when the in-place daemon reload was signalled.
        /// `false` when no reload tx was attached (e.g. test harness)
        /// — caller must restart the daemon manually to pick up the
        /// change.
        daemon_restarted: bool,
    },
    Errors {
        errors: Vec<QuickstartError>,
    },
}

rpc_type! {
    pub struct QuickstartDismissParams {
        pub run_id: String,
        /// Surface that emitted the dismissal. Deserialised straight
        /// into the typed enum — no string-match at the boundary.
        pub surface: Surface,
        #[serde(default)]
        pub last_step: Option<QuickstartStep>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn interaction_surface_is_closed_and_snake_case() {
        use crate::agent::prompt::InteractionSurface;

        assert_eq!(
            serde_json::to_value(InteractionSurface::ZerocodeCode).unwrap(),
            json!("zerocode_code")
        );
        assert_eq!(
            serde_json::from_value::<InteractionSurface>(json!("zerocode_code")).unwrap(),
            InteractionSurface::ZerocodeCode
        );
        assert!(
            serde_json::from_value::<InteractionSurface>(json!("client_authored_claims")).is_err()
        );
    }

    #[test]
    fn session_new_params_keep_siblings_round_trips_and_defaults_absent() {
        // Older clients omit the field entirely: it must parse as None and
        // serialize back out without a `keep_siblings` key.
        let legacy: SessionNewParams =
            serde_json::from_value(json!({ "agent_alias": "a" })).unwrap();
        assert_eq!(legacy.keep_siblings, None);
        assert_eq!(legacy.interaction_surface, None);
        let wire = serde_json::to_value(&legacy).unwrap();
        assert!(wire.get("keep_siblings").is_none());

        for keep in [true, false] {
            let params: SessionNewParams = serde_json::from_value(json!({
                "agent_alias": "a",
                "keep_siblings": keep,
            }))
            .unwrap();
            assert_eq!(params.keep_siblings, Some(keep));
            let wire = serde_json::to_value(&params).unwrap();
            assert_eq!(wire["keep_siblings"], json!(keep));
        }
    }

    #[test]
    fn quickstart_validate_result_ok_variant_uses_kind_tag() {
        let v = serde_json::to_value(QuickstartValidateResult::Ok).unwrap();
        assert_eq!(v, json!({"kind": "ok"}));
    }

    #[test]
    fn quickstart_validate_result_errors_variant_carries_payload() {
        // Just smoke-test the field structure — `QuickstartError` is owned
        // by `quickstart` and has its own coverage there.
        let v =
            serde_json::to_value(QuickstartValidateResult::Errors { errors: Vec::new() }).unwrap();
        assert_eq!(v["kind"], json!("errors"));
        assert!(v["errors"].is_array(), "got: {v}");
    }

    #[test]
    fn quickstart_apply_result_applied_variant_carries_daemon_flag() {
        // `daemon_restarted: false` is the test-harness contract — the web
        // surface reads this to decide whether to tell the user to restart
        // manually. Lock it. The variant is tagged (`"kind": "applied"`),
        // and the agent payload is snake_case.
        let v = serde_json::to_value(QuickstartApplyResult::Applied {
            agent: AppliedAgent {
                alias: "primary".into(),
                model_provider: "anthropic.claude".into(),
                risk_profile: "standard".into(),
                runtime_profile: "default".into(),
                channels: vec!["telegram.main".into()],
                memory_backend: "sqlite".into(),
            },
            daemon_restarted: false,
        })
        .unwrap();
        assert_eq!(v["kind"], json!("applied"));
        assert_eq!(v["daemon_restarted"], json!(false));
        assert_eq!(v["agent"]["alias"], json!("primary"));
    }

    #[test]
    fn quickstart_dismiss_params_deserializes_with_optional_last_step() {
        // `last_step` is `#[serde(default)] Option<QuickstartStep>` — older
        // dismiss payloads omit it. Must default to `None` without error.
        let params: QuickstartDismissParams = serde_json::from_value(json!({
            "run_id": "r1",
            "surface": "tui"
        }))
        .unwrap();
        assert_eq!(params.run_id, "r1");
        assert_eq!(params.surface, Surface::Tui);
        assert!(params.last_step.is_none());
    }
}
