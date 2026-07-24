//! A2A discovery surface: the well-known catalog card and per-alias agent
//! cards.

use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
#[cfg(feature = "schema-export")]
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

use zeroclaw_config::schema::Config;
use zeroclaw_runtime::skills::SkillsService;

use crate::{AppState, api::require_auth, run_gateway_chat_with_tools};

/// A2A protocol version advertised on per-alias interfaces.
const A2A_PROTOCOL_VERSION: &str = "1.0";
/// JSON-RPC is the spec-mandated baseline transport binding.
const A2A_PROTOCOL_BINDING: &str = "JSONRPC";
const CATALOG_CARD_PATH: &str = "/.well-known/agents-card.json";
/// Prefixed alias of the catalog under the `/a2a/` namespace, serving the same
/// card so the whole A2A surface lives under one prefix while the root path
/// stays as a fallback for clients that probe the origin root first.
const CATALOG_CARD_PREFIXED_PATH: &str = "/a2a/.well-known/agents-card.json";
/// Spec well-known agent-card path (A2A §14.3, RFC 8615), used under each
/// per-alias base where a conforming single-agent card is served.
const WELL_KNOWN_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

// Card DTOs (`AgentInterface`/`AgentCapabilities`/`AgentSkill`/`AgentCard`)
// and Task/Message DTOs live in the shared wire-model source
// `zeroclaw_api::a2a_wire`, used by both this inbound surface and the
// outbound client in `zeroclaw-tools`. Re-exported here so the gateway's
// existing construction sites keep working under `crate::a2a::AgentCard`.
pub use zeroclaw_api::a2a_wire::{AgentCapabilities, AgentCard, AgentInterface, AgentSkill};

/// Runtime gateway endpoint used for A2A advertisement when the operator starts
/// the gateway with CLI host/port overrides. This is created from the listener
/// inputs at route construction time; persistent config remains the source of
/// truth for config-defined URLs and explicit A2A advertisement overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdvertisedGatewayEndpoint {
    host: String,
    port: u16,
}

impl AdvertisedGatewayEndpoint {
    #[must_use]
    pub(crate) fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

fn advertised_base(config: &Config, endpoint: Option<&AdvertisedGatewayEndpoint>) -> String {
    let server = &config.a2a.server;
    let configured = server.public_base_url.trim();
    if !configured.is_empty() {
        return configured.trim_end_matches('/').to_string();
    }
    let host = server
        .bind
        .clone()
        .or_else(|| endpoint.map(|endpoint| endpoint.host.clone()))
        .unwrap_or_else(|| config.gateway.host.clone());
    let port = server
        .port
        .or_else(|| endpoint.map(|endpoint| endpoint.port))
        .unwrap_or(config.gateway.port);
    format!("http://{host}:{port}")
}

/// Per-alias A2A base path under the advertised origin.
fn alias_base_path(alias: &str) -> String {
    format!("/a2a/{alias}")
}

/// Build the ZeroClaw discovery catalog card served at the origin root. Lists
/// every published alias as a skill-less entry pointing at its per-alias card
/// and endpoint. This is a catalog, not a runnable agent: it advertises the
/// `catalog` interface and carries no skills of its own.
#[must_use]
pub fn build_catalog_card(config: &Config) -> AgentCard {
    build_catalog_card_with_endpoint(config, None)
}

#[must_use]
pub(crate) fn build_catalog_card_with_endpoint(
    config: &Config,
    endpoint: Option<&AdvertisedGatewayEndpoint>,
) -> AgentCard {
    let base = advertised_base(config, endpoint);
    let published = published_aliases(config);

    let mut supported_interfaces = Vec::with_capacity(published.len() + 1);
    supported_interfaces.push(AgentInterface {
        url: format!("{base}{CATALOG_CARD_PATH}"),
        protocol_binding: "catalog".to_string(),
        protocol_version: A2A_PROTOCOL_VERSION.to_string(),
    });
    for alias in &published {
        supported_interfaces.push(AgentInterface {
            url: format!("{base}{}", alias_base_path(alias)),
            protocol_binding: A2A_PROTOCOL_BINDING.to_string(),
            protocol_version: A2A_PROTOCOL_VERSION.to_string(),
        });
    }

    let mut skills = Vec::new();
    for alias in &published {
        for mut skill in exposed_skills(config, alias) {
            skill.id = format!("{alias}/{}", skill.id);
            skill.tags.push(alias.clone());
            skills.push(skill);
        }
    }

    AgentCard {
        name: "ZeroClaw agents".to_string(),
        description: "Discovery catalog enumerating published A2A agents on \
                      this ZeroClaw install. Not a runnable agent; each entry \
                      below serves its own A2A card and endpoint. Skills are \
                      aggregated from the published agents, each tagged with \
                      its owning alias."
            .to_string(),
        supported_interfaces,
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

/// Build a spec-conforming per-alias agent card, or `None` when the alias is
/// unknown or not published. Skills resolve from the alias's bundles and
/// narrow through `exposed_skills`.
#[must_use]
pub fn build_agent_card(config: &Config, alias: &str) -> Option<AgentCard> {
    build_agent_card_with_endpoint(config, alias, None)
}

#[must_use]
pub(crate) fn build_agent_card_with_endpoint(
    config: &Config,
    alias: &str,
    endpoint: Option<&AdvertisedGatewayEndpoint>,
) -> Option<AgentCard> {
    let agent = config.agents.get(alias)?;
    if !agent.enabled || !agent.a2a.published {
        return None;
    }

    let base = advertised_base(config, endpoint);
    let endpoint = format!("{base}{}", alias_base_path(alias));

    AgentCard {
        name: alias.to_string(),
        description: agent_description(config, alias),
        supported_interfaces: vec![AgentInterface {
            url: endpoint,
            protocol_binding: A2A_PROTOCOL_BINDING.to_string(),
            protocol_version: A2A_PROTOCOL_VERSION.to_string(),
        }],
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: AgentCapabilities {
            streaming: Some(false),
            push_notifications: Some(false),
            extended_agent_card: Some(false),
        },
        default_input_modes: vec!["text".to_string()],
        default_output_modes: vec!["text".to_string()],
        skills: exposed_skills(config, alias),
    }
    .into()
}

/// Aliases that are both enabled and A2A-published, in stable sorted order.
fn published_aliases(config: &Config) -> Vec<String> {
    let mut out: Vec<String> = config
        .agents
        .iter()
        .filter(|(_, agent)| agent.enabled && agent.a2a.published)
        .map(|(alias, _)| alias.clone())
        .collect();
    out.sort();
    out
}

fn agent_description(config: &Config, alias: &str) -> String {
    if let Some(desc) = identity_description(config, alias) {
        return desc;
    }
    format!("ZeroClaw agent '{alias}'.")
}

/// Resolve a one-line description from the alias identity document, or `None`
/// when there is no usable line. Reuses the runtime AIEOS loader so the
/// gateway and the agent system prompt read identity through the same path.
fn identity_description(config: &Config, alias: &str) -> Option<String> {
    let agent = config.agents.get(alias)?;
    let workspace_dir = config.agent_workspace_dir(alias);
    let aieos = zeroclaw_runtime::identity::load_aieos_identity(&agent.identity, &workspace_dir)
        .ok()
        .flatten()?;
    let identity = aieos.identity?;
    let line = identity
        .bio
        .filter(|b| !b.trim().is_empty())
        .or_else(|| identity.names.and_then(identity_name_line))?;
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

/// Build a name line from identity `names`, preferring the fullest form.
fn identity_name_line(names: zeroclaw_runtime::identity::Names) -> Option<String> {
    if let Some(full) = names.full.filter(|s| !s.trim().is_empty()) {
        return Some(full);
    }
    let joined = [names.first, names.last]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !joined.is_empty() {
        return Some(joined);
    }
    names.nickname.filter(|s| !s.trim().is_empty())
}

/// Resolve the alias's exposed skills: the resolved bundle skill set, after the
/// owning bundle's include/exclude filter, narrowed by `exposed_skills`. An
/// empty filter advertises no skills. Skill ids that do not resolve to a real,
/// admitted skill are dropped (bundles are canonical).
fn exposed_skills(config: &Config, alias: &str) -> Vec<AgentSkill> {
    let agent = match config.agents.get(alias) {
        Some(a) => a,
        None => return Vec::new(),
    };
    if agent.a2a.exposed_skills.is_empty() {
        return Vec::new();
    }

    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let resolved = match service.list_skills(None) {
        Ok(skills) => skills,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for wanted in &agent.a2a.exposed_skills {
        if let Some(summary) = resolved.iter().find(|s| {
            s.r#ref.name() == wanted
                && agent.skill_bundles.iter().any(|b| b == s.r#ref.bundle())
                && config
                    .skill_bundles
                    .get(s.r#ref.bundle())
                    .is_some_and(|bundle| bundle.admits_skill(s.r#ref.name()))
        }) {
            let mut tags = vec![summary.r#ref.bundle().to_string()];
            if let Some(category) = &summary.frontmatter.category {
                if !category.is_empty() {
                    tags.push(category.clone());
                }
            }
            out.push(AgentSkill {
                id: summary.r#ref.name().to_string(),
                name: summary.frontmatter.name.clone(),
                description: summary.frontmatter.description.clone(),
                tags,
            });
        }
    }
    out
}

/// `GET /.well-known/agents-card.json` — the discovery catalog card.
async fn handle_catalog_card(
    State(state): State<AppState>,
    Extension(endpoint): Extension<Option<AdvertisedGatewayEndpoint>>,
) -> impl IntoResponse {
    let config = state.config.read().clone();
    if !config.a2a.server.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(build_catalog_card_with_endpoint(&config, endpoint.as_ref())).into_response()
}

/// `GET /a2a/{alias}/.well-known/agent-card.json` — a per-alias agent card.
async fn handle_alias_card(
    State(state): State<AppState>,
    Extension(endpoint): Extension<Option<AdvertisedGatewayEndpoint>>,
    axum::extract::Path(alias): axum::extract::Path<String>,
) -> impl IntoResponse {
    let config = state.config.read().clone();
    if !config.a2a.server.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    match build_agent_card_with_endpoint(&config, &alias, endpoint.as_ref()) {
        Some(card) => Json(card).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// JSON-RPC 2.0 request envelope for the A2A task endpoint. Only `message/send`
/// is handled on this build; other methods return a JSON-RPC method-not-found.
///
/// This is the router-side request parser (fields private to this module).
/// The shared `zeroclaw_api::a2a_wire::JsonRpcRequest` is the outbound
/// construction shape; they are intentionally separate so the router keeps
/// its parse-only surface.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub(crate) struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: String,
    id: serde_json::Value,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

// Task/Message DTOs (`MessageSendParams`/`Message`/`TextPart`/`Artifact`/
// `TaskStatus`/`Task`) come from the shared wire-model `zeroclaw_api::a2a_wire`.
// Inbound constructs responses from them; outbound deserializes peer
// responses from the same types. Re-exported under `crate::a2a::` aliases
// matching the old outbound names so the construction sites below compile
// unchanged: `OutTask` → `Task`, `OutArtifact` → `Artifact`,
// `OutTextPart` → `TextPart`, `OutTaskStatus` → `TaskStatus`.
pub use zeroclaw_api::a2a_wire::{
    Artifact, Message, MessageSendParams, Task, TaskStatus, TextPart,
};

fn jsonrpc_error(id: serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

async fn handle_alias_task(
    State(state): State<AppState>,
    axum::extract::Path(alias): axum::extract::Path<String>,
    headers: HeaderMap,
    body: Result<Json<JsonRpcRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    {
        let config = state.config.read();
        if !config.a2a.server.enabled {
            return StatusCode::NOT_FOUND.into_response();
        }
        let published = config
            .agents
            .get(&alias)
            .map(|a| a.enabled && a.a2a.published)
            .unwrap_or(false);
        if !published {
            return StatusCode::NOT_FOUND.into_response();
        }
    }

    let Json(req) = match body {
        Ok(req) => req,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(jsonrpc_error(
                    serde_json::Value::Null,
                    -32700,
                    "Parse error: expected a JSON-RPC 2.0 request",
                )),
            )
                .into_response();
        }
    };

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

    if req.method != "message/send" {
        return (
            StatusCode::OK,
            Json(jsonrpc_error(
                req.id,
                -32601,
                "Method not found: only message/send is supported on this build",
            )),
        )
            .into_response();
    }

    let params: MessageSendParams = match serde_json::from_value(req.params) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::OK,
                Json(jsonrpc_error(
                    req.id,
                    -32602,
                    &format!("Invalid params: {e}"),
                )),
            )
                .into_response();
        }
    };

    let prompt = params
        .message
        .parts
        .iter()
        .filter(|p| p.kind == "text")
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    if prompt.trim().is_empty() {
        return (
            StatusCode::OK,
            Json(jsonrpc_error(
                req.id,
                -32602,
                "Invalid params: message has no text parts",
            )),
        )
            .into_response();
    }

    let session_id = format!("a2a_{alias}_{}", Uuid::new_v4());
    match run_gateway_chat_with_tools(&state, &prompt, Some(&session_id), Some(&alias)).await {
        Ok(outcome) => {
            let task = Task {
                id: Uuid::new_v4().to_string(),
                context_id: session_id,
                status: TaskStatus {
                    state: "completed".to_string(),
                },
                artifacts: vec![Artifact {
                    artifact_id: Uuid::new_v4().to_string(),
                    parts: vec![TextPart {
                        kind: "text".to_string(),
                        text: outcome.response,
                    }],
                }],
                kind: "task".to_string(),
            };
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": task,
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::OK,
            Json(jsonrpc_error(
                req.id,
                -32000,
                &format!("Agent task failed: {e}"),
            )),
        )
            .into_response(),
    }
}

pub fn a2a_routes() -> Router<AppState> {
    a2a_routes_with_endpoint(None)
}

/// A2A discovery routes using the runtime listener endpoint for URL
/// advertisement when config does not set a stronger override.
pub(crate) fn a2a_routes_with_endpoint(
    endpoint: Option<AdvertisedGatewayEndpoint>,
) -> Router<AppState> {
    Router::new()
        .route(CATALOG_CARD_PATH, get(handle_catalog_card))
        .route(CATALOG_CARD_PREFIXED_PATH, get(handle_catalog_card))
        .route(
            &format!("/a2a/{{alias}}{WELL_KNOWN_AGENT_CARD_PATH}"),
            get(handle_alias_card),
        )
        .layer(Extension(endpoint))
}

pub fn a2a_task_route() -> Router<AppState> {
    Router::new().route("/a2a/{alias}", post(handle_alias_task))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::multi_agent::AgentA2aConfig;
    use zeroclaw_config::schema::{AliasedAgentConfig, Config};

    fn config_with_published_alias(alias: &str, published: bool) -> Config {
        let mut config = Config::default();
        config.a2a.server.enabled = true;
        let agent = AliasedAgentConfig {
            a2a: AgentA2aConfig {
                published,
                exposed_skills: Vec::new(),
            },
            ..Default::default()
        };
        config.agents.insert(alias.to_string(), agent);
        config
    }

    fn write_skill(root: &std::path::Path, bundle: &str, name: &str, display: &str) {
        let dir = root.join("shared/skills").join(bundle).join(name);
        std::fs::create_dir_all(&dir).expect("mkdir skill");
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {display}\ndescription: {display} does things.\n---\n\n# {display}\n"
            ),
        )
        .expect("write manifest");
    }

    #[test]
    fn exposed_skills_resolve_tag_and_scope_to_bundle() {
        let tmp = tempfile::tempdir().expect("tmp");
        write_skill(tmp.path(), "demo", "widget", "Widget");
        write_skill(tmp.path(), "demo", "gadget", "Gadget");
        write_skill(tmp.path(), "other", "intruder", "Intruder");

        let mut config = config_with_published_alias("maker", true);
        config.config_path = tmp.path().join("config.toml");
        config
            .skill_bundles
            .insert("demo".to_string(), Default::default());
        config
            .skill_bundles
            .insert("other".to_string(), Default::default());
        {
            let agent = config.agents.get_mut("maker").unwrap();
            agent.skill_bundles = vec!["demo".to_string()];
            agent.a2a.exposed_skills = vec!["widget".to_string(), "intruder".to_string()];
        }

        let card = build_agent_card(&config, "maker").expect("card");
        // widget resolves from the declared bundle; intruder is in a bundle the
        // agent does not declare, so it is excluded even though it exists.
        let ids: Vec<&str> = card.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["widget"]);
        assert_eq!(card.skills[0].name, "Widget");
        // Per-alias card tags carry the owning bundle.
        assert_eq!(card.skills[0].tags, vec!["demo".to_string()]);

        let catalog = build_catalog_card(&config);
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].id, "maker/widget");
        // The catalog appends the owning alias on top of the resolved bundle
        // tag rather than replacing it, so both survive.
        assert_eq!(
            catalog.skills[0].tags,
            vec!["demo".to_string(), "maker".to_string()]
        );
    }

    #[test]
    fn exposed_skills_drop_when_bundle_filter_excludes_them() {
        let tmp = tempfile::tempdir().expect("tmp");
        write_skill(tmp.path(), "demo", "widget", "Widget");
        write_skill(tmp.path(), "demo", "gadget", "Gadget");

        let mut config = config_with_published_alias("maker", true);
        config.config_path = tmp.path().join("config.toml");
        let bundle = zeroclaw_config::schema::SkillBundleConfig {
            exclude: vec!["gadget".to_string()],
            ..Default::default()
        };
        config.skill_bundles.insert("demo".to_string(), bundle);
        {
            let agent = config.agents.get_mut("maker").unwrap();
            agent.skill_bundles = vec!["demo".to_string()];
            agent.a2a.exposed_skills = vec!["widget".to_string(), "gadget".to_string()];
        }

        let card = build_agent_card(&config, "maker").expect("card");
        let ids: Vec<&str> = card.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["widget"]);
    }

    #[test]
    fn catalog_card_empty_skills_without_bundles_on_disk() {
        // No skill files on disk: aggregation yields an empty set, and the
        // catalog still enumerates the published alias interface.
        let config = config_with_published_alias("researcher", true);
        let card = build_catalog_card(&config);
        assert!(card.skills.is_empty());
        // catalog interface + one per-alias interface
        assert_eq!(card.supported_interfaces.len(), 2);
        assert_eq!(card.supported_interfaces[0].protocol_binding, "catalog");
        assert!(
            card.supported_interfaces[1]
                .url
                .ends_with("/a2a/researcher")
        );
    }

    #[test]
    fn catalog_capabilities_are_never_empty() {
        let config = config_with_published_alias("researcher", true);
        let card = build_catalog_card(&config);
        let json = serde_json::to_value(&card).expect("serialize");
        // capabilities object carries explicit flags, never `{}`
        assert!(json["capabilities"].as_object().map(|o| !o.is_empty()) == Some(true));
        assert_eq!(json["capabilities"]["streaming"], false);
    }

    #[test]
    fn catalog_card_excludes_unpublished_aliases() {
        let config = config_with_published_alias("hidden", false);
        let card = build_catalog_card(&config);
        // only the catalog interface, no alias entry
        assert_eq!(card.supported_interfaces.len(), 1);
        assert_eq!(card.supported_interfaces[0].protocol_binding, "catalog");
    }

    #[test]
    fn agent_card_none_for_unpublished_alias() {
        let config = config_with_published_alias("hidden", false);
        assert!(build_agent_card(&config, "hidden").is_none());
    }

    #[test]
    fn agent_card_none_for_unknown_alias() {
        let config = config_with_published_alias("known", true);
        assert!(build_agent_card(&config, "ghost").is_none());
    }

    #[test]
    fn agent_card_none_for_disabled_alias() {
        let mut config = config_with_published_alias("dormant", true);
        config.agents.get_mut("dormant").unwrap().enabled = false;
        // published but disabled: no card, and absent from the catalog
        assert!(build_agent_card(&config, "dormant").is_none());
        let catalog = build_catalog_card(&config);
        assert_eq!(catalog.supported_interfaces.len(), 1);
    }

    #[test]
    fn published_agent_card_is_spec_shaped() {
        let config = config_with_published_alias("researcher", true);
        let card = build_agent_card(&config, "researcher").expect("card");
        assert_eq!(card.name, "researcher");
        assert_eq!(card.supported_interfaces.len(), 1);
        assert_eq!(
            card.supported_interfaces[0].protocol_binding,
            A2A_PROTOCOL_BINDING
        );
        // empty exposed_skills filter advertises no skills
        assert!(card.skills.is_empty());
    }

    #[test]
    fn public_base_url_overrides_derived_endpoint() {
        let mut config = config_with_published_alias("researcher", true);
        config.a2a.server.public_base_url = "https://agents.example.com/".into();
        let card = build_catalog_card(&config);
        assert_eq!(
            card.supported_interfaces[0].url,
            "https://agents.example.com/.well-known/agents-card.json"
        );
    }

    #[test]
    fn endpoints_derive_from_gateway_port_when_unset() {
        let mut config = config_with_published_alias("researcher", true);
        config.gateway.host = "127.0.0.1".into();
        config.gateway.port = 42617;
        // no A2A bind/port override, no public_base_url
        let card = build_catalog_card(&config);
        assert_eq!(
            card.supported_interfaces[0].url,
            "http://127.0.0.1:42617/.well-known/agents-card.json"
        );
    }

    #[test]
    fn runtime_gateway_endpoint_supersedes_config_gateway_port_when_unset() {
        let mut config = config_with_published_alias("researcher", true);
        config.gateway.host = "127.0.0.1".into();
        config.gateway.port = 42617;
        let endpoint = AdvertisedGatewayEndpoint::new("127.0.0.1", 42629);

        let catalog = build_catalog_card_with_endpoint(&config, Some(&endpoint));
        assert_eq!(
            catalog.supported_interfaces[0].url,
            "http://127.0.0.1:42629/.well-known/agents-card.json"
        );
        assert_eq!(
            catalog.supported_interfaces[1].url,
            "http://127.0.0.1:42629/a2a/researcher"
        );

        let agent = build_agent_card_with_endpoint(&config, "researcher", Some(&endpoint))
            .expect("published agent card");
        assert_eq!(
            agent.supported_interfaces[0].url,
            "http://127.0.0.1:42629/a2a/researcher"
        );
    }

    #[test]
    fn public_base_url_overrides_runtime_gateway_endpoint() {
        let mut config = config_with_published_alias("researcher", true);
        config.a2a.server.public_base_url = "https://agents.example.com/".into();
        let endpoint = AdvertisedGatewayEndpoint::new("127.0.0.1", 42629);

        let card = build_catalog_card_with_endpoint(&config, Some(&endpoint));
        assert_eq!(
            card.supported_interfaces[0].url,
            "https://agents.example.com/.well-known/agents-card.json"
        );
    }

    #[test]
    fn a2a_port_override_supersedes_gateway_port() {
        let mut config = config_with_published_alias("researcher", true);
        config.gateway.host = "127.0.0.1".into();
        config.gateway.port = 42617;
        config.a2a.server.bind = Some("0.0.0.0".into());
        config.a2a.server.port = Some(9000);
        let endpoint = AdvertisedGatewayEndpoint::new("127.0.0.1", 42629);
        let card = build_catalog_card_with_endpoint(&config, Some(&endpoint));
        assert_eq!(
            card.supported_interfaces[0].url,
            "http://0.0.0.0:9000/.well-known/agents-card.json"
        );
    }

    #[test]
    fn card_serializes_to_camelcase_wire_shape() {
        let config = config_with_published_alias("researcher", true);
        let card = build_agent_card(&config, "researcher").expect("card");
        let json = serde_json::to_value(&card).expect("serialize");
        assert!(json.get("supportedInterfaces").is_some());
        assert!(json.get("defaultInputModes").is_some());
        assert!(json.get("defaultOutputModes").is_some());
        // snake_case must not leak into the wire shape
        assert!(json.get("supported_interfaces").is_none());
    }

    #[test]
    fn empty_tags_are_omitted_from_skill_wire_shape() {
        let skill = AgentSkill {
            id: "x".to_string(),
            name: "X".to_string(),
            description: "d".to_string(),
            tags: Vec::new(),
        };
        let json = serde_json::to_value(&skill).expect("serialize");
        assert!(json.get("tags").is_none());
    }

    #[test]
    fn catalog_routes_serve_root_and_prefixed_paths() {
        assert_eq!(CATALOG_CARD_PATH, "/.well-known/agents-card.json");
        assert_eq!(
            CATALOG_CARD_PREFIXED_PATH,
            "/a2a/.well-known/agents-card.json"
        );
    }

    #[test]
    fn message_send_params_parse_text_parts() {
        let value = serde_json::json!({
            "message": {
                "parts": [
                    {"kind": "text", "text": "hello"},
                    {"kind": "text", "text": "world"},
                    {"kind": "data", "data": {"x": 1}}
                ]
            }
        });
        let params: MessageSendParams = serde_json::from_value(value).expect("parse");
        let prompt = params
            .message
            .parts
            .iter()
            .filter(|p| p.kind == "text")
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(prompt, "hello\nworld");
    }

    #[test]
    fn out_task_serializes_to_camelcase_wire_shape() {
        let task = Task {
            id: "task-1".to_string(),
            context_id: "ctx-1".to_string(),
            status: TaskStatus {
                state: "completed".to_string(),
            },
            artifacts: vec![Artifact {
                artifact_id: "art-1".to_string(),
                parts: vec![TextPart {
                    kind: "text".to_string(),
                    text: "done".to_string(),
                }],
            }],
            kind: "task".to_string(),
        };
        let json = serde_json::to_value(&task).expect("serialize");
        assert_eq!(json["contextId"], "ctx-1");
        assert_eq!(json["status"]["state"], "completed");
        assert_eq!(json["artifacts"][0]["artifactId"], "art-1");
        assert_eq!(json["artifacts"][0]["parts"][0]["kind"], "text");
        assert!(json.get("context_id").is_none());
    }

    #[test]
    fn jsonrpc_error_carries_code_and_id() {
        let err = jsonrpc_error(serde_json::json!(7), -32601, "Method not found");
        assert_eq!(err["jsonrpc"], "2.0");
        assert_eq!(err["id"], 7);
        assert_eq!(err["error"]["code"], -32601);
        assert_eq!(err["error"]["message"], "Method not found");
    }

    #[test]
    fn card_description_falls_back_to_neutral_default_without_identity() {
        let config = config_with_published_alias("researcher", true);
        let card = build_agent_card(&config, "researcher").expect("card");
        assert_eq!(card.description, "ZeroClaw agent 'researcher'.");
    }

    #[test]
    fn card_description_reads_identity_bio_when_configured() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(
            tmp.path().join("identity.json"),
            r#"{ "identity": { "names": { "first": "Nova" }, "bio": "Curates research and cites sources." } }"#,
        )
        .expect("write identity");

        let mut config = config_with_published_alias("researcher", true);
        {
            let agent = config.agents.get_mut("researcher").unwrap();
            agent.workspace.path = Some(tmp.path().to_path_buf());
            agent.identity.format = "aieos".to_string();
            agent.identity.aieos_path = Some("identity.json".to_string());
        }

        let card = build_agent_card(&config, "researcher").expect("card");
        assert_eq!(card.description, "Curates research and cites sources.");
    }

    #[test]
    fn card_description_uses_name_line_when_bio_absent() {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(
            tmp.path().join("identity.json"),
            r#"{ "identity": { "names": { "full": "Nova the Researcher" } } }"#,
        )
        .expect("write identity");

        let mut config = config_with_published_alias("researcher", true);
        {
            let agent = config.agents.get_mut("researcher").unwrap();
            agent.workspace.path = Some(tmp.path().to_path_buf());
            agent.identity.format = "aieos".to_string();
            agent.identity.aieos_path = Some("identity.json".to_string());
        }

        let card = build_agent_card(&config, "researcher").expect("card");
        assert_eq!(card.description, "Nova the Researcher");
    }
}
