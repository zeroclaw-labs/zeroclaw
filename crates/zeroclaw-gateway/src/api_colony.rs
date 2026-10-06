//! Paired operator surface for saved colonies and runtime-owned goals.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use zeroclaw_colony::{ClarificationAnswer, ColonyMessage, ColonyRuntime, GoalRequest, GoalView};
use zeroclaw_config::colony::ColonyConfig;

use crate::{
    AppState,
    api::require_auth,
    api_config::persist_and_swap,
    principal_gate::{ConfigWriteSet, RequestPrincipal, authorize_config_write},
};

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyDetail {
    pub id: String,
    pub definition: ColonyConfig,
    pub goals: Vec<GoalView>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyAccess {
    pub allowed_tools: Option<Vec<String>>,
    pub allowed_commands: Vec<String>,
    pub allowed_roots: Vec<String>,
    pub workspace_only: bool,
    pub always_ask: Vec<String>,
    pub excluded_tools: Vec<String>,
    pub network_domains: std::collections::HashMap<String, Vec<String>>,
    pub max_actions_per_hour: u32,
    pub max_cost_per_day_cents: u32,
    pub daily_limit_usd: f64,
    pub monthly_limit_usd: f64,
    pub cost_tracking_enabled: bool,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyRiskAccess {
    pub allowed_tools: Option<Vec<String>>,
    pub allowed_commands: Vec<String>,
    pub allowed_roots: Vec<String>,
    pub workspace_only: bool,
    pub always_ask: Vec<String>,
    pub excluded_tools: Vec<String>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyRuntimeAccess {
    pub max_actions_per_hour: u32,
    pub max_cost_per_day_cents: u32,
}

pub use zeroclaw_colony::context_sources::ColonyContextSource;

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyAgent {
    pub alias: String,
    pub core_command: String,
    pub enabled: bool,
    pub colony_id: Option<String>,
    pub model_provider: String,
    pub risk_profile: String,
    pub runtime_profile: String,
    pub active_turns: usize,
    pub access: ColonyAccess,
    pub context_sources: Vec<ColonyContextSource>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyChannelOption {
    pub id: String,
    pub label: String,
    pub enabled: bool,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonySnapshot {
    pub colonies: Vec<ColonyDetail>,
    pub agents: Vec<ColonyAgent>,
    pub channels: Vec<ColonyChannelOption>,
    pub risk_profiles: Vec<String>,
    pub runtime_profiles: Vec<String>,
    pub risk_profile_access: std::collections::HashMap<String, ColonyRiskAccess>,
    pub runtime_profile_access: std::collections::HashMap<String, ColonyRuntimeAccess>,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyAgentProfiles {
    pub risk_profile: String,
    pub runtime_profile: String,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyCreateRequest {
    pub id: String,
    pub definition: ColonyConfig,
    #[serde(default)]
    pub queen_template: Option<String>,
    #[serde(default)]
    pub core_commands: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub agent_profiles: std::collections::HashMap<String, ColonyAgentProfiles>,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyAgentCreateRequest {
    pub alias: String,
    pub core_command: String,
    pub template: String,
    pub expected_definition: ColonyConfig,
    #[serde(default)]
    pub connections: Vec<zeroclaw_config::colony::ColonyConnection>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PromptActivation {
    Now,
    NextRun,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonySaveRequest {
    pub definition: ColonyConfig,
    pub expected_definition: ColonyConfig,
    #[serde(default)]
    pub prompt_activation: Option<PromptActivation>,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyClarifyRequest {
    pub objective: String,
    #[serde(default)]
    pub answers: Vec<ClarificationAnswer>,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyGoalClarifyRequest {
    pub answers: Vec<ClarificationAnswer>,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ColonyControlAction {
    Start,
    Pause,
    Resume,
    Cancel,
    RetryTurn,
    SkipTurn,
    ConfirmPlan,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyControlRequest {
    pub action: ColonyControlAction,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyApprovalRequest {
    pub approved: bool,
}

#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyMessageRequest {
    pub recipient: String,
    pub content: String,
}

#[derive(Default, Deserialize)]
pub struct ColonyMessagesQuery {
    pub recipient: Option<String>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyMessagesResponse {
    pub messages: Vec<ColonyMessage>,
}

fn error(status: StatusCode, code: &str, message: impl ToString) -> Response {
    (
        status,
        Json(serde_json::json!({"code":code,"error":message.to_string()})),
    )
        .into_response()
}

fn authentication_error(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    if headers.contains_key(crate::principal_gate::AUTH_PROVIDER_HEADER) {
        return Some(error(
            StatusCode::FORBIDDEN,
            "colony_native_operator_required",
            "Colony requires a paired native operator",
        ));
    }
    if !state.pairing.require_pairing() {
        return Some(error(
            StatusCode::FORBIDDEN,
            "colony_pairing_required",
            "Colony requires pairing",
        ));
    }
    require_auth(state, headers)
        .err()
        .map(IntoResponse::into_response)
}

pub(crate) async fn runtime(state: &AppState) -> anyhow::Result<Arc<ColonyRuntime>> {
    state
        .colony_runtime
        .get_or_try_init(|| async {
            let capability =
                zeroclaw_runtime::live_config_authority::AgentExecutionCapability::from_parts(
                    Arc::clone(&state.config),
                    state.agent_lifecycle.clone(),
                );
            ColonyRuntime::open_shared(Arc::clone(&state.config), capability)
        })
        .await
        .cloned()
}

async fn detail(state: &AppState, id: &str) -> anyhow::Result<ColonyDetail> {
    let definition = state
        .config
        .read()
        .colonies
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow::Error::msg("unknown colony"))?;
    let goals = runtime(state).await?.goals(id).await?;
    Ok(ColonyDetail {
        id: id.into(),
        definition,
        goals,
    })
}

pub async fn snapshot(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let config = state.config.read().clone();
    let mut ids: Vec<_> = config.colonies.keys().cloned().collect();
    ids.sort();
    let mut colonies = Vec::new();
    for id in ids {
        match detail(&state, &id).await {
            Ok(value) => colonies.push(value),
            Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
        }
    }
    let mut aliases: Vec<_> = config.agents.keys().cloned().collect();
    aliases.sort();
    let mut agents = Vec::new();
    for alias in aliases {
        let agent = &config.agents[&alias];
        let risk = match config.risk_profiles.get(agent.risk_profile.as_str()) {
            Some(risk) => risk,
            None => continue,
        };
        agents.push(ColonyAgent {
            alias: alias.clone(),
            core_command: agent.core_command.clone(),
            enabled: agent.enabled,
            colony_id: config.colony_for_agent(&alias).map(|(id, _)| id.to_owned()),
            model_provider: agent.model_provider.to_string(),
            risk_profile: agent.risk_profile.to_string(),
            runtime_profile: agent.runtime_profile.to_string(),
            active_turns: state.agent_lifecycle.active_turn_count(&alias),
            access: ColonyAccess {
                allowed_tools: risk.effective_allowed_tools(),
                allowed_commands: risk.allowed_commands.clone(),
                allowed_roots: risk.allowed_roots.clone(),
                workspace_only: risk.workspace_only,
                always_ask: risk.always_ask.clone(),
                excluded_tools: risk.excluded_tools.clone(),
                network_domains: std::collections::HashMap::from([
                    ("browser".into(), config.browser.allowed_domains.clone()),
                    (
                        "http_request".into(),
                        config.http_request.allowed_domains.clone(),
                    ),
                    ("web_fetch".into(), config.web_fetch.allowed_domains.clone()),
                ]),
                max_actions_per_hour: config
                    .runtime_profiles
                    .get(agent.runtime_profile.as_str())
                    .map_or(0, |p| p.max_actions_per_hour),
                max_cost_per_day_cents: config
                    .runtime_profiles
                    .get(agent.runtime_profile.as_str())
                    .map_or(0, |p| p.max_cost_per_day_cents),
                daily_limit_usd: config.cost.daily_limit_usd,
                monthly_limit_usd: config.cost.monthly_limit_usd,
                cost_tracking_enabled: config.cost.enabled,
            },
            context_sources: Vec::new(),
        });
    }
    let channels = config
        .channels_by_alias()
        .into_iter()
        .map(|channel| {
            let id = format!("{}.{}", channel.channel_type, channel.alias);
            ColonyChannelOption {
                label: id.clone(),
                id,
                enabled: channel.enabled,
            }
        })
        .collect();
    let mut risk_profiles = config.risk_profiles.keys().cloned().collect::<Vec<_>>();
    risk_profiles.sort();
    let mut runtime_profiles = config.runtime_profiles.keys().cloned().collect::<Vec<_>>();
    runtime_profiles.sort();
    let risk_profile_access = config
        .risk_profiles
        .iter()
        .map(|(id, risk)| {
            (
                id.clone(),
                ColonyRiskAccess {
                    allowed_tools: risk.effective_allowed_tools(),
                    allowed_commands: risk.allowed_commands.clone(),
                    allowed_roots: risk.allowed_roots.clone(),
                    workspace_only: risk.workspace_only,
                    always_ask: risk.always_ask.clone(),
                    excluded_tools: risk.excluded_tools.clone(),
                },
            )
        })
        .collect();
    let runtime_profile_access = config
        .runtime_profiles
        .iter()
        .map(|(id, p)| {
            (
                id.clone(),
                ColonyRuntimeAccess {
                    max_actions_per_hour: p.max_actions_per_hour,
                    max_cost_per_day_cents: p.max_cost_per_day_cents,
                },
            )
        })
        .collect();
    Json(ColonySnapshot {
        colonies,
        agents,
        channels,
        risk_profiles,
        runtime_profiles,
        risk_profile_access,
        runtime_profile_access,
    })
    .into_response()
}

#[derive(Deserialize)]
pub struct ContextQuery {
    pub agent: String,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyContextResponse {
    pub sources: Vec<ColonyContextSource>,
}

pub async fn context_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ContextQuery>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let config = state.config.read().clone();
    if !config.agents.contains_key(&query.agent) {
        return error(
            StatusCode::NOT_FOUND,
            "colony_agent_missing",
            "Unknown agent",
        );
    }
    match zeroclaw_colony::context_sources::list_context_sources(&config, &query.agent).await {
        Ok(sources) => Json(ColonyContextResponse { sources }).into_response(),
        Err(e) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "colony_context_unavailable",
            e,
        ),
    }
}

pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    match detail(&state, &id).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::NOT_FOUND, "colony_missing", e),
    }
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    principal: RequestPrincipal,
    Json(body): Json<ColonyCreateRequest>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    if let Err(e) = zeroclaw_config::helpers::validate_alias_key(&body.id) {
        return error(StatusCode::BAD_REQUEST, "colony_invalid", e);
    }
    let guard = Arc::clone(&state.config_write_lock).lock_owned().await;
    let before = state.config.read().clone();
    if before.colonies.contains_key(&body.id) {
        return error(
            StatusCode::CONFLICT,
            "colony_exists",
            "Colony already exists",
        );
    }
    let mut working = before.clone();
    if let Some(template) = body.queen_template {
        if working.agents.contains_key(&body.definition.queen) {
            return error(
                StatusCode::CONFLICT,
                "colony_queen_exists",
                "Queen alias already exists",
            );
        }
        let Some(source) = working.agents.get(&template) else {
            return error(
                StatusCode::BAD_REQUEST,
                "colony_template_missing",
                "Unknown Queen template",
            );
        };
        let mut queen = source.clone();
        queen.channels.clear();
        queen.delegates.clear();
        queen.delegate_same_risk_profile = false;
        queen.workspace = Default::default();
        queen.memory = Default::default();
        queen.identity = Default::default();
        queen.a2a = Default::default();
        queen.core_command="Coordinate this colony's goal, clarify missing requirements, and assign work to the team within its approved boundaries.".into();
        queen.enabled = true;
        working.agents.insert(body.definition.queen.clone(), queen);
        working.mark_dirty(&format!("agents.{}", body.definition.queen));
    }
    for alias in body.definition.agent_aliases() {
        if state.agent_lifecycle.active_turn_count(alias) > 0 {
            return error(
                StatusCode::CONFLICT,
                "colony_agent_busy",
                "Wait for selected agents to finish their current work",
            );
        }
    }
    for (alias, command) in body.core_commands {
        if !body.definition.contains(&alias) {
            return error(
                StatusCode::BAD_REQUEST,
                "colony_invalid",
                "Core command target is outside the team",
            );
        }
        let Some(agent) = working.agents.get_mut(&alias) else {
            return error(
                StatusCode::BAD_REQUEST,
                "colony_agent_missing",
                "Unknown agent",
            );
        };
        agent.core_command = command;
        working.mark_dirty(&format!("agents.{alias}.core_command"));
    }
    for (alias, profiles) in body.agent_profiles {
        if !body.definition.contains(&alias)
            || !working.risk_profiles.contains_key(&profiles.risk_profile)
            || !working
                .runtime_profiles
                .contains_key(&profiles.runtime_profile)
        {
            return error(
                StatusCode::BAD_REQUEST,
                "colony_invalid",
                "Choose existing profiles for a team member",
            );
        }
        let Some(agent) = working.agents.get_mut(&alias) else {
            return error(
                StatusCode::BAD_REQUEST,
                "colony_agent_missing",
                "Unknown agent",
            );
        };
        agent.risk_profile = profiles.risk_profile.into();
        agent.runtime_profile = profiles.runtime_profile.into();
        working.mark_dirty(&format!("agents.{alias}.risk_profile"));
        working.mark_dirty(&format!("agents.{alias}.runtime_profile"));
    }
    working.colonies.insert(body.id.clone(), body.definition);
    working.mark_dirty(&format!("colonies.{}", body.id));
    if let Err(e) = working.validate() {
        return error(StatusCode::BAD_REQUEST, "colony_invalid", e);
    }
    let writes = ConfigWriteSet::by_effect(
        &before,
        &working,
        working.dirty_paths.iter().map(String::as_str),
    );
    let authorization = match authorize_config_write(&principal, writes, &guard) {
        Ok(value) => value,
        Err(e) => return e.into_response(),
    };
    let retained = match persist_and_swap(&state, authorization, working, guard).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    drop(retained);
    match detail(&state, &body.id).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
    }
}

pub async fn add_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    principal: RequestPrincipal,
    Path(id): Path<String>,
    Json(body): Json<ColonyAgentCreateRequest>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    if let Err(e) = zeroclaw_config::helpers::validate_alias_key(&body.alias) {
        return error(StatusCode::BAD_REQUEST, "colony_invalid", e);
    }
    let guard = Arc::clone(&state.config_write_lock).lock_owned().await;
    let before = state.config.read().clone();
    let Some(colony) = before.colonies.get(&id) else {
        return error(StatusCode::NOT_FOUND, "colony_missing", "Unknown colony");
    };
    if serde_json::to_value(colony).ok() != serde_json::to_value(&body.expected_definition).ok() {
        return error(
            StatusCode::CONFLICT,
            "colony_changed",
            "Colony changed; reload before adding an agent",
        );
    }
    if !colony.contains(&body.template) || body.core_command.trim().is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "colony_invalid",
            "Choose a team template and core command",
        );
    }
    if before.agents.contains_key(&body.alias) {
        return error(
            StatusCode::CONFLICT,
            "colony_agent_exists",
            "Agent alias already exists",
        );
    }
    let controller = match runtime(&state).await {
        Ok(value) => value,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
    };
    let goals = match controller.goals(&id).await {
        Ok(value) => value,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
    };
    if goals
        .iter()
        .any(|g| g.task.status == zeroclaw_runtime::control_plane::TaskStatus::Running)
    {
        return error(
            StatusCode::CONFLICT,
            "colony_members_busy",
            "Pause the colony before changing the team",
        );
    }
    let mut working = before.clone();
    if let Err(e) = working.add_colony_agent(&id, &body.template, &body.alias, &body.core_command) {
        return error(StatusCode::BAD_REQUEST, "colony_invalid", e);
    }
    if let Some(colony) = working.colonies.get_mut(&id) {
        colony.connections.extend(body.connections);
    }
    working.mark_dirty(&format!("agents.{}", body.alias));
    working.mark_dirty(&format!("colonies.{id}"));
    if let Err(e) = working.validate() {
        return error(StatusCode::BAD_REQUEST, "colony_invalid", e);
    }
    let authorization = match authorize_config_write(
        &principal,
        ConfigWriteSet::by_effect(
            &before,
            &working,
            working.dirty_paths.iter().map(String::as_str),
        ),
        &guard,
    ) {
        Ok(value) => value,
        Err(e) => return e.into_response(),
    };
    let retained = match persist_and_swap(&state, authorization, working, guard).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    drop(retained);
    match detail(&state, &id).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
    }
}

fn apply_prompt_edits(
    old: &ColonyConfig,
    new: &mut ColonyConfig,
    active: bool,
    activation: Option<PromptActivation>,
) -> anyhow::Result<()> {
    let changed = old.prompts.len() != new.prompts.len()
        || old
            .prompts
            .iter()
            .zip(&new.prompts)
            .any(|(a, b)| a.id != b.id || a.text != b.text || a.agents != b.agents);
    if changed && active && activation.is_none() {
        anyhow::bail!("Choose prompt activation timing");
    }
    new.instruction_revision = old.instruction_revision;
    if changed && (!active || matches!(activation, Some(PromptActivation::Now))) {
        new.instruction_revision = old
            .instruction_revision
            .checked_add(1)
            .ok_or_else(|| anyhow::Error::msg("instruction revision exhausted"))?;
    }
    for prompt in &mut new.prompts {
        if let Some(previous) = old.prompts.iter().find(|p| p.id == prompt.id) {
            prompt.revision = if prompt.text != previous.text || prompt.agents != previous.agents {
                previous
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| anyhow::Error::msg("prompt revision exhausted"))?
            } else {
                previous.revision
            };
        } else {
            prompt.revision = 1;
        }
    }
    Ok(())
}

pub async fn save(
    State(state): State<AppState>,
    headers: HeaderMap,
    principal: RequestPrincipal,
    Path(id): Path<String>,
    Json(mut body): Json<ColonySaveRequest>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let guard = Arc::clone(&state.config_write_lock).lock_owned().await;
    let before = state.config.read().clone();
    let Some(old) = before.colonies.get(&id) else {
        return error(StatusCode::NOT_FOUND, "colony_missing", "Unknown colony");
    };
    if serde_json::to_value(old).ok() != serde_json::to_value(&body.expected_definition).ok() {
        return error(
            StatusCode::CONFLICT,
            "colony_changed",
            "Colony changed; reload before saving",
        );
    }
    let controller = match runtime(&state).await {
        Ok(value) => value,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
    };
    let goals = match controller.goals(&id).await {
        Ok(value) => value,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
    };
    let active = goals
        .iter()
        .any(|g| g.task.status == zeroclaw_runtime::control_plane::TaskStatus::Running)
        || old
            .agent_aliases()
            .iter()
            .any(|alias| state.agent_lifecycle.active_turn_count(alias) > 0);
    let changed_members = old.agent_aliases() != body.definition.agent_aliases();
    if changed_members
        && (active
            || old
                .agent_aliases()
                .iter()
                .any(|alias| state.agent_lifecycle.active_turn_count(alias) > 0))
    {
        return error(
            StatusCode::CONFLICT,
            "colony_members_busy",
            "Pause and settle the colony before changing members",
        );
    }
    if let Err(e) = apply_prompt_edits(old, &mut body.definition, active, body.prompt_activation) {
        return error(StatusCode::BAD_REQUEST, "colony_prompt_activation", e);
    }
    let mut working = before.clone();
    working.colonies.insert(id.clone(), body.definition);
    working.mark_dirty(&format!("colonies.{id}"));
    if let Err(e) = working.validate() {
        return error(StatusCode::BAD_REQUEST, "colony_invalid", e);
    }
    let authorization = match authorize_config_write(
        &principal,
        ConfigWriteSet::by_effect(&before, &working, [format!("colonies.{id}").as_str()]),
        &guard,
    ) {
        Ok(value) => value,
        Err(e) => return e.into_response(),
    };
    let retained = match persist_and_swap(&state, authorization, working, guard).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    drop(retained);
    match detail(&state, &id).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
    }
}

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    principal: RequestPrincipal,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let guard = Arc::clone(&state.config_write_lock).lock_owned().await;
    let before = state.config.read().clone();
    let Some(colony) = before.colonies.get(&id) else {
        return error(StatusCode::NOT_FOUND, "colony_missing", "Unknown colony");
    };
    let controller = match runtime(&state).await {
        Ok(value) => value,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
    };
    let goals = match controller.goals(&id).await {
        Ok(value) => value,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, "colony_unavailable", e),
    };
    if goals.iter().any(|g| !g.task.status.is_terminal())
        || colony
            .agent_aliases()
            .iter()
            .any(|a| state.agent_lifecycle.active_turn_count(a) > 0)
    {
        return error(
            StatusCode::CONFLICT,
            "colony_members_busy",
            "Complete or cancel the goal before removing the colony",
        );
    }
    let mut working = before.clone();
    working.colonies.remove(&id);
    working.mark_dirty("colonies");
    let authorization = match authorize_config_write(
        &principal,
        ConfigWriteSet::default().with("colonies", zeroclaw_api::grants::Verb::Delete),
        &guard,
    ) {
        Ok(value) => value,
        Err(e) => return e.into_response(),
    };
    match persist_and_swap(&state, authorization, working, guard).await {
        Ok(_) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(response) => response,
    }
}

pub async fn clarify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ColonyClarifyRequest>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let result = async {
        runtime(&state)
            .await?
            .clarify(&id, &body.objective, &body.answers)
            .await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, "colony_clarify_failed", e),
    }
}

pub async fn goals(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let result = async { runtime(&state).await?.goals(&id).await }.await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, "colony_goals_failed", e),
    }
}

pub async fn clarify_goal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, goal)): Path<(String, String)>,
    Json(body): Json<ColonyGoalClarifyRequest>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let result = async {
        let controller = runtime(&state).await?;
        if controller.goal(&goal).await?.execution.colony_id != id {
            anyhow::bail!("Goal does not belong to this colony");
        }
        controller.clarify_goal(&goal, &body.answers).await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::CONFLICT, "colony_clarify_failed", e),
    }
}

pub async fn create_goal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<GoalRequest>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let result = async {
        let controller = runtime(&state).await?;
        zeroclaw_runtime::live_config_authority::spawn_agent_lifecycle_job(async move {
            controller.create_goal(&id, body).await
        })
        .await
        .map_err(|error| anyhow::Error::msg(format!("Goal publication failed: {error}")))?
    }
    .await;
    match result {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(e) => error(StatusCode::CONFLICT, "colony_goal_failed", e),
    }
}

pub async fn control(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, goal_id)): Path<(String, String)>,
    Json(body): Json<ColonyControlRequest>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let result = async {
        let controller = runtime(&state).await?;
        if !controller
            .goals(&id)
            .await?
            .iter()
            .any(|goal| goal.task.id == goal_id)
        {
            anyhow::bail!("Goal does not belong to this colony");
        }
        match body.action {
            ColonyControlAction::Start => {
                controller.start(&goal_id).await?;
            }
            ColonyControlAction::Pause => {
                controller.pause(&goal_id).await?;
            }
            ColonyControlAction::Resume => {
                controller.resume(&goal_id).await?;
            }
            ColonyControlAction::Cancel => {
                controller.cancel(&goal_id).await?;
            }
            ColonyControlAction::RetryTurn => {
                controller.reconcile_turn(&goal_id, true).await?;
            }
            ColonyControlAction::SkipTurn => {
                controller.reconcile_turn(&goal_id, false).await?;
            }
            ColonyControlAction::ConfirmPlan => {
                controller.confirm_plan(&goal_id).await?;
            }
        };
        controller
            .goals(&id)
            .await?
            .into_iter()
            .find(|goal| goal.task.id == goal_id)
            .ok_or_else(|| anyhow::Error::msg("Goal missing after control"))
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::CONFLICT, "colony_control_failed", e),
    }
}

pub async fn approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, goal, approval)): Path<(String, String, String)>,
    Json(body): Json<ColonyApprovalRequest>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let result = async {
        let controller = runtime(&state).await?;
        if controller.goal(&goal).await?.execution.colony_id != id {
            anyhow::bail!("Goal does not belong to this colony");
        }
        controller
            .approve_tool(&goal, &approval, body.approved)
            .await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(StatusCode::CONFLICT, "colony_approval_failed", e),
    }
}

pub async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ColonyMessagesQuery>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let result = async {
        runtime(&state)
            .await?
            .messages(&id, query.recipient.as_deref())
            .await
    }
    .await;
    match result {
        Ok(messages) => Json(ColonyMessagesResponse { messages }).into_response(),
        Err(e) => error(StatusCode::BAD_REQUEST, "colony_messages_failed", e),
    }
}

pub async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ColonyMessageRequest>,
) -> Response {
    if let Some(response) = authentication_error(&state, &headers) {
        return response;
    }
    let result = async {
        runtime(&state)
            .await?
            .send_message(&id, &body.recipient, &body.content)
            .await
    }
    .await;
    match result {
        Ok(messages) => Json(ColonyMessagesResponse { messages }).into_response(),
        Err(e) => error(StatusCode::CONFLICT, "colony_message_failed", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    use zeroclaw_config::{colony::ColonyPrompt, schema::Config};

    async fn surface(tmp: &tempfile::TempDir, paired: bool) -> (AppState, Router, String) {
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        config.risk_profiles.entry("default".into()).or_default();
        config.runtime_profiles.entry("default".into()).or_default();
        config.providers.models.openai.insert(
            "synthetic".into(),
            zeroclaw_config::schema::OpenAIModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("synthetic-model".into()),
                    ..Default::default()
                },
            },
        );
        for alias in ["queen_a", "queen_b", "worker"] {
            config.agents.insert(
                alias.into(),
                zeroclaw_config::schema::AliasedAgentConfig {
                    model_provider: "openai.synthetic".into(),
                    risk_profile: "default".into(),
                    runtime_profile: "default".into(),
                    ..Default::default()
                },
            );
        }
        let mut state = crate::api::tests::test_state(config.clone());
        state.pairing = Arc::new(zeroclaw_config::pairing::PairingGuard::new(
            paired,
            &[],
            Default::default(),
        ));
        let token = if paired {
            state
                .pairing
                .try_pair(&state.pairing.pairing_code().unwrap(), "colony-test")
                .await
                .unwrap()
                .unwrap()
        } else {
            String::new()
        };
        let auth = Arc::new(
            crate::principal_gate::GatewayInboundAuth::from_config(
                &config,
                Arc::clone(&state.pairing),
            )
            .unwrap(),
        );
        let router = crate::config_admin_router(&auth).with_state(state.clone());
        (state, router, token)
    }

    fn request(method: &str, uri: &str, token: &str, body: serde_json::Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if !token.is_empty() {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    fn definition(queen: &str) -> ColonyConfig {
        ColonyConfig {
            name: "Job search".into(),
            queen: queen.into(),
            members: vec!["worker".into()],
            connections: vec![zeroclaw_config::colony::ColonyConnection {
                from: queen.into(),
                to: "worker".into(),
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn colony_http_surface_is_closed_without_native_pairing() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, router, _) = surface(&tmp, true).await;
        let response = router
            .oneshot(request(
                "POST",
                "/api/colonies",
                "",
                serde_json::json!({
            "id":"jobs", "definition":definition("queen_a")}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(state.config.read().colonies.is_empty());
        let (_, router, _) = surface(&tmp, false).await;
        assert_eq!(
            router
                .oneshot(request("GET", "/api/colonies", "", serde_json::Value::Null))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn colony_handler_does_not_accept_native_token_with_provider_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _, token) = surface(&tmp, true).await;
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        headers.insert(
            crate::principal_gate::AUTH_PROVIDER_HEADER,
            "synthetic-provider".parse().unwrap(),
        );
        let response = snapshot(State(state.clone()), headers).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(state.config.read().colonies.is_empty());
    }

    #[tokio::test]
    async fn concurrent_colony_creation_claims_member_once_and_persists_direction() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, router, token) = surface(&tmp, true).await;
        let first = request(
            "POST",
            "/api/colonies",
            &token,
            serde_json::json!({"id":"first","definition":definition("queen_a")}),
        );
        let second = request(
            "POST",
            "/api/colonies",
            &token,
            serde_json::json!({"id":"second","definition":definition("queen_b")}),
        );
        let (first, second) = tokio::join!(router.clone().oneshot(first), router.oneshot(second));
        let statuses = [first.unwrap().status(), second.unwrap().status()];
        assert_eq!(
            statuses
                .iter()
                .filter(|&&s| s == StatusCode::CREATED)
                .count(),
            1,
            "{statuses:?}"
        );
        assert_eq!(state.config.read().colonies.len(), 1);
        let persisted: Config =
            toml::from_str(&std::fs::read_to_string(tmp.path().join("config.toml")).unwrap())
                .unwrap();
        assert_eq!(persisted.colonies.len(), 1);
        let queen = &persisted.colonies.values().next().unwrap().queen;
        assert!(persisted.colony_allows_communication(queen, "worker"));
        assert!(!persisted.colony_allows_communication("worker", queen));
    }

    #[tokio::test]
    async fn colony_http_join_rejects_active_work_and_stale_save_preserves_definition() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, router, token) = surface(&tmp, true).await;
        let turn = state.agent_lifecycle.reserve_turn("worker").unwrap();
        let body = serde_json::json!({"id":"jobs", "definition":definition("queen_a")});
        assert_eq!(
            router
                .clone()
                .oneshot(request("POST", "/api/colonies", &token, body.clone()))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        drop(turn);
        let response = router
            .clone()
            .oneshot(request("POST", "/api/colonies", &token, body))
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let old = definition("queen_a");
        let mut changed = old.clone();
        changed.name = "Edited team".into();
        let body = serde_json::json!({"definition":changed,"expected_definition":old});
        assert_eq!(
            router
                .clone()
                .oneshot(request("PUT", "/api/colonies/jobs", &token, body.clone()))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            router
                .oneshot(request("PUT", "/api/colonies/jobs", &token, body))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(state.config.read().colonies["jobs"].name, "Edited team");
    }

    #[test]
    fn active_prompt_edits_require_timing_and_keep_deferred_revision() {
        let old = ColonyConfig {
            prompts: vec![ColonyPrompt {
                id: "p".into(),
                text: "old".into(),
                revision: 3,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut new = old.clone();
        new.prompts[0].text = "new".into();
        assert!(apply_prompt_edits(&old, &mut new, true, None).is_err());
        apply_prompt_edits(&old, &mut new, true, Some(PromptActivation::NextRun)).unwrap();
        assert_eq!(new.prompts[0].text, "new");
        assert_eq!(new.prompts[0].revision, 4);
        assert_eq!(new.instruction_revision, old.instruction_revision);
        let mut now = old.clone();
        now.prompts[0].text = "new".into();
        apply_prompt_edits(&old, &mut now, true, Some(PromptActivation::Now)).unwrap();
        assert_eq!(now.instruction_revision, old.instruction_revision + 1);
    }
}
