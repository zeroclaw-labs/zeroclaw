use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use daemonclaw_api::agent::TurnResult;
use daemonclaw_config::policy::SecurityPolicy;
use daemonclaw_config::schema::AuditConfig;

use crate::hooks::traits::{HookHandler, TurnCompleteAction};
use crate::security::audit::AuditLogger;
use crate::tasks::{
    AcceptanceVerifier, Autonomy, TaskActor, TaskOutcome, TaskStatus, WorkspaceVerifier,
    CURRENT_TASK_BINDING,
};

pub struct CloseGateHook {
    workspace_dir: PathBuf,
    security: Arc<SecurityPolicy>,
    verification_timeout_secs: u64,
    audit_config: AuditConfig,
}

impl CloseGateHook {
    pub fn new(
        workspace_dir: PathBuf,
        security: Arc<SecurityPolicy>,
        verification_timeout_secs: u64,
        audit_config: AuditConfig,
    ) -> Self {
        Self {
            workspace_dir,
            security,
            verification_timeout_secs,
            audit_config,
        }
    }

    fn runtime_actor() -> TaskActor {
        TaskActor {
            channel: "runtime".to_string(),
            id: Some("close_gate".to_string()),
        }
    }
}

#[async_trait]
impl HookHandler for CloseGateHook {
    fn name(&self) -> &str {
        "close_gate"
    }

    fn priority(&self) -> i32 {
        -100
    }

    async fn on_turn_complete(&self, _result: &TurnResult) -> TurnCompleteAction {
        let binding = match CURRENT_TASK_BINDING.try_with(|b| b.clone()) {
            Ok(Some(b)) => b,
            _ => return TurnCompleteAction::Continue,
        };

        let task = match crate::tasks::store::get_task(&self.workspace_dir, &binding.task_id) {
            Ok(t) => t,
            Err(_) => return TurnCompleteAction::Continue,
        };

        if task.status != TaskStatus::Review {
            return TurnCompleteAction::Continue;
        }

        if task.autonomy == Autonomy::Gated {
            return TurnCompleteAction::Continue;
        }

        let has_human_items = task.acceptance.iter().any(|i| i.kind == "human");
        let effective_band = if task.autonomy == Autonomy::Auto && has_human_items {
            Autonomy::Assisted
        } else {
            task.autonomy
        };

        let verifier = WorkspaceVerifier::new(
            self.workspace_dir.clone(),
            Arc::clone(&self.security),
            self.verification_timeout_secs,
        );

        let audit = match AuditLogger::new(self.audit_config.clone(), self.workspace_dir.clone()) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!("close_gate: failed to open audit logger: {e}");
                return TurnCompleteAction::Continue;
            }
        };

        let actor = Self::runtime_actor();
        let task_short = &binding.task_id[..8.min(binding.task_id.len())];

        match effective_band {
            Autonomy::Auto => {
                match crate::tasks::store::close_task(
                    &self.workspace_dir,
                    &binding.task_id,
                    &actor,
                    TaskOutcome::Succeeded,
                    &verifier,
                    false,
                    &audit,
                ) {
                    Ok(_) => {
                        tracing::info!(task_id = %binding.task_id, "close_gate: auto task closed");
                        TurnCompleteAction::Continue
                    }
                    Err(crate::tasks::TaskError::CloseRefused { reason }) => {
                        // Reopen so agent can resume work
                        let _ = crate::tasks::store::reopen_task(
                            &self.workspace_dir,
                            &binding.task_id,
                            &actor,
                            &reason,
                            &audit,
                        );
                        tracing::info!(task_id = %binding.task_id, %reason, "close_gate: reopened");
                        TurnCompleteAction::InjectError(format!(
                            "task {task_short} not closed: {reason}"
                        ))
                    }
                    Err(_) => TurnCompleteAction::Continue,
                }
            }

            Autonomy::Assisted => {
                // Run verification and record results, but don't close
                let mut updated_task =
                    match crate::tasks::store::get_task(&self.workspace_dir, &binding.task_id) {
                        Ok(t) => t,
                        Err(_) => return TurnCompleteAction::Continue,
                    };

                let mut all_satisfied = true;
                let mut unmet = Vec::new();
                for item in &mut updated_task.acceptance {
                    if item.kind != "human" {
                        item.satisfied = false;
                        match verifier.verify(&item.kind, &item.check) {
                            Ok(true) => item.satisfied = true,
                            Ok(false) => {
                                all_satisfied = false;
                                unmet.push(item.check.clone());
                            }
                            Err(reason) => {
                                all_satisfied = false;
                                unmet.push(format!("{}: {reason}", item.check));
                            }
                        }
                    } else if !item.satisfied {
                        all_satisfied = false;
                        unmet.push(format!("(human) {}", item.check));
                    }
                }

                // Persist updated satisfaction state
                if let Err(e) = crate::tasks::store::update_acceptance(
                    &self.workspace_dir,
                    &binding.task_id,
                    &updated_task.acceptance,
                ) {
                    tracing::warn!(task_id = %binding.task_id, "close_gate: failed to persist acceptance: {e}");
                }

                if all_satisfied {
                    tracing::info!(task_id = %binding.task_id, "close_gate: assisted verification passed, awaiting operator");
                    TurnCompleteAction::Continue
                } else {
                    let reason = format!("unsatisfied: {}", unmet.join(", "));
                    let _ = crate::tasks::store::reopen_task(
                        &self.workspace_dir,
                        &binding.task_id,
                        &actor,
                        &reason,
                        &audit,
                    );
                    tracing::info!(task_id = %binding.task_id, %reason, "close_gate: assisted reopened");
                    TurnCompleteAction::InjectError(format!(
                        "task {task_short} verification failed: {reason}"
                    ))
                }
            }

            Autonomy::Gated => TurnCompleteAction::Continue,
        }
    }
}
