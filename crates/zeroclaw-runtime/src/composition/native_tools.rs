//! Private native compatibility construction. Generic sources never enter it.
//! Existing lifecycle handles remain owned by their current runtime callers.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::ToolRequest;
use crate::live_config_authority::AgentExecutionCapability;
use crate::sop::{SopAuditLogger, SopEngine};
use crate::tools::{AcpSessionReadView, AllToolsResult, CanvasStore, ForwardedEnvironment};
use zeroclaw_config::schema::Config;

/// Borrowed/owned inputs the native factory already consumed before R1.
/// This is private: no SOP, canvas, ACP or execution internals enter ToolRequest.
pub(crate) struct RegistryContext<'a> {
    pub(crate) workspace_dir: &'a Path,
    pub(crate) canvas_store: Option<CanvasStore>,
    pub(crate) is_subagent: bool,
    pub(crate) tui_env: Option<ForwardedEnvironment>,
    pub(crate) sop_engine: Option<Arc<Mutex<SopEngine>>>,
    pub(crate) sop_audit: Option<Arc<SopAuditLogger>>,
    pub(crate) live_config: Option<Arc<parking_lot::RwLock<Config>>>,
    pub(crate) execution_capability: Option<AgentExecutionCapability>,
    pub(crate) acp_sessions: Option<AcpSessionReadView>,
}

impl<'a> RegistryContext<'a> {
    pub(crate) fn new(workspace_dir: &'a Path) -> Self {
        Self {
            workspace_dir,
            canvas_store: None,
            is_subagent: false,
            tui_env: None,
            sop_engine: None,
            sop_audit: None,
            live_config: None,
            execution_capability: None,
            acp_sessions: None,
        }
    }
}

/// Invoke the existing canonical native factory with the same resolved inputs.
pub(super) fn build(
    request: &ToolRequest<'_>,
    context: RegistryContext<'_>,
) -> anyhow::Result<AllToolsResult> {
    let config = request.config;
    let risk_profile = config
        .risk_profile_for_agent(request.agent_alias)
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"agent": request.agent_alias})),
                "composition_native_risk_profile_missing"
            );
            anyhow::Error::msg("agent native registry has no configured risk profile")
        })?;
    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };
    let fallback_api_key = config
        .resolved_model_provider_for_agent(request.agent_alias)
        .and_then(|(_, _, provider)| provider.api_key.as_deref());
    crate::tools::all_tools_with_runtime_context(
        Arc::clone(config),
        request.security,
        risk_profile,
        request.agent_alias,
        Arc::clone(request.runtime),
        Arc::clone(request.memory),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        context.workspace_dir,
        &config.agents,
        fallback_api_key,
        config,
        context.canvas_store,
        context.is_subagent,
        context.tui_env,
        context.sop_engine,
        context.sop_audit,
        context.live_config,
        context.execution_capability,
        context.acp_sessions,
    )
}

/// Warm shared parsers on a bounded stack without constructing native tools.
pub(super) fn warm_turn_parsers() -> anyhow::Result<()> {
    std::thread::scope(|scope| -> anyhow::Result<()> {
        let handle = std::thread::Builder::new()
            .name("zeroclaw-turn-parsers".into())
            .stack_size(crate::tools::TOOL_REGISTRY_BUILD_STACK_BYTES)
            .spawn_scoped(scope, crate::tools::warm_lazy_regexes)
            .map_err(|error| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": error.to_string()})),
                    "composition_turn_parser_spawn_failed"
                );
                anyhow::Error::msg(format!("failed to spawn turn-parser thread: {error}"))
            })?;
        match handle.join() {
            Ok(()) => Ok(()),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}
