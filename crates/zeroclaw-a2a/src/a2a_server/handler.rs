//! A2A server (inbound) — JSON-RPC handler + axum routes.
//!
//! Hosts register agents through [`A2aServerState`] (each with an [`A2aExecutor`] for
//! execution and an optional [`AgentCardProducer`] for discovery); this crate owns the
//!
//! The open-source implementation is synchronous: no task store / state machine /
//! streaming; `GetTask` / `CancelTask` / `SendStreamingMessage` are out of scope —
//! those are later host extensions. Behavior is unchanged from before the refactor.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;

use crate::a2a::a2a_wire::{SendMessageParams, SendMessageResponse};

use super::agent_card::{AgentCardProducer, StaticAgentCard};
use super::error::ServerError;
use super::executor::A2aExecutor;
use super::protocol::{
    A2A_PROTOCOL_VERSION, CATALOG_CARD_PATH, CATALOG_CARD_PREFIXED_PATH,
    WELL_KNOWN_AGENT_CARD_PATH, jsonrpc_error, task_not_found_error, unsupported_operation_error,
    version_not_supported_error, wants_return_immediately,
};

/// JSON-RPC method names.
mod methods {
    pub const SEND_MESSAGE: &str = "SendMessage";
}

/// JSON-RPC 2.0 request envelope (parsed on the router side).
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default)]
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// A registered A2A agent: executor (task handling) + optional discovery-card source.
///
/// `card_producer = None` -> task handling only (`/a2a/{alias}` JSON-RPC); the Agent Card
/// is supplied externally, the alias is not in the catalog, and its per-agent card 404s.
#[derive(Clone)]
pub struct A2aAgent {
    pub executor: Arc<dyn A2aExecutor>,
    pub card_producer: Option<Arc<dyn AgentCardProducer>>,
}

/// Admission check — the host's admission/auth layer a standalone A2A service runs
///
/// By default (unset) `a2a_router` rejects all task operations (defaults closed). A
/// standalone consumer must implement this trait and register it via
/// [`A2aServerState::with_admission`], wiring host admission/auth (e.g. bearer token)
/// before protocol execution. The gateway does not use this router (it uses its own
pub trait AdmissionCheck: Send + Sync {
    /// Validate a task request; `Ok(())` admits, `Err` rejects with a reason.
    fn check(&self, headers: &axum::http::HeaderMap, alias: &str) -> Result<(), String>;
}

/// A2A server state — hosts multiple agents on one port, dispatched by alias.
#[derive(Clone, Default)]
pub struct A2aServerState {
    agents: Arc<HashMap<String, A2aAgent>>,
    admission: Option<Arc<dyn AdmissionCheck>>,
}

impl A2aServerState {
    /// Build an empty server state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent with a card (task handling + this server publishes its card).
    #[must_use]
    pub fn register(
        mut self,
        alias: impl Into<String>,
        card: crate::a2a::a2a_wire::AgentCard,
        executor: Arc<dyn A2aExecutor>,
    ) -> Self {
        let producer = Arc::new(StaticAgentCard::new(card));
        self.insert(alias.into(), Some(producer), executor);
        self
    }

    /// Register an agent for task handling only (card supplied externally, not published).
    #[must_use]
    pub fn register_executor_only(
        mut self,
        alias: impl Into<String>,
        executor: Arc<dyn A2aExecutor>,
    ) -> Self {
        self.insert(alias.into(), None, executor);
        self
    }

    /// Configure an admission check. When unset, [`a2a_router`] rejects all task
    /// operations by default; a standalone service must call this to admit tasks.
    #[must_use]
    pub fn with_admission(mut self, admission: Arc<dyn AdmissionCheck>) -> Self {
        self.admission = Some(admission);
        self
    }

    /// Register an agent with a custom [`AgentCardProducer`] (card generated on demand).
    #[must_use]
    pub fn register_with_producer(
        mut self,
        alias: impl Into<String>,
        producer: Arc<dyn AgentCardProducer>,
        executor: Arc<dyn A2aExecutor>,
    ) -> Self {
        self.insert(alias.into(), Some(producer), executor);
        self
    }

    fn insert(
        &mut self,
        alias: String,
        card_producer: Option<Arc<dyn AgentCardProducer>>,
        executor: Arc<dyn A2aExecutor>,
    ) {
        Arc::make_mut(&mut self.agents).insert(
            alias,
            A2aAgent {
                executor,
                card_producer,
            },
        );
    }

    fn get(&self, alias: &str) -> Option<&A2aAgent> {
        self.agents.get(alias)
    }
}

/// Build the A2A multi-agent server router.
pub fn a2a_router(state: A2aServerState) -> Router {
    Router::new()
        .route(WELL_KNOWN_AGENT_CARD_PATH, get(handle_catalog_card))
        .route(CATALOG_CARD_PATH, get(handle_catalog_card))
        .route(CATALOG_CARD_PREFIXED_PATH, get(handle_catalog_card))
        .route(
            &format!("/a2a/{{alias}}{WELL_KNOWN_AGENT_CARD_PATH}"),
            get(handle_agent_card),
        )
        .route("/a2a/{alias}", post(handle_task_route))
        .with_state(state)
}

/// `GET /.well-known/agents-card.json` —— catalog card。
///
/// Assemble from the registered per-alias cards: list each published alias's interfaces
async fn handle_catalog_card(
    State(state): State<A2aServerState>,
) -> Json<crate::a2a::a2a_wire::AgentCard> {
    let mut published: Vec<(&String, &A2aAgent)> = state
        .agents
        .iter()
        .filter(|(_, a)| a.card_producer.is_some())
        .collect();
    published.sort_by(|a, b| a.0.cmp(b.0));

    let cards: Vec<(String, crate::a2a::a2a_wire::AgentCard)> = published
        .iter()
        .filter_map(|(alias, a)| {
            a.card_producer
                .as_ref()
                .map(|p| ((*alias).clone(), p.card()))
        })
        .collect();

    Json(build_catalog_from_cards(&cards))
}

/// Assemble a discovery catalog card from per-alias cards.
fn build_catalog_from_cards(
    cards: &[(String, crate::a2a::a2a_wire::AgentCard)],
) -> crate::a2a::a2a_wire::AgentCard {
    use crate::a2a::a2a_wire::{AgentCapabilities, AgentCard, AgentInterface, AgentSkill};

    // Lead with the `catalog` binding marker (mirrors the gateway's catalog
    // card). Without it, a client cannot tell this aggregate catalog from a
    // single agent: `select_interface` would treat a lone published alias as a
    // catalog with one candidate and route any requested agent name — including
    // a typo or stale alias — to that alias instead of failing.
    // Derive the origin from the first published interface, preserving the URL
    // as-declared (scheme://host[:explicit-port]) — mirroring the gateway's
    // catalog, which does not force a scheme-default port into the URL.
    let catalog_url = cards
        .first()
        .and_then(|(_, card)| card.supported_interfaces.first())
        .and_then(|i| reqwest::Url::parse(&i.url).ok())
        .map(|u| {
            let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
            format!(
                "{}://{}{port}{CATALOG_CARD_PATH}",
                u.scheme(),
                u.host_str().unwrap_or_default()
            )
        })
        .unwrap_or_default();

    let mut interfaces = Vec::with_capacity(cards.len() + 1);
    interfaces.push(AgentInterface {
        url: catalog_url,
        protocol_binding: super::protocol::CATALOG_PROTOCOL_BINDING.to_string(),
        tenant: None,
        protocol_version: A2A_PROTOCOL_VERSION.to_string(),
    });
    let mut skills: Vec<AgentSkill> = Vec::new();
    for (alias, card) in cards {
        interfaces.extend(card.supported_interfaces.iter().cloned());
        for skill in &card.skills {
            let mut skill = skill.clone();
            skill.id = format!("{alias}/{}", skill.id);
            if !skill.tags.contains(alias) {
                skill.tags.push(alias.clone());
            }
            skills.push(skill);
        }
    }

    AgentCard {
        name: "ZeroClaw agents".to_string(),
        description: "Discovery catalog enumerating published A2A agents on \
                      this ZeroClaw install. Not a runnable agent; each entry \
                      below serves its own A2A card and endpoint."
            .to_string(),
        supported_interfaces: interfaces,
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: AgentCapabilities {
            streaming: Some(false),
            push_notifications: Some(false),
            extended_agent_card: Some(false),
        },
        default_input_modes: vec!["text".to_string()],
        default_output_modes: vec!["text".to_string()],
        skills,
    }
}

/// `GET /a2a/{alias}/.well-known/agent-card.json` — single-agent card.
async fn handle_agent_card(
    State(state): State<A2aServerState>,
    Path(alias): Path<String>,
) -> Response {
    match state.get(&alias).and_then(|a| a.card_producer.as_ref()) {
        Some(producer) => Json(producer.card()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `POST /a2a/{alias}` — JSON-RPC endpoint.
async fn handle_task_route(
    State(state): State<A2aServerState>,
    Path(alias): Path<String>,
    headers: axum::http::HeaderMap,
    body: Result<Json<JsonRpcRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    // RFC boundary: standalone router defaults closed — without a configured
    // host admission check, no task operation is exposed.
    if let Some(admit) = &state.admission {
        if let Err(reason) = admit.check(&headers, &alias) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(jsonrpc_error(Value::Null, -32004, &reason)),
            )
                .into_response();
        }
    } else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(jsonrpc_error(
                Value::Null,
                -32004,
                "admission not configured: this A2A server does not expose task injection by default; configure a host admission check or use a host-authenticated route",
            )),
        )
            .into_response();
    }

    let Some(agent) = state.get(&alias) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let Json(req) = match body {
        Ok(req) => req,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(jsonrpc_error(
                    Value::Null,
                    -32700,
                    "Parse error: expected a JSON-RPC 2.0 request",
                )),
            )
                .into_response();
        }
    };

    let advertised = headers.get("A2A-Version").and_then(|v| v.to_str().ok());
    handle_jsonrpc(req, agent.executor.as_ref(), advertised).await
}

/// Protocol handling (independent of axum state): parsed request + executor -> JSON-RPC
///
/// Hosts with a hand-written handler (e.g. gateway auth layer) can reuse this directly,
/// performing auth and config checks before calling it.
pub async fn handle_jsonrpc(
    req: JsonRpcRequest,
    executor: &dyn A2aExecutor,
    advertised_version: Option<&str>,
) -> Response {
    if req.jsonrpc != "2.0" {
        return (
            StatusCode::BAD_REQUEST,
            Json(jsonrpc_error(
                req.id,
                -32600,
                "Invalid request: jsonrpc must be \"2.0\"",
            )),
        )
            .into_response();
    }

    // A2A version negotiation (spec §5.x): the inbound serves A2A `1.0`.
    // A request advertising a different `A2A-Version` is rejected with a
    // `VersionNotSupportedError`, distinguished from generic JSON-RPC errors
    // by the A2A `reason` field (UPPER_SNAKE_CASE, spec §5.4).
    let advertised = advertised_version.unwrap_or("").trim();
    if advertised != A2A_PROTOCOL_VERSION {
        return Json(version_not_supported_error(req.id, advertised)).into_response();
    }

    if req.method != methods::SEND_MESSAGE {
        return Json(jsonrpc_error(
            req.id,
            -32601,
            "Method not found: only SendMessage is supported on this build",
        ))
        .into_response();
    }

    let params: SendMessageParams = match serde_json::from_value(req.params) {
        Ok(p) => p,
        Err(e) => {
            return Json(jsonrpc_error(
                req.id,
                -32602,
                &format!("Invalid params: {e}"),
            ))
            .into_response();
        }
    };

    // Spec §4: `returnImmediately: true` — this server has no task store and
    // runs the turn synchronously, so it cannot honor the contract. Reject
    // explicitly rather than silently returning a completed task.
    if wants_return_immediately(&params) {
        return Json(unsupported_operation_error(
            req.id,
            "returnImmediately=true is not supported by this server: it has no A2A task store and runs the turn synchronously. Use returnImmediately=false (the spec default) or target a peer that implements the non-terminal lifecycle.",
        ))
        .into_response();
    }
    // Spec §5.4: a supplied `taskId` must identify an existing task. This
    // stateless server has no task store → any `taskId` is unknowable.
    if let Some(tid) = params.message.task_id.as_deref() {
        return Json(task_not_found_error(req.id, tid)).into_response();
    }
    // A context-only continuation this server cannot honor is unsupported.
    if params.message.context_id.is_some() {
        return Json(unsupported_operation_error(
            req.id,
            "contextId continuation is not supported by this server: it has no durable A2A task store and starts a fresh session per request. Omit contextId or target a peer that implements context continuation.",
        ))
        .into_response();
    }

    let prompt = params
        .message
        .parts
        .iter()
        .filter_map(|p| p.as_text())
        .collect::<Vec<_>>()
        .join("\n");

    if prompt.trim().is_empty() {
        return Json(jsonrpc_error(
            req.id,
            -32602,
            "Invalid params: message has no text parts",
        ))
        .into_response();
    }

    match executor.execute(params.message).await {
        Ok(task) => Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": req.id,
            "result": SendMessageResponse::Task { task },
        }))
        .into_response(),
        Err(e) => Json(jsonrpc_error(
            req.id,
            -32000,
            &format!("Agent task failed: {e}"),
        ))
        .into_response(),
    }
}

/// `ServerError` → JSON-RPC error envelope。
#[must_use]
pub fn server_error_value(id: Value, e: &ServerError) -> Value {
    jsonrpc_error(id, e.jsonrpc_code(), &e.message())
}

/// The A2A protocol version this implementation serves.
#[must_use]
pub fn protocol_version() -> &'static str {
    A2A_PROTOCOL_VERSION
}

/// Advertised endpoint override (CLI host/port) used by hosts that build URLs
/// from the runtime listener rather than config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedEndpoint {
    pub host: String,
    pub port: u16,
}

impl AdvertisedEndpoint {
    #[must_use]
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::a2a_wire::{AgentCapabilities, AgentCard, AgentInterface};

    /// A single-agent card advertising one v1 JSON-RPC interface at
    /// `<base>/a2a/{alias}` (the ZeroClaw convention).
    fn agent_card(alias: &str) -> AgentCard {
        AgentCard {
            name: format!("agent-{alias}"),
            description: "test agent".to_string(),
            supported_interfaces: vec![AgentInterface {
                url: format!("https://peer.example.com/a2a/{alias}"),
                protocol_binding: "JSONRPC".to_string(),
                tenant: None,
                protocol_version: A2A_PROTOCOL_VERSION.to_string(),
            }],
            version: "1.0".to_string(),
            capabilities: AgentCapabilities {
                streaming: Some(false),
                push_notifications: Some(false),
                extended_agent_card: Some(false),
            },
            default_input_modes: vec!["text".to_string()],
            default_output_modes: vec!["text".to_string()],
            skills: vec![],
        }
    }

    #[test]
    fn catalog_card_advertises_catalog_marker() {
        // Greptile finding 4: the standalone catalog must carry the same
        // `catalog` interface marker as the gateway's, so a client can tell an
        // aggregate catalog from a single agent. Without it, a lone published
        // alias makes the card look like a single-agent card to
        // `select_interface`, which then routes any requested agent name —
        // including a typo or stale alias — to that alias instead of failing.
        let cards = vec![("alpha".to_string(), agent_card("alpha"))];
        let catalog = build_catalog_from_cards(&cards);

        let catalog_iface = catalog
            .supported_interfaces
            .iter()
            .find(|i| i.protocol_binding.eq_ignore_ascii_case("catalog"))
            .expect("catalog card must advertise a `catalog` interface marker");
        assert_eq!(
            catalog_iface.url,
            "https://peer.example.com/.well-known/agents-card.json"
        );

        // The published alias is still listed alongside the marker.
        assert!(
            catalog
                .supported_interfaces
                .iter()
                .any(|i| i.url == "https://peer.example.com/a2a/alpha"),
            "published alias interface must be listed: {:?}",
            catalog.supported_interfaces
        );
    }

    #[test]
    fn build_catalog_from_cards_is_empty_safe() {
        // No published aliases: the marker is still emitted (empty origin URL),
        // never a panic.
        let catalog = build_catalog_from_cards(&[]);
        assert!(
            catalog
                .supported_interfaces
                .iter()
                .any(|i| { i.protocol_binding.eq_ignore_ascii_case("catalog") })
        );
    }
}
