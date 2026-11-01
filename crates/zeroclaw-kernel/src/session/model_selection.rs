//! Provider-independent selection semantics. Committed selections are resolved
//! against live defaults; provider instances and credentials never enter here.
use super::engine::EngineSession;
use super::*;
use zeroclaw_api::session_events::capture::{
    ModelChangeDisposition, ModelOverrideSelection, ModelRouteMode, current_provider_admission,
    current_session_execution,
};
use zeroclaw_api::session_events::*;

/// A resolved request, not another persisted or cached selection.
pub struct ResolvedModelSelection {
    pub provider_ref: String,
    pub model: String,
    pub route_mode: ModelRouteMode,
}

impl ResolvedModelSelection {
    /// Resolve defaults lazily so an explicit selection needs no default profile.
    pub fn resolve(
        selection: ModelOverrideSelection,
        defaults: impl FnOnce() -> anyhow::Result<(String, String)>,
    ) -> anyhow::Result<Self> {
        let (provider_ref, model, route_mode) = match selection {
            ModelOverrideSelection::Set {
                provider_ref,
                model,
                route_mode,
                ..
            } => (provider_ref, model, route_mode),
            ModelOverrideSelection::Inherit { .. } | ModelOverrideSelection::Clear => {
                let (provider, model) = defaults()?;
                (provider, model, ModelRouteMode::Automatic)
            }
        };
        Ok(Self {
            provider_ref,
            model,
            route_mode,
        })
    }

    /// Preserve the committed thinking override when an execution selects a
    /// different route. The bound admission owns authorization and serialization.
    pub async fn observe(self, outcome: ModelChangeDisposition) -> anyhow::Result<()> {
        let thinking = match current_session_execution() {
            Some(session) => match session.restore_model_override().await? {
                Some(ModelOverrideSelection::Set { thinking, .. })
                | Some(ModelOverrideSelection::Inherit { thinking }) => thinking,
                _ => None,
            },
            None => None,
        };
        if let Some(admission) = current_provider_admission() {
            admission
                .observe_model_change(
                    ModelOverrideSelection::Set {
                        provider_ref: self.provider_ref,
                        model: self.model,
                        thinking,
                        route_mode: self.route_mode,
                    },
                    outcome,
                )
                .await?;
        }
        Ok(())
    }
}

impl EngineSession {
    pub(super) async fn control_model_change(
        &self,
        selection: ModelOverrideSelection,
        outcome: ModelChangeDisposition,
    ) -> anyhow::Result<()> {
        let (control, payloads) = self.control_input(serde_json::json!({"command":"model_override", "selection":selection,"source":"authenticated_session_command"}))?;
        self.owner
            .record_model_change(control, payloads, outcome, self.authority.clone())
            .await?;
        Ok(())
    }
}

impl SessionCoordinator {
    pub(super) async fn record_model_change(
        self: &Arc<Self>,
        control: SessionEvent,
        mut payloads: Vec<PayloadRecord>,
        outcome: ModelChangeDisposition,
        resolver: Arc<dyn PermissionResolver>,
    ) -> Result<(), SessionError> {
        let owner = self.clone();
        self.blocking(move || {
            let stream = control.stream_id.clone();
            let branch = control
                .branch_id
                .clone()
                .ok_or(SessionError::InvocationMismatch)?;
            let lock = owner.lock_for(&stream);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            resolver.authorize_context(&stream, &branch)?;
            if matches!(outcome, ModelChangeDisposition::AppliedForTurn) {
                control.turn_id.as_ref().ok_or_else(|| {
                    SessionError::InvalidAdmission("turn model selection requires bound turn".into())
                })?;
            }
            if matches!(outcome, ModelChangeDisposition::Applied | ModelChangeDisposition::AppliedForTurn)
                && let Some(turn) = control.turn_id.as_ref()
            {
                owner.require_known_turn(&stream, &branch, Some(turn))?;
            }
            let head = owner.store.head(&stream)?;
            let history = owner.stream_history(&stream)?;
            let lineage = lineage::frozen_lineage(&history, &stream, &branch, head.sequence)?;
            let previous = history.iter().rev().find(|event| {
                lineage::includes(&lineage, event)
                    && matches!(event.body.data, EventData::SessionModelOverrideChanged {
                        resolution: ModelResolution::Applied { .. }
                    })
            }).map(|event| event.event_id.clone());
            let resolution = match outcome {
                ModelChangeDisposition::Applied => ModelResolution::Applied {
                    previous_selection_event_id: previous,
                },
                ModelChangeDisposition::AppliedForTurn => ModelResolution::AppliedForTurn,
                ModelChangeDisposition::Rejected { reason_key, diagnostic } => {
                    let evidence=PayloadRecord::json("session.error-detail/v1", &serde_json::json!({
                        "error_key":reason_key,
                        "detail":diagnostic.as_deref().map(str::to_owned),
                        "detail_unavailable_reason":if diagnostic.is_none(){Some("host_diagnostic_unavailable")}else{None},
                    })).map_err(AppendError::from)?;
                    let reference=StoredPayloadRef::from(&evidence);
                    payloads.push(evidence);
                    ModelResolution::Rejected {reason_key, evidence_ref:Some(reference)}
                },
            };
            let mut changed = SessionEvent::typed(
                stream.clone(),
                EventData::SessionModelOverrideChanged { resolution },
                control.actor.clone(),
            )
            .map_err(AppendError::from)?;
            changed.branch_id = Some(branch.clone());
            changed.turn_id = control.turn_id.clone();
            changed.causation_id = Some(control.event_id.clone());
            let key = format!("model-change:{}", control.event_id);
            let mut events = vec![control, changed];
            if let Some(binding) = payloads
                .iter()
                .find(|payload| payload.codec == "session.execution-binding/v1")
                && let Some(revision) =
                    owner.authority_observation(&stream, &branch, &events[0].actor, binding)?
            {
                events.push(revision);
            }
            let request = AppendRequest {
                stream_id: stream.clone(),
                expected_head: head,
                idempotency_key: key,
                events,
                payloads,
                erasure_reference_commitment: None,
            };
            rules::validate_history_batch(&history, &request)?;
            owner.store.append_batch(request)?;
            let _ = owner.wakeups.send(stream);
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_selection_does_not_resolve_defaults() {
        let resolved = ResolvedModelSelection::resolve(
            ModelOverrideSelection::Set {
                provider_ref: "openai.selected".into(),
                model: "model".into(),
                thinking: None,
                route_mode: ModelRouteMode::Manual,
            },
            || anyhow::bail!("defaults must not be read"),
        )
        .unwrap();
        assert_eq!(resolved.provider_ref, "openai.selected");
        assert_eq!(resolved.model, "model");
        assert_eq!(resolved.route_mode, ModelRouteMode::Manual);
    }

    #[test]
    fn inherit_and_clear_resolve_current_defaults_and_propagate_errors() {
        for selection in [
            ModelOverrideSelection::Clear,
            ModelOverrideSelection::Inherit { thinking: None },
        ] {
            let resolved = ResolvedModelSelection::resolve(selection.clone(), || {
                Ok(("openai.current".into(), "current".into()))
            })
            .unwrap();
            assert_eq!(resolved.provider_ref, "openai.current");
            assert_eq!(resolved.model, "current");
            assert_eq!(resolved.route_mode, ModelRouteMode::Automatic);
            assert!(
                ResolvedModelSelection::resolve(selection, || anyhow::bail!("default_missing"))
                    .is_err()
            );
        }
    }
}
