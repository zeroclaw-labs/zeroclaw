//! Agent-specific provider installation and conversation conversion.
//! Session decisions and durable control remain owned by the kernel.
use super::*;

pub(super) use zeroclaw_providers::factory::SessionModelProvider;

/// Retain the actual base route only while a turn temporarily replaces it.
/// Restoring on drop also covers a cancelled or dropped turn future.
pub(super) struct RestoreTurnModel<'a> {
    pub(super) agent: &'a mut Agent,
    pub(super) base: Option<SessionModelProvider>,
}

impl Drop for RestoreTurnModel<'_> {
    fn drop(&mut self) {
        if let Some(binding) = self.base.take() {
            self.agent.install_model_binding(binding);
        }
    }
}

impl Agent {
    /// Install a complete route and return the actual previous binding for
    /// callers that need turn-local restoration.
    fn install_model_binding(&mut self, binding: SessionModelProvider) -> SessionModelProvider {
        SessionModelProvider {
            provider: std::mem::replace(&mut self.model_provider, binding.provider),
            provider_ref: std::mem::replace(&mut self.model_provider_name, binding.provider_ref),
            model: std::mem::replace(&mut self.model_name, binding.model),
            route_resolver: std::mem::replace(
                &mut self.model_route_resolver,
                binding.route_resolver,
            ),
        }
    }

    pub(super) async fn apply_observed_model_switch(
        &mut self,
        current_model: &str,
        provider: String,
        model: String,
        turn_base: Option<&mut Option<SessionModelProvider>>,
    ) -> Result<Option<String>> {
        use zeroclaw_api::session_events::capture::{ModelChangeDisposition, ModelRouteMode};
        use zeroclaw_kernel::session::model_selection::ResolvedModelSelection;
        let prepared = self.prepare_model_switch(current_model, &provider, &model);
        ResolvedModelSelection {
            provider_ref: provider.clone(),
            model: model.clone(),
            route_mode: if self.automatic_model_routing {
                ModelRouteMode::Automatic
            } else {
                ModelRouteMode::Manual
            },
        }
        .observe(match &prepared {
            Ok(_) if turn_base.is_some() => ModelChangeDisposition::AppliedForTurn,
            Ok(_) => ModelChangeDisposition::Applied,
            Err(error) => ModelChangeDisposition::Rejected {
                reason_key: "model_switch_construction_failed".into(),
                diagnostic: Some(Self::session_execution_diagnostic(error)),
            },
        })
        .await?;
        match prepared {
            Ok(Some(binding)) => {
                let previous = self.install_model_binding(binding);
                if let Some(base) = turn_base {
                    base.get_or_insert(previous);
                }
                Ok(Some(model))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                ::zeroclaw_log::record!(ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error":Self::session_execution_diagnostic(&error)})),
                    "model_switch_construction_failed");
                Ok(None)
            }
        }
    }

    /// Restore a committed selection using the provider's canonical route
    /// configuration. Credential bytes are resolved here, never from history.
    pub fn restore_session_model_selection(
        &mut self,
        selection: zeroclaw_api::session_events::capture::ModelOverrideSelection,
    ) -> Result<()> {
        use zeroclaw_api::session_events::capture::ModelRouteMode;
        use zeroclaw_kernel::session::model_selection::ResolvedModelSelection;
        self.sync_config_generation();
        let ResolvedModelSelection {
            provider_ref: provider,
            model,
            route_mode,
        } = ResolvedModelSelection::resolve(selection, || {
            let config = self
                .provider_switch_config
                .as_ref()
                .and_then(|config| config.config.as_ref())
                .ok_or_else(|| anyhow::Error::msg("session_default_model_configuration_missing"))?;
            let (family, alias, entry) = config
                .resolved_model_provider_for_agent(&self.agent_alias)
                .ok_or_else(|| anyhow::Error::msg("session_default_model_profile_missing"))?;
            Ok((
                format!("{family}.{alias}"),
                entry
                    .model
                    .clone()
                    .ok_or_else(|| anyhow::Error::msg("session_default_model_missing"))?,
            ))
        })?;
        if let Some(binding) = self.prepare_model_switch(&self.model_name, &provider, &model)? {
            self.install_model_binding(binding);
        }
        self.automatic_model_routing = route_mode == ModelRouteMode::Automatic;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn try_apply_model_switch(
        &mut self,
        current_effective_model: &str,
        new_model_provider: String,
        new_model: String,
    ) -> Option<String> {
        let binding = self
            .prepare_model_switch(current_effective_model, &new_model_provider, &new_model)
            .ok()??;
        self.install_model_binding(binding);
        Some(new_model)
    }

    fn prepare_model_switch(
        &self,
        current_effective_model: &str,
        new_model_provider: &str,
        new_model: &str,
    ) -> Result<Option<SessionModelProvider>> {
        if new_model_provider == self.model_provider_name && new_model == current_effective_model {
            return Ok(None);
        }
        let config = self.provider_switch_config.as_ref()
            .and_then(|config| config.config.as_ref())
            .ok_or_else(|| anyhow::Error::msg(
                "model_switch requested but agent has no provider_switch_config; cannot rebuild provider safely"))?;
        zeroclaw_providers::factory::build_switched_model_provider(
            config,
            &self.agent_alias,
            new_model_provider,
            new_model,
        )
        .map(Some)
    }

    /// Manual native compaction uses the existing provider dispatch and lane.
    /// The kernel records the frozen source, actual attempt and ordered result.
    pub async fn compact_native_context(
        &mut self,
        native: Arc<dyn zeroclaw_api::session_events::capture::SessionExecutionPort>,
    ) -> Result<usize> {
        use zeroclaw_api::session_events::{ProviderPurpose, capture::ContextSummaryExecutor};
        let dispatcher = self.tool_dispatcher.as_ref();
        let provider = self.model_provider.as_ref();
        let model = self.model_name.as_str();
        let temperature = self.temperature;
        let summarize: ContextSummaryExecutor<'_> = Box::new(|conversation| {
            Box::pin(async move {
                let messages = dispatcher.to_provider_messages(&conversation);
                let response = zeroclaw_providers::dispatch::ProviderDispatch::from_ref(provider)
                    .chat(
                        zeroclaw_api::model_provider::ChatRequest {
                            messages: &messages,
                            tools: None,
                            thinking: None,
                        },
                        model,
                        temperature,
                    )
                    .await?;
                Ok(response.text_or_empty().to_owned())
            })
        });
        let installed = native
            .summarize_context(ProviderPurpose::Summary, summarize)
            .await?;
        let count = installed.messages.len();
        self.seed_conversation_history(installed.messages);
        self.set_history_has_trim_breadcrumb(installed.history_has_trim_breadcrumb.unwrap_or(true));
        Ok(count)
    }
}
