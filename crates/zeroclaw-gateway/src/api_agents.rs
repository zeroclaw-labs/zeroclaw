//! `GET /api/agents/summary` — agent overview surface.
//!
//! Returns one row per configured agent with the runtime-relevant facts the
//! dashboard needs to render `rBots()`: alias, enabled, channel count,
//! workspace path, and — most importantly for `Plans/binary-seeking-
//! umbrella.md` Phase 4-5 — whether the agent has its own dedicated
//! gateway and the URL operators should hit to reach it.
//!
//! The dashboard side is a separate slice; this PR ships only the
//! backend so per-agent gateway info is queryable from anywhere.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use super::AppState;
use super::api::require_auth;

/// One row per configured agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub alias: String,
    pub enabled: bool,
    pub channel_count: usize,
    /// Resolved per-agent workspace dir as displayed in the dashboard.
    pub workspace_dir: String,
    /// Per-agent gateway URL when the agent declares its own
    /// `gateway_port`. `None` means the agent shares the global gateway.
    pub gateway_url: Option<String>,
    /// Resolved gateway port (per-agent override when set, else the
    /// global `gateway.port`). Surfaced separately from `gateway_url`
    /// so the dashboard can show a "shared" badge without re-parsing.
    pub gateway_port: u16,
    /// True when the agent has its own dedicated gateway supervisor
    /// running (`gateway_port` is `Some`). Lets the dashboard distinguish
    /// "shared global gateway" from "isolated per-agent gateway" without
    /// inferring it from the URL string.
    pub dedicated_gateway: bool,
}

/// Wrapper carrying the overview list plus the global gateway URL so
/// the dashboard can show "shared" agents with a click-through to the
/// canonical endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsSummaryResponse {
    pub global_gateway_url: String,
    pub agents: Vec<AgentSummary>,
}

/// `GET /api/agents/summary` — read-only overview of configured agents
/// with per-agent gateway info.
pub async fn handle_agents_summary(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let host = config.gateway.host.trim();
    let scheme = "http"; // Gateway is HTTP today; HTTPS termination is upstream.

    let global_gateway_url = format!("{scheme}://{host}:{}", config.gateway.port);

    let mut agents: Vec<AgentSummary> = config
        .agents
        .iter()
        .map(|(alias, agent)| {
            let port = agent.gateway_port.unwrap_or(config.gateway.port);
            let gateway_url = agent.gateway_port.map(|p| format!("{scheme}://{host}:{p}"));
            AgentSummary {
                alias: alias.clone(),
                enabled: agent.enabled,
                channel_count: agent.channels.len(),
                workspace_dir: config.agent_workspace_dir(alias).display().to_string(),
                gateway_url,
                gateway_port: port,
                dedicated_gateway: agent.gateway_port.is_some(),
            }
        })
        .collect();
    // Deterministic order so the dashboard doesn't reshuffle per refresh.
    agents.sort_by(|a, b| a.alias.cmp(&b.alias));

    Json(AgentsSummaryResponse {
        global_gateway_url,
        agents,
    })
    .into_response()
}
