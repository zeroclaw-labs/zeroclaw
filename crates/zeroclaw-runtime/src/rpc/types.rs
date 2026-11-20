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

/// Project the controller reply without exposing any of its runtime types.
pub(crate) fn goal_response_wire(
    response: &zeroclaw_goal::control::GoalResponse,
) -> GoalResponseWire {
    use GoalResponseWire as W;
    use zeroclaw_goal::control::{GoalPendingAction, GoalQueuedControl, GoalResponse as R};

    match response {
        R::Help => W::Help,
        R::Disabled => W::Disabled,
        R::NoCurrentGoal => W::NoCurrentGoal,
        R::SessionUnavailable => W::SessionUnavailable,
        R::AlreadyActive => W::AlreadyActive,
        R::BudgetRequired => W::BudgetRequired,
        R::Stale => W::Stale,
        R::Queued(control) => W::Queued(match control {
            GoalQueuedControl::Start => GoalQueuedControlWire::Start,
            GoalQueuedControl::Resume => GoalQueuedControlWire::Resume,
            GoalQueuedControl::Pause => GoalQueuedControlWire::Pause,
            GoalQueuedControl::Budget => GoalQueuedControlWire::Budget,
        }),
        R::TransitionPending(action) => W::TransitionPending(match action {
            GoalPendingAction::Pause => GoalPendingActionWire::Pause,
            GoalPendingAction::Cancel => GoalPendingActionWire::Cancel,
        }),
        R::Started(status) => W::Started(goal_status_wire(status)),
        R::Status(status) => W::Status(goal_status_wire(status)),
        R::Budget(status) => W::Budget(goal_status_wire(status)),
        R::BudgetUpdated(status) => W::BudgetUpdated(goal_status_wire(status)),
        R::Paused(status) => W::Paused(goal_status_wire(status)),
        R::AlreadyPaused(status) => W::AlreadyPaused(goal_status_wire(status)),
        R::Resumed(status) => W::Resumed(goal_status_wire(status)),
        R::Cancelled(status) => W::Cancelled(goal_status_wire(status)),
        R::AlreadyCancelled(status) => W::AlreadyCancelled(goal_status_wire(status)),
        R::Terminal(status) => W::Terminal(goal_status_wire(status)),
    }
}

fn goal_status_wire(status: &zeroclaw_goal::control::GoalStatusProjection) -> GoalStatusWire {
    use zeroclaw_api::task::TaskStatus;
    use zeroclaw_goal::task::{GoalAccountingState, GoalPauseReason};

    GoalStatusWire {
        task_id: status.task_id.clone(),
        status: match status.status {
            TaskStatus::Running => GoalTaskStatusWire::Running,
            TaskStatus::Paused => GoalTaskStatusWire::Paused,
            TaskStatus::Completed => GoalTaskStatusWire::Completed,
            TaskStatus::Failed => GoalTaskStatusWire::Failed,
            TaskStatus::Cancelled => GoalTaskStatusWire::Cancelled,
            TaskStatus::Lost => GoalTaskStatusWire::Lost,
            TaskStatus::TimedOut => GoalTaskStatusWire::TimedOut,
        },
        execution_epoch: status.execution_epoch,
        token_limit: status.token_limit,
        cost_limit_usd: status.cost_limit_usd,
        accounting_state: match status.accounting_state {
            GoalAccountingState::Complete => GoalAccountingStateWire::Complete,
            GoalAccountingState::Missing => GoalAccountingStateWire::Missing,
            GoalAccountingState::Invalid => GoalAccountingStateWire::Invalid,
            GoalAccountingState::OutcomeUnknown => GoalAccountingStateWire::OutcomeUnknown,
        },
        pause_reason: status.pause_reason.map(|reason| match reason {
            GoalPauseReason::OperatorPaused => GoalPauseReasonWire::OperatorPaused,
            GoalPauseReason::NeedsUserInput => GoalPauseReasonWire::NeedsUserInput,
            GoalPauseReason::HumanEscalation => GoalPauseReasonWire::HumanEscalation,
            GoalPauseReason::ExternalDependency => GoalPauseReasonWire::ExternalDependency,
            GoalPauseReason::ProviderUnavailable => GoalPauseReasonWire::ProviderUnavailable,
            GoalPauseReason::VerifierBlocked => GoalPauseReasonWire::VerifierBlocked,
            GoalPauseReason::BudgetExhausted => GoalPauseReasonWire::BudgetExhausted,
            GoalPauseReason::BudgetUnavailable => GoalPauseReasonWire::BudgetUnavailable,
            GoalPauseReason::DaemonRestart => GoalPauseReasonWire::DaemonRestart,
        }),
        resumable: status.resumable,
    }
}

pub(crate) fn goal_failure_reason_wire(
    reason: zeroclaw_goal::control::GoalFailureReason,
) -> GoalFailureReasonWire {
    use GoalFailureReasonWire as W;
    use zeroclaw_goal::control::GoalFailureReason as R;
    match reason {
        R::AgentOperationFailed => W::AgentOperationFailed,
        R::ToolResultsUnpaired => W::ToolResultsUnpaired,
        R::EmptyAgentResponse => W::EmptyAgentResponse,
        R::VerifierOperationFailed => W::VerifierOperationFailed,
        R::VerifierResponseInvalid => W::VerifierResponseInvalid,
        R::AccountingIncomplete => W::AccountingIncomplete,
        R::PricingIncomplete => W::PricingIncomplete,
        R::OperationOutcomeUnknown => W::OperationOutcomeUnknown,
        R::ExecutorStartFailed => W::ExecutorStartFailed,
        R::ExecutorFailed => W::ExecutorFailed,
        R::CandidatePersistenceFailed => W::CandidatePersistenceFailed,
        R::Other => W::Other,
    }
}

// ── Re-exports: runtime types that already derive Serialize + Deserialize ──

pub use crate::cron::{CronJob, CronJobPatch, CronRun, DeliveryConfig, Schedule};
pub use crate::doctor::{DiagResult, Severity as DoctorSeverity};
pub use crate::quickstart::{
    AppliedAgent, FieldDescriptor, FieldSection, QuickstartError, QuickstartStep, Surface,
};
pub use crate::skills::frontmatter::SkillFrontmatter;

/// Project a provider safeguard-fallback notice onto its wire form. The
/// conversion stays in the runtime so the protocol crate does not depend on
/// providers. The classifier category never crosses the wire.
pub(crate) fn safeguard_fallback_wire(
    notice: &zeroclaw_providers::SafeguardFallbackNotice,
) -> SafeguardFallbackWire {
    use zeroclaw_providers::SafeguardFallbackKind as K;
    SafeguardFallbackWire {
        fallback_kind: match notice.kind {
            K::ServerSide => SafeguardFallbackKindWire::Server,
            K::ClientSide => SafeguardFallbackKindWire::Client,
            K::ClientAndServer => SafeguardFallbackKindWire::ClientServer,
        },
        requested_model: notice.requested_model.clone(),
        served_model: notice.served_model.clone(),
    }
}

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
    fn session_goal_failure_serializes_stable_client_payload() {
        let update = SessionGoalUpdate::Failed {
            session_id: "sess-1".into(),
            reason: goal_failure_reason_wire(
                zeroclaw_goal::control::GoalFailureReason::ToolResultsUnpaired,
            ),
        };

        assert_eq!(
            serde_json::to_value(update).unwrap(),
            json!({
                "failed": {
                    "session_id": "sess-1",
                    "reason": "tool_results_unpaired"
                }
            })
        );
    }

    #[test]
    fn session_goal_current_variants_match_the_native_client_fixture() {
        use zeroclaw_goal::control::{GoalFailureReason, GoalResponse};

        // The RPC-only client consumes this same fixture without linking the
        // daemon. Exercise both independent serde owners against identical bytes.
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/goal_wire_variants.json"
        )))
        .unwrap();
        for value in fixture["failure_reasons"].as_array().unwrap() {
            let reason: GoalFailureReason = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(reason).unwrap(), *value);
            let update = SessionGoalUpdate::Failed {
                session_id: "goal-session".into(),
                reason: goal_failure_reason_wire(reason),
            };
            assert_eq!(
                serde_json::to_value(update).unwrap()["failed"]["reason"],
                *value
            );
        }
        let mut values = Vec::new();
        for kind in fixture["unit_responses"].as_array().unwrap() {
            values.push(json!({"kind": kind}));
        }
        for kind in fixture["status_responses"].as_array().unwrap() {
            values.push(json!({"kind": kind, "status": fixture["status"]}));
        }
        for control in fixture["queued_controls"].as_array().unwrap() {
            values.push(json!({"kind": "queued", "status": control}));
        }
        for action in fixture["pending_actions"].as_array().unwrap() {
            values.push(json!({"kind": "transition_pending", "status": action}));
        }
        for value in values {
            let response: GoalResponse = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&response).unwrap(), value);
            let response = goal_response_wire(&response);
            let result = SessionGoalResult {
                response: response.clone(),
            };
            let update = SessionGoalUpdate::Lifecycle {
                session_id: "goal-session".into(),
                response,
            };
            assert_eq!(
                serde_json::to_value(update).unwrap()["lifecycle"]["response"],
                value
            );
            assert_eq!(serde_json::to_value(result).unwrap()["response"], value);
        }
    }

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
    fn safeguard_fallback_wire_maps_every_kind_and_drops_the_category() {
        use zeroclaw_providers::{SafeguardFallbackKind as K, SafeguardFallbackNotice};

        for (kind, wire) in [
            (K::ServerSide, "server"),
            (K::ClientSide, "client"),
            (K::ClientAndServer, "client_server"),
        ] {
            let notice = SafeguardFallbackNotice {
                kind,
                requested_model: "big".into(),
                served_model: "small".into(),
                category: Some("cyber".into()),
            };
            let v = serde_json::to_value(safeguard_fallback_wire(&notice)).unwrap();
            assert_eq!(
                v,
                json!({"fallback_kind": wire, "requested_model": "big", "served_model": "small"})
            );
        }
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
