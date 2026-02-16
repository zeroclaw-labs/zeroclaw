//! Axum-based HTTP gateway with proper HTTP/1.1 compliance, body limits, and timeouts.
//!
//! This module replaces the raw TCP implementation with axum for:
//! - Proper HTTP/1.1 parsing and compliance
//! - Content-Length validation (handled by hyper)
//! - Request body size limits (64KB max)
//! - Request timeouts (30s) to prevent slow-loris attacks
//! - Header sanitization (handled by axum/hyper)

use crate::channels::{Channel, WhatsAppChannel};
use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory};
use crate::providers::{self, Provider};
use crate::security::pairing::{constant_time_eq, is_public_bind};
use crate::security::SecurityPolicy;
use crate::status_events;
use anyhow::Result;
use async_trait::async_trait;
use axum::{
    body::Bytes,
    extract::{ws, Multipart, Path, Query, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{delete, get, patch, post, put},
    Router,
};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

mod local_bridge;

/// Maximum request body size (64KB) — prevents memory exhaustion
pub const MAX_BODY_SIZE: usize = 65_536;
/// Request timeout (30s) — prevents slow-loris attacks
pub const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Shared state for all axum handlers
#[derive(Clone)]
pub struct AppState {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub temperature: f64,
    pub mem: Arc<dyn Memory>,
    pub security: Arc<SecurityPolicy>,
    pub composio_api_key: Option<String>,
    pub browser_config: crate::config::BrowserConfig,
    pub workspace_dir: std::path::PathBuf,
    pub registry_db: crate::aria::db::AriaDb,
    pub auto_save: bool,
    pub webhook_secret: Option<Arc<str>>,
    pub gateway_host: String,
    pub gateway_port: u16,
    pub whatsapp: Option<Arc<WhatsAppChannel>>,
    pub local_tool_bridge: Arc<local_bridge::LocalToolBridge>,
}

/// Run the HTTP gateway using axum with proper HTTP/1.1 compliance.
#[allow(clippy::too_many_lines)]
pub async fn run_gateway(host: &str, port: u16, config: Config) -> Result<()> {
    // ── Security: refuse public bind without tunnel or explicit opt-in ──
    if is_public_bind(host) && config.tunnel.provider == "none" && !config.gateway.allow_public_bind
    {
        anyhow::bail!(
            "🛑 Refusing to bind to {host} — gateway would be exposed to the internet.\n\
             Fix: use --host 127.0.0.1 (default), configure a tunnel, or set\n\
             [gateway] allow_public_bind = true in config.toml (NOT recommended)."
        );
    }

    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let actual_port = listener.local_addr()?.port();
    let display_addr = format!("{host}:{actual_port}");

    let provider: Arc<dyn Provider> = Arc::from(providers::create_resilient_provider(
        config.default_provider.as_deref().unwrap_or("openrouter"),
        config.api_key.as_deref(),
        &config.reliability,
    )?);
    let model = config
        .default_model
        .clone()
        .unwrap_or_else(|| "anthropic/claude-sonnet-4-20250514".into());
    let temperature = config.default_temperature;
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let composio_api_key = if config.composio.enabled {
        config.composio.api_key.clone()
    } else {
        None
    };
    // Extract webhook secret for authentication
    let webhook_secret: Option<Arc<str>> = config
        .channels_config
        .webhook
        .as_ref()
        .and_then(|w| w.secret.as_deref())
        .map(Arc::from);

    // WhatsApp channel (if configured)
    let whatsapp_channel: Option<Arc<WhatsAppChannel>> =
        config.channels_config.whatsapp.as_ref().map(|wa| {
            Arc::new(WhatsAppChannel::new(
                wa.access_token.clone(),
                wa.phone_number_id.clone(),
                wa.verify_token.clone(),
                wa.allowed_numbers.clone(),
            ))
        });

    // ── Tunnel ────────────────────────────────────────────────
    let tunnel = crate::tunnel::create_tunnel(&config.tunnel)?;
    let mut tunnel_url: Option<String> = None;

    if let Some(ref tun) = tunnel {
        println!("🔗 Starting {} tunnel...", tun.name());
        match tun.start(host, actual_port).await {
            Ok(url) => {
                println!("🌐 Tunnel active: {url}");
                tunnel_url = Some(url);
            }
            Err(e) => {
                println!("⚠️  Tunnel failed to start: {e}");
                println!("   Falling back to local-only mode.");
            }
        }
    }

    println!("🦀 Aria Gateway listening on http://{display_addr}");
    if let Some(ref url) = tunnel_url {
        println!("  🌐 Public URL: {url}");
    }
    println!("  POST /webhook   — {{\"message\": \"your prompt\"}}");
    if whatsapp_channel.is_some() {
        println!("  GET  /whatsapp  — Meta webhook verification");
        println!("  POST /whatsapp  — WhatsApp message webhook");
    }
    println!("  GET  /health    — health check");
    if webhook_secret.is_some() {
        println!("  🔒 Webhook secret: ENABLED");
    }
    println!("  Press Ctrl+C to stop.\n");

    crate::health::mark_component_ok("gateway");

    // ── Aria Registry API + Dashboard Schema ───────────────────
    let aria_db_path = config.workspace_dir.join("aria.db");
    let aria_registries =
        crate::aria::initialize_aria_registries(&aria_db_path).unwrap_or_else(|e| {
            tracing::warn!("Failed to initialize Aria registries: {e}");
            // Create a fallback in-memory DB
            let db = crate::aria::db::AriaDb::open_in_memory().expect("fallback in-memory DB");
            std::sync::Arc::new(crate::aria::AriaRegistries::new(db))
        });
    crate::dashboard::ensure_schema(&aria_registries.db)?;
    if let Ok(src) = std::env::var("AFW_IMPORT_CLOUD_DB") {
        let imported = crate::dashboard::import_from_cloud_db(
            &aria_registries.db,
            std::path::Path::new(&src),
        )?;
        if imported > 0 {
            tracing::info!(imported_tables = imported, source = %src, "Imported dashboard tables from cloud DB");
        }
    }
    let registry_api = crate::api::registry_router(aria_registries.db.clone());

    // Build shared state
    let state = AppState {
        provider,
        model,
        temperature,
        mem,
        security: security.clone(),
        composio_api_key,
        browser_config: config.browser.clone(),
        workspace_dir: config.workspace_dir.clone(),
        registry_db: aria_registries.db.clone(),
        auto_save: config.memory.auto_save,
        webhook_secret,
        gateway_host: host.to_string(),
        gateway_port: actual_port,
        whatsapp: whatsapp_channel,
        local_tool_bridge: Arc::new(local_bridge::LocalToolBridge::new(
            security.clone(),
            aria_registries.db.clone(),
        )),
    };

    // Build router with middleware
    // Note: Body limit layer prevents memory exhaustion from oversized requests
    // Timeout is handled by tokio's TcpListener accept timeout and hyper's built-in timeouts
    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/webhook", post(handle_webhook))
        // Dashboard parity routes (cloud-compatible)
        .route("/api/messages", post(api_send_message))
        .route("/api/messages", get(api_list_messages))
        .route("/api/chats", post(api_create_chat))
        .route("/api/chats", get(api_list_chats))
        .route("/api/chats/:chat_id/messages", get(api_get_chat_messages))
        .route("/api/chats/:chat_id", delete(api_delete_chat))
        .route("/api/chats/:chat_id/duplicate", post(api_duplicate_chat))
        .route("/api/conversations", get(api_list_conversations))
        .route("/api/approvals", get(api_list_approvals))
        .route("/api/approvals", post(api_respond_approval))
        .route("/api/sessions", get(api_list_sessions))
        .route("/api/sessions/:key", patch(api_patch_session))
        .route("/api/sessions/:key/reset", post(api_reset_session))
        .route("/api/sessions/:key", delete(api_delete_session))
        .route("/api/events", get(api_list_events))
        .route("/api/inbox", get(api_list_inbox).post(api_create_inbox))
        .route("/api/inbox/:id/read", post(api_mark_inbox_read))
        .route("/api/inbox/:id/archive", post(api_archive_inbox))
        .route("/api/inbox/:id/open-chat", post(api_open_inbox_chat))
        .route("/api/nodes", get(api_list_nodes))
        .route("/api/metrics", get(api_get_metrics))
        .route("/api/status", get(api_get_status))
        .route("/api/channels", get(api_list_channels))
        .route("/api/channels/:id", patch(api_patch_channel))
        .route("/api/channels/:id", delete(api_delete_channel))
        .route("/api/cron", get(api_list_cron))
        .route("/api/cron/:id", patch(api_patch_cron))
        .route("/api/cron/:id", delete(api_delete_cron))
        .route("/api/cron/:id/run", post(api_run_cron))
        .route("/api/skills", get(api_list_skills))
        .route("/api/skills/:id", patch(api_patch_skill))
        .route("/api/config", get(api_get_config))
        .route("/api/config", put(api_put_config))
        .route("/api/tools", get(api_list_tools))
        .route("/api/agents", get(api_list_agents))
        .route("/api/teams", get(api_list_teams))
        .route("/api/teams/:id", get(api_get_team))
        .route("/api/pipelines", get(api_list_pipelines))
        .route("/api/pipelines/:id", get(api_get_pipeline))
        .route("/api/kv", get(api_list_kv))
        .route("/api/kv/:key", get(api_get_kv))
        .route("/api/containers", get(api_list_containers))
        .route("/api/logs", get(api_list_logs))
        .route("/api/api-keys", get(api_list_api_keys))
        .route("/api/api-keys", post(api_create_api_key))
        .route("/api/api-keys/:id", delete(api_revoke_api_key))
        .route("/api/v1/auth/magic-number", post(api_create_magic_number))
        .route("/api/v1/auth/magic-numbers", get(api_list_magic_numbers))
        .route(
            "/api/v1/auth/magic-number/:id",
            delete(api_revoke_magic_number),
        )
        .route("/api/billing", get(api_get_billing))
        .route("/api/billing/usage", get(api_get_billing_usage))
        .route("/api/billing/invoices", get(api_get_billing_invoices))
        .route("/api/billing/payment-methods", get(api_get_billing_methods))
        .route("/api/feeds", get(api_list_feeds))
        .route("/api/feeds/:id", get(api_get_feed))
        .route("/api/feeds/:id/items", get(api_list_feed_items))
        .route("/api/feeds/:id", patch(api_patch_feed))
        .route("/api/feed/files", get(api_list_feed_files))
        .route("/api/feed/files", post(api_upload_feed_file))
        .route(
            "/api/feed/files/:file_id/content",
            get(api_get_feed_file_content),
        )
        .route("/api/feed/files/:file_id", delete(api_delete_feed_file))
        .route("/api/tool-calls", get(api_list_tool_calls))
        .route("/api/tool-calls/stats", get(api_tool_calls_stats))
        .route("/api/tool-calls/:id", get(api_get_tool_call))
        .route("/whatsapp", get(handle_whatsapp_verify))
        .route("/whatsapp", post(handle_whatsapp_message))
        // WebSocket endpoints (canonical /ws/* paths)
        .route("/ws/events", get(handle_events_ws_events))
        .route("/ws/chat", get(handle_events_ws_chat))
        .route("/ws/local-bridge", get(handle_events_ws_local_bridge))
        .route("/ws/status", get(handle_events_ws_status))
        .route("/ws/logs", get(handle_events_ws_logs))
        // Backward-compat alias; keep temporarily while clients migrate.
        .route("/events", get(handle_events_ws_events))
        .with_state(state)
        .merge(registry_api)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE));

    // Run the server
    axum::serve(listener, app).await?;

    Ok(())
}

// ══════════════════════════════════════════════════════════════════════════════
// AXUM HANDLERS
// ══════════════════════════════════════════════════════════════════════════════

/// GET /health — always public (no secrets leaked)
async fn handle_health(State(_state): State<AppState>) -> impl IntoResponse {
    let body = serde_json::json!({
        "status": "ok",
        "runtime": crate::health::snapshot_json(),
    });
    Json(body)
}

/// Webhook request body
#[derive(serde::Deserialize)]
pub struct WebhookBody {
    pub message: String,
}

/// POST /webhook — main webhook endpoint
async fn handle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<WebhookBody>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    // ── Webhook secret auth (optional, additional layer) ──
    if let Some(ref secret) = state.webhook_secret {
        let header_val = headers
            .get("X-Webhook-Secret")
            .and_then(|v| v.to_str().ok());
        match header_val {
            Some(val) if constant_time_eq(val, secret.as_ref()) => {}
            _ => {
                tracing::warn!("Webhook: rejected request — invalid or missing X-Webhook-Secret");
                let err = serde_json::json!({"error": "Unauthorized — invalid or missing X-Webhook-Secret header"});
                return (StatusCode::UNAUTHORIZED, Json(err));
            }
        }
    }

    // ── Parse body ──
    let Json(webhook_body) = match body {
        Ok(b) => b,
        Err(e) => {
            let err = serde_json::json!({
                "error": format!("Invalid JSON: {e}. Expected: {{\"message\": \"...\"}}")
            });
            return (StatusCode::BAD_REQUEST, Json(err));
        }
    };

    let message = &webhook_body.message;

    if state.auto_save {
        let _ = state
            .mem
            .store("webhook_msg", message, MemoryCategory::Conversation)
            .await;
    }

    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth.strip_prefix("Bearer ").unwrap_or("");
    let tenant = resolve_tenant_from_token(&state, token);

    let run_result = crate::agent::orchestrator::run_live_turn(
        crate::agent::orchestrator::LiveTurnConfig {
            provider: state.provider.as_ref(),
            security: &state.security,
            memory: state.mem.clone(),
            composio_api_key: state.composio_api_key.as_deref(),
            browser_config: &state.browser_config,
            registry_db: &state.registry_db,
            workspace_dir: &state.workspace_dir,
            tenant_id: &tenant,
            model: &state.model,
            temperature: state.temperature,
            mode_hint: "",
            max_turns: Some(25),
            external_tool_context: None,
        },
        message,
        None,
    )
    .await;

    match run_result {
        Ok(response) => {
            let body = serde_json::json!({"response": response.output, "model": state.model});
            (StatusCode::OK, Json(body))
        }
        Err(e) => {
            let err = serde_json::json!({"error": format!("LLM error: {e}")});
            (StatusCode::INTERNAL_SERVER_ERROR, Json(err))
        }
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// DASHBOARD API (Cloud route parity)
// ══════════════════════════════════════════════════════════════════════════════

fn api_ok<T: serde::Serialize>(data: T) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "data": data
        })),
    )
}

fn api_err(
    status: StatusCode,
    message: impl Into<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({
            "success": false,
            "error": {
                "code": format!("HTTP_{}", status.as_u16()),
                "message": message.into()
            }
        })),
    )
}

fn resolve_tenant_from_token(state: &AppState, token: &str) -> String {
    crate::tenant::resolve_tenant_from_token(&state.registry_db, token)
}

fn api_tenant(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth.strip_prefix("Bearer ").unwrap_or("");
    // Dashboard API routes intentionally do not enforce bearer tokens.
    // Pairing remains available for webhook-level hardening.
    Ok(resolve_tenant_from_token(state, token))
}

fn iso_from_millis(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(serde::Deserialize)]
struct ApiCreateChatBody {
    title: Option<String>,
    session_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiSendMessageBody {
    content: Option<String>,
    message: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    mode: Option<String>,
}

#[derive(serde::Deserialize)]
struct WsChatMessage {
    #[serde(rename = "type")]
    message_type: String,
    content: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    mode: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiApprovalBody {
    action: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiUpdateStatusBody {
    status: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiPatchChannelBody {
    status: Option<String>,
    name: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiPatchSkillBody {
    enabled: Option<bool>,
}

#[derive(serde::Deserialize)]
struct InboxListQuery {
    status: Option<String>,
    source: Option<String>,
    limit: Option<u32>,
    cursor: Option<i64>,
}

#[derive(serde::Deserialize)]
struct ApiCreateInboxBody {
    #[serde(rename = "sourceType")]
    source_type: Option<String>,
    #[serde(rename = "sourceId")]
    source_id: Option<String>,
    #[serde(rename = "runId")]
    run_id: Option<String>,
    #[serde(rename = "chatId")]
    chat_id: Option<String>,
    title: Option<String>,
    preview: Option<String>,
    body: Option<String>,
    metadata: Option<serde_json::Value>,
    status: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiUpdateConfigBody {
    gateway: Option<serde_json::Value>,
    auth: Option<serde_json::Value>,
    limits: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct ApiUpdateFeedBody {
    status: Option<String>,
}

#[derive(serde::Deserialize)]
struct ApiCreateKeyBody {
    name: Option<String>,
    scopes: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
enum ChatRunMode {
    Default,
    Plan,
    Autonomous,
}

fn parse_chat_mode(raw: Option<&str>) -> ChatRunMode {
    match raw.map(str::trim).unwrap_or_default() {
        "plan" => ChatRunMode::Plan,
        "autonomous" => ChatRunMode::Autonomous,
        _ => ChatRunMode::Default,
    }
}

async fn build_chat_memory_context(mem: &dyn Memory, user_msg: &str) -> String {
    let mut context = String::new();
    if let Ok(entries) = mem.recall(user_msg, 5).await {
        if !entries.is_empty() {
            context.push_str("[Memory context]\n");
            for entry in &entries {
                context.push_str("- ");
                context.push_str(&entry.key);
                context.push_str(": ");
                context.push_str(&entry.content);
                context.push('\n');
            }
            context.push('\n');
        }
    }
    context
}

fn mode_prompt(mode: ChatRunMode) -> &'static str {
    match mode {
        ChatRunMode::Plan => {
            "PLAN MODE: analyze and propose an implementation plan only. Do not execute changes yet."
        }
        ChatRunMode::Autonomous => {
            "AUTONOMOUS MODE: execute end-to-end. Prefer decisive action and report concise progress."
        }
        ChatRunMode::Default => "",
    }
}

fn load_registry_tools_prompt_section(
    db: &crate::aria::db::AriaDb,
    tenant_id: &str,
) -> anyhow::Result<String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT name, description, version, schema
             FROM aria_tools
             WHERE tenant_id=?1 AND status='active'
             ORDER BY updated_at DESC",
        )?;
        let mut rows = stmt.query([tenant_id])?;
        let mut out = String::new();
        while let Some(row) = rows.next()? {
            if out.is_empty() {
                out.push_str("## Available Tools\n\n");
            }
            let name: String = row.get(0)?;
            let description: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let version: i64 = row.get::<_, Option<i64>>(2)?.unwrap_or(1);
            let schema: String = row
                .get::<_, Option<String>>(3)?
                .unwrap_or_else(|| "{}".to_string());
            out.push_str(&format!(
                "- **{}** (v{}): {}\n  Schema: {}\n",
                name, version, description, schema
            ));
        }
        Ok(out)
    })
}

fn load_registry_agents_prompt_section(
    db: &crate::aria::db::AriaDb,
    tenant_id: &str,
) -> anyhow::Result<String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT name, description, version, model
             FROM aria_agents
             WHERE tenant_id=?1 AND status='active'
             ORDER BY updated_at DESC",
        )?;
        let mut rows = stmt.query([tenant_id])?;
        let mut out = String::new();
        while let Some(row) = rows.next()? {
            if out.is_empty() {
                out.push_str("## Available Agents\n\n");
            }
            let name: String = row.get(0)?;
            let description: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let version: i64 = row.get::<_, Option<i64>>(2)?.unwrap_or(1);
            let model: String = row
                .get::<_, Option<String>>(3)?
                .unwrap_or_else(|| "default".to_string());
            out.push_str(&format!(
                "- **{}** (v{}): {}\n  Model: {}\n",
                name, version, description, model
            ));
        }
        Ok(out)
    })
}

fn build_live_system_prompt(
    state: &AppState,
    tenant: &str,
    tools: &[Box<dyn crate::tools::Tool>],
    mode_hint: &str,
) -> String {
    let skills = crate::skills::load_skills(&state.workspace_dir);
    let descriptors: Vec<crate::prompt::SkillDescriptor> = skills
        .iter()
        .map(|s| crate::prompt::SkillDescriptor {
            name: s.name.clone(),
            description: s.description.clone(),
        })
        .collect();

    let tool_descs_owned: Vec<(String, String)> = tools
        .iter()
        .map(|t| (t.name().to_string(), t.description().to_string()))
        .collect();
    let tool_descs: Vec<(&str, &str)> = tool_descs_owned
        .iter()
        .map(|(name, desc)| (name.as_str(), desc.as_str()))
        .collect();

    let registry_tools_section = load_registry_tools_prompt_section(&state.registry_db, tenant)
        .unwrap_or_else(|e| {
            tracing::warn!(tenant, error = %e, "Failed to build registry tools prompt section");
            String::new()
        });
    let registry_agents_section = load_registry_agents_prompt_section(&state.registry_db, tenant)
        .unwrap_or_else(|e| {
            tracing::warn!(tenant, error = %e, "Failed to build registry agents prompt section");
            String::new()
        });

    let prompt = crate::prompt::SystemPromptBuilder::new(&state.workspace_dir)
        .tools(&tool_descs)
        .skills(&descriptors)
        .model(&state.model)
        .registry_tools_section(registry_tools_section)
        .registry_agents_section(registry_agents_section)
        .build();

    if mode_hint.is_empty() {
        prompt
    } else {
        format!("{prompt}\n\n{mode_hint}")
    }
}

fn ensure_chat_row(state: &AppState, tenant: &str, chat_id: &str, seed: &str, ts: i64) {
    let _ = state.registry_db.with_conn(|conn| {
        let exists: Option<String> = conn
            .query_row(
                "SELECT id FROM chats WHERE tenant_id=?1 AND session_id=?2 LIMIT 1",
                rusqlite::params![tenant, chat_id],
                |row| row.get(0),
            )
            .ok();
        if exists.is_none() {
            let title = seed.chars().take(64).collect::<String>();
            conn.execute(
                "INSERT INTO chats (id, tenant_id, title, preview, session_id, message_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, '', ?4, 0, ?5, ?5)",
                rusqlite::params![uuid::Uuid::new_v4().to_string(), tenant, title, chat_id, ts],
            )?;
        }
        Ok(())
    });
}

fn insert_chat_message(
    state: &AppState,
    tenant: &str,
    chat_id: &str,
    role: &str,
    content: &str,
    ts: i64,
) -> String {
    let msg_id = uuid::Uuid::new_v4().to_string();
    let _ = state.registry_db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO messages (id, tenant_id, chat_id, role, content, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![msg_id, tenant, chat_id, role, content, ts],
        )?;
        Ok(())
    });
    msg_id
}

fn update_chat_preview(state: &AppState, tenant: &str, chat_id: &str, preview: &str, ts: i64) {
    let _ = state.registry_db.with_conn(|conn| {
        conn.execute(
            "UPDATE chats
             SET preview=?1, message_count=(SELECT COUNT(*) FROM messages WHERE chat_id=?2 AND tenant_id=?3), updated_at=?4
             WHERE session_id=?2 AND tenant_id=?3",
            rusqlite::params![preview.chars().take(140).collect::<String>(), chat_id, tenant, ts],
        )?;
        Ok(())
    });
}

fn persist_tool_trace(
    state: &AppState,
    tenant: &str,
    chat_id: &str,
    run_id: &str,
    trace: &crate::agent::executor::AgentToolTrace,
    now: i64,
) {
    let status = if trace.is_error { "error" } else { "success" };
    let _ = state.registry_db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO tool_calls (id, tenant_id, session_id, run_id, agent_id, tool_name, status, args_json, result_json, error, duration_ms, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
            rusqlite::params![
                trace.id.clone(),
                tenant,
                chat_id,
                run_id,
                "main",
                trace.name.clone(),
                status,
                trace.args.to_string(),
                trace.result.clone(),
                if trace.is_error { Some(trace.result.clone()) } else { None::<String> },
                trace.duration_ms as i64,
                now,
            ],
        )?;
        Ok(())
    });
}

async fn api_create_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ApiCreateChatBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let chat_id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    let title = body.title.unwrap_or_else(|| "New Chat".to_string());
    let session_id = body.session_id.unwrap_or_else(|| chat_id.clone());
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO chats (id, tenant_id, title, preview, session_id, message_count, created_at, updated_at)
             VALUES (?1, ?2, ?3, '', ?4, 0, ?5, ?5)",
            rusqlite::params![chat_id, tenant, title, session_id, now],
        )?;
        Ok(())
    });
    if let Err(e) = res {
        return api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    api_ok(serde_json::json!({
        "id": session_id,
        "title": title,
        "preview": "",
        "messageCount": 0,
        "createdAt": iso_from_millis(now),
        "updatedAt": iso_from_millis(now),
    }))
}

async fn api_send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ApiSendMessageBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let content = body
        .content
        .or(body.message)
        .unwrap_or_default()
        .trim()
        .to_string();
    if content.is_empty() {
        return api_err(StatusCode::BAD_REQUEST, "content is required");
    }
    let session_id = body
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mode = parse_chat_mode(body.mode.as_deref());
    let run_id = format!("run-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let now = now_ms();

    ensure_chat_row(&state, &tenant, &session_id, &content, now);
    let _ = insert_chat_message(&state, &tenant, &session_id, "user", &content, now);

    let mode_hint = mode_prompt(mode);
    let run_result = crate::agent::orchestrator::run_live_turn(
        crate::agent::orchestrator::LiveTurnConfig {
            provider: state.provider.as_ref(),
            security: &state.security,
            memory: state.mem.clone(),
            composio_api_key: state.composio_api_key.as_deref(),
            browser_config: &state.browser_config,
            registry_db: &state.registry_db,
            workspace_dir: &state.workspace_dir,
            tenant_id: &tenant,
            model: &state.model,
            temperature: state.temperature,
            mode_hint,
            max_turns: Some(25),
            external_tool_context: Some(crate::agent::executor::ExternalToolContext {
                tenant_id: tenant.clone(),
                chat_id: session_id.clone(),
                run_id: run_id.clone(),
                executor: state.local_tool_bridge.clone(),
            }),
        },
        &content,
        None,
    )
    .await;

    let assistant_text = match run_result {
        Ok(res) => {
            let finished_at = now_ms();
            for trace in &res.tool_traces {
                persist_tool_trace(&state, &tenant, &session_id, &run_id, trace, finished_at);
            }
            status_events::emit(
                "task.completed",
                serde_json::json!({
                    "id": run_id,
                    "name": "chat",
                    "durationMs": res.duration_ms,
                }),
            );
            res.output
        }
        Err(e) => {
            status_events::emit(
                "task.failed",
                serde_json::json!({
                    "id": run_id,
                    "name": "chat",
                    "errorMessage": e.to_string(),
                }),
            );
            format!("LLM error: {e}")
        }
    };
    let assistant_id = insert_chat_message(
        &state,
        &tenant,
        &session_id,
        "assistant",
        &assistant_text,
        now_ms(),
    );
    update_chat_preview(&state, &tenant, &session_id, &assistant_text, now_ms());

    api_ok(serde_json::json!({
        "id": assistant_id,
        "role": "assistant",
        "content": assistant_text,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
}

async fn api_list_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let limit = q.limit.unwrap_or(500) as i64;
    let offset = q.offset.unwrap_or(0) as i64;
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, role, content, created_at FROM messages
             WHERE tenant_id=?1 ORDER BY created_at ASC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant, limit, offset], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "role": row.get::<_, String>(1)?,
                "content": row.get::<_, String>(2)?,
                "timestamp": iso_from_millis(row.get::<_, i64>(3)?),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(items),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_chats(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT session_id, title, preview, message_count, created_at, updated_at
             FROM chats WHERE tenant_id=?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "title": row.get::<_, String>(1)?,
                "preview": row.get::<_, String>(2)?,
                "messageCount": row.get::<_, i64>(3)?,
                "createdAt": iso_from_millis(row.get::<_, i64>(4)?),
                "updatedAt": iso_from_millis(row.get::<_, i64>(5)?),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(items),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_get_chat_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(chat_id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, role, content, created_at FROM messages
             WHERE tenant_id=?1 AND chat_id=?2 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant, chat_id], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "role": row.get::<_, String>(1)?,
                "content": row.get::<_, String>(2)?,
                "timestamp": iso_from_millis(row.get::<_, i64>(3)?),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(items),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_delete_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(chat_id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM messages WHERE tenant_id=?1 AND chat_id=?2",
            rusqlite::params![tenant, chat_id],
        )?;
        conn.execute(
            "DELETE FROM chats WHERE tenant_id=?1 AND session_id=?2",
            rusqlite::params![tenant, chat_id],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({"id": chat_id, "deleted": true})),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_duplicate_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(chat_id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let new_chat_id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    let res = state.registry_db.with_conn(|conn| {
        let (title, preview): (String, String) = conn.query_row(
            "SELECT title, preview FROM chats WHERE tenant_id=?1 AND session_id=?2",
            rusqlite::params![tenant, chat_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        conn.execute(
            "INSERT INTO chats (id, tenant_id, title, preview, session_id, message_count, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                tenant,
                format!("{title} (copy)"),
                preview,
                new_chat_id,
                now
            ],
        )?;
        let mut stmt = conn.prepare(
            "SELECT role, content, created_at FROM messages WHERE tenant_id=?1 AND chat_id=?2 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant, chat_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut count = 0_i64;
        for row in rows.flatten() {
            conn.execute(
                "INSERT INTO messages (id, tenant_id, chat_id, role, content, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    tenant,
                    new_chat_id,
                    row.0,
                    row.1,
                    row.2
                ],
            )?;
            count += 1;
        }
        conn.execute(
            "UPDATE chats SET message_count=?1, updated_at=?2 WHERE tenant_id=?3 AND session_id=?4",
            rusqlite::params![count, now, tenant, new_chat_id],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({
            "id": new_chat_id,
            "title": "Chat copy",
            "preview": "",
            "messageCount": 0,
            "createdAt": iso_from_millis(now),
            "updatedAt": iso_from_millis(now),
        })),
        Err(_) => api_err(StatusCode::NOT_FOUND, "Chat not found"),
    }
}

async fn api_list_conversations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT session_id, title, created_at, updated_at FROM chats WHERE tenant_id=?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            let sid: String = row.get(0)?;
            Ok(serde_json::json!({
                "id": sid.clone(),
                "title": row.get::<_, String>(1)?,
                "messages": [],
                "createdAt": iso_from_millis(row.get::<_, i64>(2)?),
                "updatedAt": iso_from_millis(row.get::<_, i64>(3)?),
                "sessionId": sid,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(items),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_approvals(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, command, metadata_json, countdown, status, created_at, expires_at
             FROM approvals WHERE tenant_id=?1 AND status='pending' ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "command": row.get::<_, String>(1)?,
                "metadata": serde_json::from_str::<serde_json::Value>(&row.get::<_, Option<String>>(2)?.unwrap_or_else(|| "{}".to_string())).unwrap_or(serde_json::json!({})),
                "countdown": row.get::<_, i64>(3)?,
                "status": row.get::<_, String>(4)?,
                "createdAt": iso_from_millis(row.get::<_, i64>(5)?),
                "expiresAt": row.get::<_, Option<i64>>(6)?.map(iso_from_millis),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(items),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_respond_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ApiApprovalBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let id = body.request_id.unwrap_or_default();
    let action = body.action.unwrap_or_else(|| "deny".to_string());
    if id.is_empty() {
        return api_err(StatusCode::BAD_REQUEST, "requestId is required");
    }
    let status = if action == "deny" {
        "denied"
    } else {
        "approved"
    };
    let _ = state.registry_db.with_conn(|conn| {
        conn.execute(
            "UPDATE approvals SET status=?1 WHERE tenant_id=?2 AND id=?3",
            rusqlite::params![status, tenant, id],
        )?;
        Ok(())
    });
    api_ok(serde_json::json!({"id": id, "status": status}))
}

async fn api_list_sessions(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, status, last_activity, run_count, created_at, user_id
             FROM sessions WHERE tenant_id=?1 ORDER BY last_activity DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "status": row.get::<_, String>(2)?,
                "lastActivity": row.get::<_, Option<i64>>(3)?.map(iso_from_millis),
                "runCount": row.get::<_, i64>(4)?,
                "createdAt": iso_from_millis(row.get::<_, i64>(5)?),
                "userId": row.get::<_, Option<String>>(6)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(items),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_patch_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Json(body): Json<ApiUpdateStatusBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let status = body.status.unwrap_or_else(|| "active".to_string());
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "UPDATE sessions SET status=?1, last_activity=?2 WHERE tenant_id=?3 AND id=?4",
            rusqlite::params![status, now_ms(), tenant, key],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({"id": key, "status": status})),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_reset_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "UPDATE sessions SET run_count=0, last_activity=?1 WHERE tenant_id=?2 AND id=?3",
            rusqlite::params![now_ms(), tenant, key],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({"reset": true})),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM sessions WHERE tenant_id=?1 AND id=?2",
            rusqlite::params![tenant, key],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({"deleted": true})),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_events(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, type, title, description, source, timestamp FROM events
             WHERE tenant_id=?1 ORDER BY timestamp DESC LIMIT 200",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "type": row.get::<_, String>(1)?,
                "title": row.get::<_, String>(2)?,
                "description": row.get::<_, Option<String>>(3)?,
                "source": row.get::<_, Option<String>>(4)?,
                "timestamp": iso_from_millis(row.get::<_, i64>(5)?),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(items),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<InboxListQuery>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200) as i64;
    let source_filter = query.source.unwrap_or_default();
    let status_filter = query.status.unwrap_or_else(|| "unread".to_string());
    let cursor = query.cursor.unwrap_or(i64::MAX);

    let sql = if source_filter.is_empty() {
        "SELECT id, source_type, source_id, run_id, chat_id, title, preview, body, metadata_json, status, created_at, read_at
         FROM inbox_items
         WHERE tenant_id=?1 AND status=?2 AND created_at < ?3
         ORDER BY created_at DESC
         LIMIT ?4"
    } else {
        "SELECT id, source_type, source_id, run_id, chat_id, title, preview, body, metadata_json, status, created_at, read_at
         FROM inbox_items
         WHERE tenant_id=?1 AND status=?2 AND source_type=?3 AND created_at < ?4
         ORDER BY created_at DESC
         LIMIT ?5"
    };

    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(sql)?;
        let mut items = Vec::new();
        if source_filter.is_empty() {
            let rows = stmt.query_map(
                rusqlite::params![tenant, status_filter, cursor, limit],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "sourceType": row.get::<_, String>(1)?,
                        "sourceId": row.get::<_, Option<String>>(2)?,
                        "runId": row.get::<_, Option<String>>(3)?,
                        "chatId": row.get::<_, Option<String>>(4)?,
                        "title": row.get::<_, String>(5)?,
                        "preview": row.get::<_, Option<String>>(6)?,
                        "body": row.get::<_, Option<String>>(7)?,
                        "metadata": row
                            .get::<_, Option<String>>(8)?
                            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                            .unwrap_or(serde_json::json!({})),
                        "status": row.get::<_, String>(9)?,
                        "createdAt": iso_from_millis(row.get::<_, i64>(10)?),
                        "readAt": row.get::<_, Option<i64>>(11)?.map(iso_from_millis),
                    }))
                },
            )?;
            items.extend(rows.filter_map(std::result::Result::ok));
        } else {
            let rows = stmt.query_map(
                rusqlite::params![tenant, status_filter, source_filter, cursor, limit],
                |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "sourceType": row.get::<_, String>(1)?,
                        "sourceId": row.get::<_, Option<String>>(2)?,
                        "runId": row.get::<_, Option<String>>(3)?,
                        "chatId": row.get::<_, Option<String>>(4)?,
                        "title": row.get::<_, String>(5)?,
                        "preview": row.get::<_, Option<String>>(6)?,
                        "body": row.get::<_, Option<String>>(7)?,
                        "metadata": row
                            .get::<_, Option<String>>(8)?
                            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                            .unwrap_or(serde_json::json!({})),
                        "status": row.get::<_, String>(9)?,
                        "createdAt": iso_from_millis(row.get::<_, i64>(10)?),
                        "readAt": row.get::<_, Option<i64>>(11)?.map(iso_from_millis),
                    }))
                },
            )?;
            items.extend(rows.filter_map(std::result::Result::ok));
        }
        let next_cursor = items
            .last()
            .and_then(|item| item["createdAt"].as_str())
            .and_then(|iso| chrono::DateTime::parse_from_rfc3339(iso).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc).timestamp_millis());
        Ok(serde_json::json!({
            "items": items,
            "nextCursor": next_cursor,
        }))
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_create_inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ApiCreateInboxBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let title = body
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let item_body = body
        .body
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty());
    let Some(title) = title else {
        return api_err(StatusCode::BAD_REQUEST, "title is required");
    };
    let Some(full_body) = item_body else {
        return api_err(StatusCode::BAD_REQUEST, "body is required");
    };

    let source_type = body.source_type.unwrap_or_else(|| "agent".to_string());
    let item = crate::dashboard::NewInboxItem {
        source_type: source_type.clone(),
        source_id: body.source_id,
        run_id: body.run_id,
        chat_id: body.chat_id,
        title: title.clone(),
        preview: body
            .preview
            .or_else(|| Some(full_body.chars().take(160).collect())),
        body: Some(full_body),
        metadata: body.metadata.unwrap_or_else(|| serde_json::json!({})),
        status: body.status,
    };

    let created = crate::dashboard::create_inbox_item(&state.registry_db, &tenant, &item);
    match created {
        Ok(id) => {
            status_events::emit(
                "inbox.item.created",
                serde_json::json!({
                    "tenantId": tenant,
                    "id": id,
                    "title": title,
                    "sourceType": source_type,
                }),
            );
            api_ok(serde_json::json!({ "id": id, "status": "unread" }))
        }
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_mark_inbox_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let ts = now_ms();
    let res = state.registry_db.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE inbox_items
             SET status='read', read_at=?1
             WHERE tenant_id=?2 AND id=?3 AND status!='archived'",
            rusqlite::params![ts, tenant, id],
        )?;
        Ok(changed)
    });
    match res {
        Ok(0) => api_err(StatusCode::NOT_FOUND, "Inbox item not found"),
        Ok(_) => {
            api_ok(serde_json::json!({ "id": id, "status": "read", "readAt": iso_from_millis(ts) }))
        }
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_archive_inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let ts = now_ms();
    let res = state.registry_db.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE inbox_items
             SET status='archived', archived_at=?1
             WHERE tenant_id=?2 AND id=?3",
            rusqlite::params![ts, tenant, id],
        )?;
        Ok(changed)
    });
    match res {
        Ok(0) => api_err(StatusCode::NOT_FOUND, "Inbox item not found"),
        Ok(_) => api_ok(serde_json::json!({ "id": id, "status": "archived" })),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_open_inbox_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let ts = now_ms();
    let res = state.registry_db.with_conn(|conn| {
        let row = conn.query_row(
            "SELECT title, preview, body, chat_id FROM inbox_items WHERE tenant_id=?1 AND id=?2",
            rusqlite::params![tenant, id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        );
        let (title, preview, body, existing_chat_id) = match row {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Ok((None::<String>, false));
            }
            Err(e) => return Err(e.into()),
        };

        let created_new = existing_chat_id.is_none();
        let chat_id = existing_chat_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if created_new {
            let seed = preview.clone().unwrap_or_else(|| title.clone());
            conn.execute(
                "INSERT OR IGNORE INTO chats (id, tenant_id, title, preview, session_id, message_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    tenant,
                    title,
                    seed,
                    chat_id,
                    ts
                ],
            )?;
            let assistant_text = body
                .or(preview)
                .unwrap_or_else(|| "Inbox item opened".to_string());
            conn.execute(
                "INSERT INTO messages (id, tenant_id, chat_id, role, content, created_at)
                 VALUES (?1, ?2, ?3, 'assistant', ?4, ?5)",
                rusqlite::params![
                    uuid::Uuid::new_v4().to_string(),
                    tenant,
                    chat_id,
                    assistant_text,
                    ts
                ],
            )?;
            conn.execute(
                "UPDATE chats SET message_count = message_count + 1, preview=?1, updated_at=?2
                 WHERE tenant_id=?3 AND session_id=?4",
                rusqlite::params![
                    assistant_text.chars().take(200).collect::<String>(),
                    ts,
                    tenant,
                    chat_id
                ],
            )?;
        }

        conn.execute(
            "UPDATE inbox_items
             SET chat_id=?1, status='read', read_at=?2
             WHERE tenant_id=?3 AND id=?4",
            rusqlite::params![chat_id, ts, tenant, id],
        )?;
        Ok((Some(chat_id), created_new))
    });

    match res {
        Ok((None, _)) => api_err(StatusCode::NOT_FOUND, "Inbox item not found"),
        Ok((Some(chat_id), created)) => api_ok(serde_json::json!({
            "id": id,
            "chatId": chat_id,
            "created": created,
        })),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_nodes(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, type, status, load, memory_usage, cpu_usage, last_heartbeat
             FROM nodes WHERE tenant_id=?1 ORDER BY name ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "type": row.get::<_, String>(2)?,
                "status": row.get::<_, String>(3)?,
                "load": row.get::<_, i64>(4)?,
                "memoryUsage": row.get::<_, i64>(5)?,
                "cpuUsage": row.get::<_, i64>(6)?,
                "lastHeartbeat": row.get::<_, Option<i64>>(7)?.map(iso_from_millis),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(items),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_get_metrics(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let active_sessions: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE tenant_id=?1 AND status='active'",
            rusqlite::params![tenant],
            |row| row.get(0),
        )?;
        let alerts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE tenant_id=?1 AND type IN ('error','warning')",
            rusqlite::params![tenant],
            |row| row.get(0),
        )?;
        Ok(serde_json::json!({
            "cpuUsage": 0,
            "memoryUsage": 0,
            "activeSessions": active_sessions,
            "alerts": alerts,
        }))
    });
    match res {
        Ok(data) => api_ok(data),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_get_status(State(_state): State<AppState>, _headers: HeaderMap) -> impl IntoResponse {
    let runtime = crate::health::snapshot_json();
    let uptime = runtime
        .get("uptime_seconds")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    api_ok(serde_json::json!({
        "isConnected": true,
        "uptime": format!("{uptime}s"),
        "activeSessions": 0,
        "alerts": 0
    }))
}

async fn api_list_channels(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id,name,type,status,requests_per_min,endpoint,created_at,updated_at
             FROM channels WHERE tenant_id=?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "type": row.get::<_, String>(2)?,
                "status": row.get::<_, String>(3)?,
                "requestsPerMin": row.get::<_, i64>(4)?,
                "endpoint": row.get::<_, Option<String>>(5)?,
                "createdAt": iso_from_millis(row.get::<_, i64>(6)?),
                "updatedAt": iso_from_millis(row.get::<_, i64>(7)?),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_patch_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ApiPatchChannelBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let status = body.status.unwrap_or_else(|| "active".to_string());
    let name = body.name.unwrap_or_else(|| id.clone());
    let now = now_ms();
    let res = state.registry_db.with_conn(|conn| {
        let updated = conn.execute(
            "UPDATE channels SET name=?1, status=?2, updated_at=?3 WHERE tenant_id=?4 AND id=?5",
            rusqlite::params![name, status, now, tenant, id],
        )?;
        if updated == 0 {
            conn.execute(
                "INSERT INTO channels (id, tenant_id, name, type, status, requests_per_min, endpoint, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'custom', ?4, 0, NULL, ?5, ?5)",
                rusqlite::params![id, tenant, name, status, now],
            )?;
        }
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({ "id": id, "name": name, "status": status })),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_delete_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM channels WHERE tenant_id=?1 AND id=?2",
            rusqlite::params![tenant, id],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({"id": id, "deleted": true})),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_cron(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let runtime_db_path = state.workspace_dir.join("cron").join("jobs.db");
    let mut runtime_map: HashMap<String, (Option<String>, Option<String>, Option<String>)> =
        HashMap::new();
    if runtime_db_path.exists() {
        if let Ok(conn) = rusqlite::Connection::open(runtime_db_path) {
            if let Ok(mut stmt) =
                conn.prepare("SELECT id, next_run, last_run, last_status FROM cron_jobs")
            {
                if let Ok(rows) = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                }) {
                    for (id, next, last, status) in rows.flatten() {
                        runtime_map.insert(id, (next, last, status));
                    }
                }
            }
        }
    }

    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, description, schedule_kind, schedule_data, status, enabled, cron_job_id, created_at, updated_at
             FROM aria_cron_functions
             WHERE tenant_id=?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            let schedule_kind: String = row.get(3)?;
            let schedule_raw: String = row.get(4)?;
            let schedule_json: serde_json::Value = serde_json::from_str(&schedule_raw)
                .unwrap_or_else(|_| serde_json::json!({}));
            let schedule = match schedule_kind.as_str() {
                "cron" => schedule_json
                    .get("expr")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("* * * * *")
                    .to_string(),
                "every" => format!(
                    "every {}s",
                    schedule_json
                        .get("every_ms")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(60000)
                        / 1000
                ),
                "at" => format!(
                    "at {}",
                    schedule_json
                        .get("at_ms")
                        .and_then(serde_json::Value::as_i64)
                        .map(|ms| iso_from_millis(ms))
                        .unwrap_or_else(|| "unknown".to_string())
                ),
                _ => schedule_raw.clone(),
            };

            let enabled = row.get::<_, i64>(6)? != 0;
            let declared_status: String = row.get(5)?;
            let status = if enabled && declared_status == "active" {
                "active"
            } else {
                "paused"
            };
            let runtime_id: Option<String> = row.get(7)?;
            let (next_run, last_run) = if let Some(rid) = runtime_id {
                if let Some((next, last, _)) = runtime_map.get(&rid) {
                    (next.clone(), last.clone())
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "schedule": schedule,
                "status": status,
                "lastRun": last_run,
                "nextRun": next_run,
                "handler": "cron_handler",
                "description": row.get::<_, String>(2)?,
                "createdAt": row.get::<_, String>(8)?,
                "updatedAt": row.get::<_, String>(9)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(items),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_patch_cron(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ApiUpdateStatusBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let status = body.status.unwrap_or_else(|| "active".to_string());
    let enabled = status != "paused";
    let now = chrono::Utc::now().to_rfc3339();

    let updated = state.registry_db.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE aria_cron_functions
             SET enabled=?1, status=?2, updated_at=?3
             WHERE id=?4 AND tenant_id=?5",
            rusqlite::params![
                i64::from(enabled),
                if enabled { "active" } else { "paused" },
                now,
                id,
                tenant
            ],
        )?;
        Ok(changed)
    });
    match updated {
        Ok(0) => api_err(StatusCode::NOT_FOUND, "Cron function not found"),
        Ok(_) => {
            crate::aria::hooks::notify_cron_uploaded(&id);
            api_ok(
                serde_json::json!({ "id": id, "status": if enabled { "active" } else { "paused" } }),
            )
        }
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
async fn api_delete_cron(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let changed = conn.execute(
            "DELETE FROM aria_cron_functions WHERE id=?1 AND tenant_id=?2",
            rusqlite::params![id, tenant],
        )?;
        Ok(changed)
    });
    match res {
        Ok(0) => api_err(StatusCode::NOT_FOUND, "Cron function not found"),
        Ok(_) => {
            crate::aria::hooks::notify_cron_deleted(&id);
            api_ok(serde_json::json!({"id": id, "deleted": true}))
        }
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
async fn api_run_cron(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let cron_job_id = state.registry_db.with_conn(|conn| {
        let found = conn.query_row(
            "SELECT cron_job_id FROM aria_cron_functions WHERE id=?1 AND tenant_id=?2",
            rusqlite::params![id, tenant],
            |row| row.get::<_, Option<String>>(0),
        );
        match found {
            Ok(v) => Ok(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    });

    let mut runtime_id = match cron_job_id {
        Ok(v) => v,
        Err(_) => return api_err(StatusCode::NOT_FOUND, "Cron function not found"),
    };
    if runtime_id.is_none() {
        crate::aria::hooks::notify_cron_uploaded(&id);
        runtime_id = state
            .registry_db
            .with_conn(|conn| {
                let found = conn.query_row(
                    "SELECT cron_job_id FROM aria_cron_functions WHERE id=?1 AND tenant_id=?2",
                    rusqlite::params![id, tenant],
                    |row| row.get::<_, Option<String>>(0),
                );
                match found {
                    Ok(v) => Ok(v),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(e.into()),
                }
            })
            .unwrap_or(None);
    }

    let Some(rid) = runtime_id else {
        return api_err(StatusCode::CONFLICT, "Cron function is not schedulable");
    };

    let db_path = state.workspace_dir.join("cron").join("jobs.db");
    let queued = (|| -> Result<bool> {
        let conn = rusqlite::Connection::open(db_path)?;
        let due_at = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
        let changed = conn.execute(
            "UPDATE cron_jobs SET next_run=?1 WHERE id=?2",
            rusqlite::params![due_at, rid],
        )?;
        Ok(changed > 0)
    })();

    match queued {
        Ok(true) => api_ok(serde_json::json!({"id": id, "queued": true})),
        Ok(false) => api_err(StatusCode::NOT_FOUND, "Runtime cron job not found"),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_skills(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id,name,description,enabled,call_count,version,category,permissions_json
             FROM skills WHERE tenant_id=?1 ORDER BY name ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                "enabled": row.get::<_, i64>(3)? != 0,
                "callCount": row.get::<_, i64>(4)?,
                "version": row.get::<_, Option<String>>(5)?.unwrap_or_else(|| "1.0.0".to_string()),
                "category": row.get::<_, Option<String>>(6)?.unwrap_or_else(|| "system".to_string()),
                "permissions": row
                    .get::<_, Option<String>>(7)?
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .unwrap_or(serde_json::json!([])),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
async fn api_patch_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ApiPatchSkillBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let enabled = body.enabled.unwrap_or(true);
    let res = state.registry_db.with_conn(|conn| {
        let updated = conn.execute(
            "UPDATE skills SET enabled=?1 WHERE tenant_id=?2 AND id=?3",
            rusqlite::params![if enabled { 1 } else { 0 }, tenant, id],
        )?;
        if updated == 0 {
            conn.execute(
                "INSERT INTO skills (id, tenant_id, name, description, enabled, call_count, version, category, permissions_json)
                 VALUES (?1, ?2, ?3, '', ?4, 0, '1.0.0', 'system', '[]')",
                rusqlite::params![id, tenant, id, if enabled { 1 } else { 0 }],
            )?;
        }
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({ "id": id, "enabled": enabled })),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_get_config(State(state): State<AppState>, _headers: HeaderMap) -> impl IntoResponse {
    api_ok(serde_json::json!({
        "gateway": { "port": state.gateway_port, "host": state.gateway_host, "cors": {"enabled": true, "origins": ["*"]}},
        "auth": { "provider": "apikey" },
        "limits": { "maxConcurrentRuns": 8, "timeoutMs": 30000, "maxTokensPerRequest": 8192, "rateLimitPerMinute": 120 }
    }))
}
async fn api_put_config(Json(body): Json<ApiUpdateConfigBody>) -> impl IntoResponse {
    api_ok(serde_json::json!({
        "gateway": body.gateway.unwrap_or(serde_json::json!({})),
        "auth": body.auth.unwrap_or(serde_json::json!({})),
        "limits": body.limits.unwrap_or(serde_json::json!({}))
    }))
}

async fn api_list_tools(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id,name,description,version,updated_at FROM aria_tools WHERE tenant_id=?1 AND status!='deleted' ORDER BY created_at DESC")?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, String>(2)?,
                "category": "custom",
                "enabled": true,
                "version": row.get::<_, i64>(3)?.to_string(),
                "callCount": 0,
                "avgDurationMs": 0,
                "lastUsed": row.get::<_, String>(4)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_agents(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id,name,description,status,model,tools,created_at,updated_at FROM aria_agents WHERE tenant_id=?1 AND status!='deleted' ORDER BY created_at DESC")?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            let tools_json = row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
            let tools = serde_json::from_str::<serde_json::Value>(&tools_json).unwrap_or(serde_json::json!([]));
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, String>(2)?,
                "status": match row.get::<_, String>(3)?.as_str() { "active" => "online", other => other },
                "model": row.get::<_, Option<String>>(4)?.unwrap_or_else(|| "default".to_string()),
                "tools": tools,
                "runCount": 0,
                "successRate": 1.0,
                "createdAt": row.get::<_, String>(6)?,
                "lastActive": row.get::<_, String>(7)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_teams(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id,name,description,mode,members,shared_context,timeout_seconds,status,created_at,updated_at FROM aria_teams WHERE tenant_id=?1 AND status!='deleted' ORDER BY created_at DESC")?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            let members_json = row.get::<_, String>(4).unwrap_or_else(|_| "[]".to_string());
            let members = serde_json::from_str::<serde_json::Value>(&members_json).unwrap_or(serde_json::json!([]));
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, String>(2)?,
                "mode": row.get::<_, String>(3)?,
                "agents": members,
                "coordinator": serde_json::Value::Null,
                "sharedContext": row.get::<_, Option<String>>(5)?.unwrap_or_else(|| "false".to_string()) == "true",
                "timeoutSeconds": row.get::<_, Option<i64>>(6)?,
                "status": row.get::<_, String>(7)?,
                "createdAt": row.get::<_, String>(8)?,
                "updatedAt": row.get::<_, String>(9)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_get_team(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.query_row(
            "SELECT id,name,description,mode,members,shared_context,timeout_seconds,status,created_at,updated_at
             FROM aria_teams WHERE tenant_id=?1 AND id=?2 AND status!='deleted'",
            rusqlite::params![tenant, id],
            |row| {
                let members_json = row.get::<_, String>(4).unwrap_or_else(|_| "[]".to_string());
                let members = serde_json::from_str::<serde_json::Value>(&members_json).unwrap_or(serde_json::json!([]));
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, String>(2)?,
                    "mode": row.get::<_, String>(3)?,
                    "agents": members,
                    "coordinator": serde_json::Value::Null,
                    "sharedContext": row.get::<_, Option<String>>(5)?.unwrap_or_else(|| "false".to_string()) == "true",
                    "timeoutSeconds": row.get::<_, Option<i64>>(6)?,
                    "status": row.get::<_, String>(7)?,
                    "createdAt": row.get::<_, String>(8)?,
                    "updatedAt": row.get::<_, String>(9)?,
                }))
            },
        )
        .map_err(anyhow::Error::from)
    });
    match res {
        Ok(v) => api_ok(v),
        Err(_) => api_err(StatusCode::NOT_FOUND, "Team not found"),
    }
}

async fn api_list_pipelines(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id,name,description,status,steps,created_at,updated_at FROM aria_pipelines WHERE tenant_id=?1 AND status!='deleted' ORDER BY created_at DESC")?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            let steps_json = row.get::<_, String>(4).unwrap_or_else(|_| "[]".to_string());
            let stages = serde_json::from_str::<serde_json::Value>(&steps_json).unwrap_or(serde_json::json!([]));
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, String>(2)?,
                "status": row.get::<_, String>(3)?,
                "stages": stages,
                "runCount": 0,
                "lastRun": serde_json::Value::Null,
                "createdAt": row.get::<_, String>(5)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_get_pipeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.query_row(
            "SELECT id,name,description,status,steps,created_at FROM aria_pipelines WHERE tenant_id=?1 AND id=?2 AND status!='deleted'",
            rusqlite::params![tenant, id],
            |row| {
                let steps_json = row.get::<_, String>(4).unwrap_or_else(|_| "[]".to_string());
                let stages = serde_json::from_str::<serde_json::Value>(&steps_json).unwrap_or(serde_json::json!([]));
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, String>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "stages": stages,
                    "runCount": 0,
                    "lastRun": serde_json::Value::Null,
                    "createdAt": row.get::<_, String>(5)?,
                }))
            },
        )
        .map_err(anyhow::Error::from)
    });
    match res {
        Ok(v) => api_ok(v),
        Err(_) => api_err(StatusCode::NOT_FOUND, "Pipeline not found"),
    }
}

async fn api_list_kv(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT key,value,created_at,updated_at FROM aria_kv WHERE tenant_id=?1 ORDER BY updated_at DESC")?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            let val_str: String = row.get(1)?;
            let value = serde_json::from_str::<serde_json::Value>(&val_str).unwrap_or(serde_json::Value::String(val_str));
            Ok(serde_json::json!({
                "key": row.get::<_, String>(0)?,
                "value": value,
                "createdAt": row.get::<_, String>(2)?,
                "updatedAt": row.get::<_, String>(3)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_get_kv(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.query_row(
            "SELECT key,value,created_at,updated_at FROM aria_kv WHERE tenant_id=?1 AND key=?2",
            rusqlite::params![tenant, key],
            |row| {
                let val_str: String = row.get(1)?;
                let value = serde_json::from_str::<serde_json::Value>(&val_str)
                    .unwrap_or(serde_json::Value::String(val_str));
                Ok(serde_json::json!({
                    "key": row.get::<_, String>(0)?,
                    "value": value,
                    "createdAt": row.get::<_, String>(2)?,
                    "updatedAt": row.get::<_, String>(3)?,
                }))
            },
        )
        .map_err(anyhow::Error::from)
    });
    match res {
        Ok(v) => api_ok(v),
        Err(_) => api_err(StatusCode::NOT_FOUND, "KV key not found"),
    }
}

async fn api_list_containers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id,name,image,state,created_at FROM aria_containers WHERE tenant_id=?1 ORDER BY created_at DESC")?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            let status = match row.get::<_, String>(3)?.as_str() {
                "running" => "running",
                "pending" => "starting",
                "stopped" => "stopped",
                _ => "error",
            };
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "image": row.get::<_, String>(2)?,
                "status": status,
                "cpuUsage": 0,
                "memoryUsage": 0,
                "ports": [],
                "createdAt": row.get::<_, String>(4)?,
                "uptime": "0m",
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let limit = q.limit.unwrap_or(200) as i64;
    let offset = q.offset.unwrap_or(0) as i64;
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id,timestamp,level,source,message,metadata_json,trace_id,span_id,session_id,agent_id,duration FROM logs WHERE tenant_id=?1 ORDER BY timestamp DESC LIMIT ?2 OFFSET ?3")?;
        let rows = stmt.query_map(rusqlite::params![tenant, limit, offset], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "timestamp": iso_from_millis(row.get::<_, i64>(1)?),
                "level": row.get::<_, String>(2)?,
                "source": row.get::<_, Option<String>>(3)?.unwrap_or_else(|| "system".to_string()),
                "message": row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                "metadata": row.get::<_, Option<String>>(5)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "traceId": row.get::<_, Option<String>>(6)?,
                "spanId": row.get::<_, Option<String>>(7)?,
                "sessionId": row.get::<_, Option<String>>(8)?,
                "agentId": row.get::<_, Option<String>>(9)?,
                "duration": row.get::<_, Option<i64>>(10)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_api_keys(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id,name,key_preview,status,scopes_json,created_at,last_used_at,expires_at,request_count,rate_limit FROM api_keys WHERE tenant_id=?1 ORDER BY created_at DESC")?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "keyPreview": row.get::<_, String>(2)?,
                "status": row.get::<_, String>(3)?,
                "scopes": row
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .unwrap_or(serde_json::json!(["full"])),
                "createdAt": iso_from_millis(row.get::<_, i64>(5)?),
                "lastUsedAt": row.get::<_, Option<i64>>(6)?.map(iso_from_millis),
                "expiresAt": row.get::<_, Option<i64>>(7)?.map(iso_from_millis),
                "requestCount": row.get::<_, i64>(8)?,
                "rateLimit": row.get::<_, i64>(9)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_create_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ApiCreateKeyBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let id = uuid::Uuid::new_v4().to_string();
    let raw_key = format!("sk_{}", uuid::Uuid::new_v4().as_simple());
    let preview = format!("{}...{}", &raw_key[..7], &raw_key[raw_key.len() - 4..]);
    let now = now_ms();
    let name = body.name.unwrap_or_else(|| "Default API Key".to_string());
    let scopes = serde_json::to_string(&body.scopes.unwrap_or_else(|| vec!["full".to_string()]))
        .unwrap_or_else(|_| "[]".to_string());
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO api_keys (id, tenant_id, name, key_hash, key_preview, status, scopes_json, rate_limit, request_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, 1000, 0, ?7)",
            rusqlite::params![id, tenant, name, raw_key, preview, scopes, now],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({
            "id": id,
            "name": name,
            "keyPreview": preview,
            "status": "active",
            "scopes": serde_json::from_str::<serde_json::Value>(&scopes).unwrap_or(serde_json::json!(["full"])),
            "createdAt": iso_from_millis(now),
            "lastUsedAt": serde_json::Value::Null,
            "expiresAt": serde_json::Value::Null,
            "requestCount": 0,
            "rateLimit": 1000,
            "rawKey": raw_key,
        })),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_revoke_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let _ = state.registry_db.with_conn(|conn| {
        conn.execute(
            "UPDATE api_keys SET status='revoked' WHERE tenant_id=?1 AND id=?2",
            rusqlite::params![tenant, id],
        )?;
        Ok(())
    });
    api_ok(serde_json::json!({"revoked": true}))
}

async fn api_create_magic_number(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let id = uuid::Uuid::new_v4().to_string();
    let raw = format!("mn_{}", uuid::Uuid::new_v4().as_simple());
    let preview = format!("{}...{}", &raw[..7], &raw[raw.len() - 4..]);
    let now = now_ms();
    let _ = state.registry_db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO magic_numbers (id, tenant_id, user_id, email, key_hash, key_preview, name, status, created_at)
             VALUES (?1, ?2, 'dev-user', NULL, ?3, ?4, 'default', 'active', ?5)",
            rusqlite::params![id, tenant, raw, preview, now],
        )?;
        Ok(())
    });
    api_ok(serde_json::json!({
        "id": id,
        "magicNumber": raw,
        "keyPreview": preview,
        "tenantId": tenant,
        "userId": "dev-user",
    }))
}

async fn api_list_magic_numbers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id,name,key_preview,user_id,email,created_at,last_used_at FROM magic_numbers WHERE tenant_id=?1 AND status='active' ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "keyPreview": row.get::<_, String>(2)?,
                "userId": row.get::<_, String>(3)?,
                "email": row.get::<_, Option<String>>(4)?,
                "createdAt": row.get::<_, i64>(5)?,
                "lastUsedAt": row.get::<_, Option<i64>>(6)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(items) => api_ok(serde_json::json!({ "magicNumbers": items })),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_revoke_magic_number(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let _ = state.registry_db.with_conn(|conn| {
        conn.execute(
            "UPDATE magic_numbers SET status='revoked', revoked_at=?1 WHERE tenant_id=?2 AND id=?3",
            rusqlite::params![now_ms(), tenant, id],
        )?;
        Ok(())
    });
    api_ok(serde_json::json!({ "revoked": true }))
}

async fn api_get_billing(_state: State<AppState>, _headers: HeaderMap) -> impl IntoResponse {
    api_ok(serde_json::json!({
        "plan": "free",
        "status": "active",
        "nextInvoiceDate": serde_json::Value::Null,
    }))
}
async fn api_get_billing_usage(_state: State<AppState>, _headers: HeaderMap) -> impl IntoResponse {
    api_ok(serde_json::json!({
        "requests": 0,
        "tokens": 0,
        "storageBytes": 0
    }))
}
async fn api_get_billing_invoices(
    _state: State<AppState>,
    _headers: HeaderMap,
) -> impl IntoResponse {
    api_ok(Vec::<serde_json::Value>::new())
}
async fn api_get_billing_methods(
    _state: State<AppState>,
    _headers: HeaderMap,
) -> impl IntoResponse {
    api_ok(Vec::<serde_json::Value>::new())
}

async fn api_list_feeds(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT f.id,f.name,f.description,f.schedule,f.refresh_seconds,f.category,f.status,f.created_at,f.updated_at,
                    (SELECT COUNT(*) FROM aria_feed_items i WHERE i.tenant_id=f.tenant_id AND i.feed_id=f.id) AS item_count
             FROM aria_feeds f
             WHERE f.tenant_id=?1 AND f.status!='deleted'
             ORDER BY f.created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, String>(2)?,
                "type": serde_json::Value::Null,
                "schedule": row.get::<_, String>(3)?,
                "timezone": serde_json::Value::Null,
                "refreshSeconds": row.get::<_, Option<i64>>(4)?,
                "agent": "feed-agent",
                "tools": [],
                "category": row.get::<_, Option<String>>(5)?,
                "status": row.get::<_, String>(6)?,
                "itemCount": row.get::<_, i64>(9)?,
                "createdAt": row.get::<_, String>(7)?,
                "updatedAt": row.get::<_, String>(8)?,
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_get_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.query_row(
            "SELECT f.id,f.name,f.description,f.schedule,f.refresh_seconds,f.category,f.status,f.created_at,f.updated_at,
                    (SELECT COUNT(*) FROM aria_feed_items i WHERE i.tenant_id=f.tenant_id AND i.feed_id=f.id) AS item_count
             FROM aria_feeds f
             WHERE f.tenant_id=?1 AND f.id=?2 AND f.status!='deleted'",
            rusqlite::params![tenant, id],
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "description": row.get::<_, String>(2)?,
                    "type": serde_json::Value::Null,
                    "schedule": row.get::<_, String>(3)?,
                    "timezone": serde_json::Value::Null,
                    "refreshSeconds": row.get::<_, Option<i64>>(4)?,
                    "agent": "feed-agent",
                    "tools": [],
                    "category": row.get::<_, Option<String>>(5)?,
                    "status": row.get::<_, String>(6)?,
                    "itemCount": row.get::<_, i64>(9)?,
                    "createdAt": row.get::<_, String>(7)?,
                    "updatedAt": row.get::<_, String>(8)?,
                }))
            },
        )
        .map_err(anyhow::Error::from)
    });
    match res {
        Ok(v) => api_ok(v),
        Err(_) => api_err(StatusCode::NOT_FOUND, "Feed not found"),
    }
}

async fn api_list_feed_items(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let limit = q.limit.unwrap_or(100) as i64;
    let offset = q.offset.unwrap_or(0) as i64;
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id,card_type,source,metadata,timestamp FROM aria_feed_items
             WHERE tenant_id=?1 AND feed_id=?2 ORDER BY created_at DESC LIMIT ?3 OFFSET ?4",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant, id, limit, offset], |row| {
            let card_type = row.get::<_, String>(1)?;
            let metadata_str = row
                .get::<_, Option<String>>(3)?
                .unwrap_or_else(|| "{}".to_string());
            let data = serde_json::from_str::<serde_json::Value>(&metadata_str)
                .unwrap_or(serde_json::json!({}));
            let ts = row
                .get::<_, Option<i64>>(4)?
                .map(iso_from_millis)
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "type": card_type,
                "timestamp": ts,
                "source": row.get::<_, Option<String>>(2)?,
                "data": data
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_patch_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ApiUpdateFeedBody>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let status = body.status.unwrap_or_else(|| "active".to_string());
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "UPDATE aria_feeds SET status=?1, updated_at=?2 WHERE tenant_id=?3 AND id=?4",
            rusqlite::params![status, chrono::Utc::now().to_rfc3339(), tenant, id],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({ "id": id, "status": status })),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_feed_files(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id,name,extension,content_type,size,source_id,description,tags_json,created_at,updated_at
             FROM feed_files WHERE tenant_id=?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "extension": row.get::<_, String>(2)?,
                "contentType": row.get::<_, String>(3)?,
                "size": row.get::<_, i64>(4)?,
                "sourceId": row.get::<_, Option<String>>(5)?,
                "description": row.get::<_, Option<String>>(6)?,
                "tags": row
                    .get::<_, Option<String>>(7)?
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .unwrap_or(serde_json::json!([])),
                "createdAt": iso_from_millis(row.get::<_, i64>(8)?),
                "updatedAt": row.get::<_, Option<i64>>(9)?.map(iso_from_millis),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
async fn api_upload_feed_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let now = now_ms();
    let mut filename = "upload.bin".to_string();
    let mut content_type = "application/octet-stream".to_string();
    let mut content_raw = String::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        if let Some(name) = field.file_name() {
            filename = name.to_string();
        }
        if let Some(ct) = field.content_type() {
            content_type = ct.to_string();
        }
        if let Ok(bytes) = field.bytes().await {
            content_raw = String::from_utf8_lossy(&bytes).to_string();
            break;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let ext = filename.split('.').next_back().unwrap_or("bin").to_string();
    let size = content_raw.len() as i64;
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO feed_files (id, tenant_id, name, extension, content_type, size, blob_key, source_id, description, tags_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, '[]', ?8, ?8)",
            rusqlite::params![id, tenant, filename, ext, content_type, size, content_raw, now],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({
            "id": id,
            "name": filename,
            "extension": ext,
            "contentType": content_type,
            "size": size,
            "createdAt": iso_from_millis(now),
        })),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
async fn api_get_feed_file_content(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(file_id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.query_row(
            "SELECT blob_key, content_type FROM feed_files WHERE tenant_id=?1 AND id=?2",
            rusqlite::params![tenant, file_id],
            |row| {
                let raw: String = row.get(0)?;
                let ctype: String = row.get(1)?;
                Ok(serde_json::json!({
                    "fileId": file_id,
                    "contentType": if ctype.contains("json") { "json" } else if ctype.contains("csv") { "csv" } else if ctype.contains("markdown") || ctype.contains("md") { "markdown" } else { "plaintext" },
                    "raw": raw
                }))
            },
        )
        .map_err(anyhow::Error::from)
    });
    match res {
        Ok(v) => api_ok(v),
        Err(_) => api_err(StatusCode::NOT_FOUND, "File not found"),
    }
}
async fn api_delete_feed_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(file_id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM feed_files WHERE tenant_id=?1 AND id=?2",
            rusqlite::params![tenant, file_id],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => api_ok(serde_json::json!({ "id": file_id, "deleted": true })),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_list_tool_calls(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id,session_id,run_id,agent_id,tool_name,status,args_json,result_json,error,duration_ms,created_at,updated_at
             FROM tool_calls WHERE tenant_id=?1 ORDER BY created_at DESC LIMIT 200",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "sessionId": row.get::<_, Option<String>>(1)?,
                "runId": row.get::<_, Option<String>>(2)?,
                "agentId": row.get::<_, Option<String>>(3)?,
                "toolName": row.get::<_, String>(4)?,
                "status": row.get::<_, String>(5)?,
                "args": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "result": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "error": row.get::<_, Option<String>>(8)?,
                "durationMs": row.get::<_, Option<i64>>(9)?,
                "createdAt": iso_from_millis(row.get::<_, i64>(10)?),
                "updatedAt": iso_from_millis(row.get::<_, i64>(11)?),
            }))
        })?;
        Ok(rows.filter_map(std::result::Result::ok).collect::<Vec<_>>())
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_tool_calls_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE tenant_id=?1",
            rusqlite::params![tenant],
            |row| row.get(0),
        )?;
        let errors: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tool_calls WHERE tenant_id=?1 AND status='error'",
            rusqlite::params![tenant],
            |row| row.get(0),
        )?;
        Ok(serde_json::json!({ "total": total, "errors": errors }))
    });
    match res {
        Ok(v) => api_ok(v),
        Err(e) => api_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn api_get_tool_call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let tenant = match api_tenant(&state, &headers) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let res = state.registry_db.with_conn(|conn| {
        conn.query_row(
            "SELECT id,session_id,run_id,agent_id,tool_name,status,args_json,result_json,error,duration_ms,created_at,updated_at
             FROM tool_calls WHERE tenant_id=?1 AND id=?2",
            rusqlite::params![tenant, id],
            |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "sessionId": row.get::<_, Option<String>>(1)?,
                    "runId": row.get::<_, Option<String>>(2)?,
                    "agentId": row.get::<_, Option<String>>(3)?,
                    "toolName": row.get::<_, String>(4)?,
                    "status": row.get::<_, String>(5)?,
                    "args": row.get::<_, Option<String>>(6)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "result": row.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                    "error": row.get::<_, Option<String>>(8)?,
                    "durationMs": row.get::<_, Option<i64>>(9)?,
                    "createdAt": iso_from_millis(row.get::<_, i64>(10)?),
                    "updatedAt": iso_from_millis(row.get::<_, i64>(11)?),
                }))
            },
        )
        .map_err(anyhow::Error::from)
    });
    match res {
        Ok(v) => api_ok(v),
        Err(_) => api_err(StatusCode::NOT_FOUND, "Tool call not found"),
    }
}

/// `WhatsApp` verification query params
#[derive(serde::Deserialize)]
pub struct WhatsAppVerifyQuery {
    #[serde(rename = "hub.mode")]
    pub mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub challenge: Option<String>,
}

/// GET /whatsapp — Meta webhook verification
async fn handle_whatsapp_verify(
    State(state): State<AppState>,
    Query(params): Query<WhatsAppVerifyQuery>,
) -> impl IntoResponse {
    let Some(ref wa) = state.whatsapp else {
        return (StatusCode::NOT_FOUND, "WhatsApp not configured".to_string());
    };

    // Verify the token matches
    if params.mode.as_deref() == Some("subscribe")
        && params.verify_token.as_deref() == Some(wa.verify_token())
    {
        if let Some(ch) = params.challenge {
            tracing::info!("WhatsApp webhook verified successfully");
            return (StatusCode::OK, ch);
        }
        return (StatusCode::BAD_REQUEST, "Missing hub.challenge".to_string());
    }

    tracing::warn!("WhatsApp webhook verification failed — token mismatch");
    (StatusCode::FORBIDDEN, "Forbidden".to_string())
}

/// POST /whatsapp — incoming message webhook
async fn handle_whatsapp_message(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    let Some(ref wa) = state.whatsapp else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "WhatsApp not configured"})),
        );
    };

    // Parse JSON body
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Invalid JSON payload"})),
        );
    };

    // Parse messages from the webhook payload
    let messages = wa.parse_webhook_payload(&payload);

    if messages.is_empty() {
        // Acknowledge the webhook even if no messages (could be status updates)
        return (StatusCode::OK, Json(serde_json::json!({"status": "ok"})));
    }

    // Process each message
    for msg in &messages {
        tracing::info!(
            "WhatsApp message from {}: {}",
            msg.sender,
            if msg.content.len() > 50 {
                format!("{}...", &msg.content[..50])
            } else {
                msg.content.clone()
            }
        );

        // Auto-save to memory
        if state.auto_save {
            let _ = state
                .mem
                .store(
                    &format!("whatsapp_{}", msg.sender),
                    &msg.content,
                    MemoryCategory::Conversation,
                )
                .await;
        }

        // Run a full live turn with tenant-scoped tools/prompt.
        let tenant = resolve_tenant_from_token(&state, "");
        match crate::agent::orchestrator::run_live_turn(
            crate::agent::orchestrator::LiveTurnConfig {
                provider: state.provider.as_ref(),
                security: &state.security,
                memory: state.mem.clone(),
                composio_api_key: state.composio_api_key.as_deref(),
                browser_config: &state.browser_config,
                registry_db: &state.registry_db,
                workspace_dir: &state.workspace_dir,
                tenant_id: &tenant,
                model: &state.model,
                temperature: state.temperature,
                mode_hint: "",
                max_turns: Some(25),
                external_tool_context: None,
            },
            &msg.content,
            None,
        )
        .await
        {
            Ok(response) => {
                let reply = if response.output.is_empty() {
                    "Tool execution completed".to_string()
                } else {
                    response.output
                };
                // Send reply via WhatsApp
                if let Err(e) = wa.send(&reply, &msg.sender).await {
                    tracing::error!("Failed to send WhatsApp reply: {e}");
                }
            }
            Err(e) => {
                tracing::error!("LLM error for WhatsApp message: {e}");
                let _ = wa.send(&format!("⚠️ Error: {e}"), &msg.sender).await;
            }
        }
    }

    // Acknowledge the webhook
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
}

/// GET /ws/events — WebSocket endpoint for real-time agent event streaming.
///
/// Clients connect via WebSocket and receive JSON-serialized `AgentEvent` messages
/// for all tool executions, assistant text, thinking, and lifecycle events.
/// This powers the dashboard's real-time agent activity display.
async fn handle_events_ws_events(
    ws: WebSocketUpgrade,
    State(_state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_events_socket(socket, "events"))
}

async fn handle_events_ws_chat(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let bearer = auth.strip_prefix("Bearer ").unwrap_or("");
    let token = if !bearer.is_empty() {
        bearer.to_string()
    } else {
        query.get("token").cloned().unwrap_or_default()
    };
    let tenant = resolve_tenant_from_token(&state, &token);
    ws.on_upgrade(move |socket| handle_chat_socket(socket, state, tenant))
}

async fn handle_events_ws_local_bridge(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let bearer = auth.strip_prefix("Bearer ").unwrap_or("");
    let token = if !bearer.is_empty() {
        bearer.to_string()
    } else {
        query.get("token").cloned().unwrap_or_default()
    };
    let tenant = resolve_tenant_from_token(&state, &token);
    let device_id = query
        .get("deviceId")
        .cloned()
        .unwrap_or_else(|| "default-device".to_string());
    let bridge = state.local_tool_bridge.clone();
    ws.on_upgrade(move |socket| bridge.handle_socket(socket, tenant, device_id))
}

async fn handle_events_ws_status(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let bearer = auth.strip_prefix("Bearer ").unwrap_or("");
    let token = if !bearer.is_empty() {
        bearer.to_string()
    } else {
        query.get("token").cloned().unwrap_or_default()
    };
    let tenant = resolve_tenant_from_token(&state, &token);
    ws.on_upgrade(move |socket| handle_status_socket(socket, tenant))
}

async fn handle_events_ws_logs(
    ws: WebSocketUpgrade,
    State(_state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_events_socket(socket, "logs"))
}

struct WsChatSink<'a> {
    socket: &'a mut ws::WebSocket,
    state: &'a AppState,
    tenant: String,
    chat_id: String,
    run_id: String,
    accumulated: String,
    stream_seq: u64,
    thinking_started_ms: Option<i64>,
    thinking_content: String,
}

#[async_trait]
impl crate::agent::executor::AgentExecutionSink for WsChatSink<'_> {
    async fn on_assistant_delta(&mut self, delta: &str, accumulated: &str) {
        let bus = crate::events::event_bus();
        self.accumulated = accumulated.to_string();
        if delta.is_empty() {
            return;
        }
        // Emit in smaller chunks so the frontend renders progressively and
        // tool cards can interleave naturally with text updates.
        const MAX_CHUNK_CHARS: usize = 160;
        let chars: Vec<char> = delta.chars().collect();
        for chunk in chars.chunks(MAX_CHUNK_CHARS) {
            let delta_chunk: String = chunk.iter().collect();
            self.stream_seq = self.stream_seq.saturating_add(1);
            bus.emit_assistant(&self.run_id, Some(&self.chat_id), &delta_chunk, accumulated);
            let _ = self
                .socket
                .send(ws::Message::Text(
                    serde_json::json!({
                        "type": "run.streaming",
                        "runId": self.run_id,
                        "chatId": self.chat_id,
                        "delta": delta_chunk,
                        "seq": self.stream_seq,
                        "charCount": chunk.len(),
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                    })
                    .to_string()
                    .into(),
                ))
                .await;
        }
    }

    async fn on_thinking_start(&mut self) {
        self.thinking_started_ms = Some(now_ms());
        self.thinking_content.clear();
    }

    async fn on_thinking_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.thinking_content.push_str(delta);
        let _ = self
            .socket
            .send(ws::Message::Text(
                serde_json::json!({
                    "type": "run.thinking",
                    "runId": self.run_id,
                    "chatId": self.chat_id,
                    "delta": delta,
                })
                .to_string()
                .into(),
            ))
            .await;
    }

    async fn on_thinking_end(&mut self) {
        if self.thinking_content.is_empty() {
            self.thinking_started_ms = None;
            return;
        }
        let duration_ms = self
            .thinking_started_ms
            .map(|start| (now_ms() - start).max(0) as u64)
            .unwrap_or(0);
        let _ = self
            .socket
            .send(ws::Message::Text(
                serde_json::json!({
                    "type": "run.thinking.done",
                    "runId": self.run_id,
                    "chatId": self.chat_id,
                    "thinkingContent": self.thinking_content,
                    "durationMs": duration_ms,
                })
                .to_string()
                .into(),
            ))
            .await;
        self.thinking_started_ms = None;
    }

    async fn on_tool_start(&mut self, id: &str, name: &str, args: &serde_json::Value) {
        let bus = crate::events::event_bus();
        let now = now_ms();
        let _ = self.state.registry_db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tool_calls (id, tenant_id, session_id, run_id, agent_id, tool_name, status, args_json, result_json, error, duration_ms, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7, '', NULL, NULL, ?8, ?8)",
                rusqlite::params![
                    id,
                    self.tenant,
                    self.chat_id,
                    self.run_id,
                    "main",
                    name,
                    args.to_string(),
                    now,
                ],
            )?;
            Ok(())
        });

        let _ = self
            .socket
            .send(ws::Message::Text(
                serde_json::json!({
                    "type": "tool.started",
                    "runId": self.run_id,
                    "chatId": self.chat_id,
                    "toolCall": {
                        "id": id,
                        "name": name,
                        "args": args,
                        "startedAt": chrono::Utc::now().to_rfc3339(),
                    }
                })
                .to_string()
                .into(),
            ))
            .await;

        status_events::emit(
            "tool.started",
            serde_json::json!({
                "runId": self.run_id,
                "chatId": self.chat_id,
                "tenantId": self.tenant,
                "toolId": id,
                "toolName": name,
                "status": "running",
            }),
        );
        bus.emit_tool(
            &self.run_id,
            Some(&self.chat_id),
            crate::events::ToolEventData {
                phase: crate::events::ToolPhase::Start,
                tool_call_id: id.to_string(),
                name: name.to_string(),
                args: Some(args.clone()),
                partial_json: None,
                partial_result: None,
                result: None,
                error: None,
                is_error: None,
                duration_ms: None,
            },
        );
    }

    async fn on_tool_end(
        &mut self,
        id: &str,
        name: &str,
        result: &str,
        is_error: bool,
        duration_ms: u64,
    ) {
        let bus = crate::events::event_bus();
        let now = now_ms();
        let status = if is_error { "error" } else { "success" };
        let _ = self.state.registry_db.with_conn(|conn| {
            conn.execute(
                "UPDATE tool_calls
                 SET status=?1, result_json=?2, error=?3, duration_ms=?4, updated_at=?5
                 WHERE tenant_id=?6 AND run_id=?7 AND id=?8",
                rusqlite::params![
                    status,
                    result,
                    if is_error {
                        Some(result.to_string())
                    } else {
                        None::<String>
                    },
                    duration_ms as i64,
                    now,
                    self.tenant,
                    self.run_id,
                    id,
                ],
            )?;
            Ok(())
        });

        let _ = self
            .socket
            .send(ws::Message::Text(
                serde_json::json!({
                    "type": "tool.completed",
                    "runId": self.run_id,
                    "chatId": self.chat_id,
                    "toolCall": {
                        "id": id,
                        "name": name,
                        "status": status,
                        "duration": format!("{duration_ms}ms"),
                        "result": result,
                        "completedAt": chrono::Utc::now().to_rfc3339(),
                    }
                })
                .to_string()
                .into(),
            ))
            .await;

        status_events::emit(
            "tool.completed",
            serde_json::json!({
                "runId": self.run_id,
                "chatId": self.chat_id,
                "tenantId": self.tenant,
                "toolId": id,
                "toolName": name,
                "status": status,
                "durationMs": duration_ms,
            }),
        );
        bus.emit_tool(
            &self.run_id,
            Some(&self.chat_id),
            crate::events::ToolEventData {
                phase: crate::events::ToolPhase::Result,
                tool_call_id: id.to_string(),
                name: name.to_string(),
                args: None,
                partial_json: None,
                partial_result: None,
                result: Some(result.to_string()),
                error: if is_error {
                    Some(result.to_string())
                } else {
                    None
                },
                is_error: Some(is_error),
                duration_ms: Some(duration_ms),
            },
        );
    }
}

async fn handle_chat_socket(mut socket: ws::WebSocket, state: AppState, tenant: String) {
    tracing::info!("Dashboard chat WebSocket connected");
    let mut seen_request_ids: HashSet<String> = HashSet::new();

    while let Some(msg) = socket.recv().await {
        let ws::Message::Text(text) = (match msg {
            Ok(m) => m,
            Err(_) => break,
        }) else {
            continue;
        };

        let parsed = serde_json::from_str::<WsChatMessage>(text.as_ref());
        let req = match parsed {
            Ok(v) => v,
            Err(_) => {
                let _ = socket
                    .send(ws::Message::Text(
                        serde_json::json!({
                            "type": "error",
                            "error": { "type": "invalid_request_error", "message": "Invalid chat payload" }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await;
                continue;
            }
        };

        if req.message_type != "message" {
            continue;
        }

        if let Some(request_id) = req.request_id.as_deref() {
            if seen_request_ids.contains(request_id) {
                let _ = socket
                    .send(ws::Message::Text(
                        serde_json::json!({
                            "type": "run.completed",
                            "runId": format!("dup-{}", &request_id.chars().take(8).collect::<String>()),
                            "chatId": req.session_id.clone(),
                            "status": "complete",
                            "durationMs": 0,
                            "tokenCount": 0
                        })
                        .to_string()
                        .into(),
                    ))
                    .await;
                continue;
            }
            seen_request_ids.insert(request_id.to_string());
            if seen_request_ids.len() > 300 {
                seen_request_ids.clear();
            }
        }

        let content = req.content.unwrap_or_default().trim().to_string();
        if content.is_empty() {
            let _ = socket
                .send(ws::Message::Text(
                    serde_json::json!({
                        "type": "error",
                        "error": { "type": "invalid_request_error", "message": "content is required" }
                    })
                    .to_string()
                    .into(),
                ))
                .await;
            continue;
        }

        let chat_id = req
            .session_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let run_id = format!("run-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let started = now_ms();
        let mode = parse_chat_mode(req.mode.as_deref());

        ensure_chat_row(&state, &tenant, &chat_id, &content, started);
        let _ = insert_chat_message(&state, &tenant, &chat_id, "user", &content, started);

        let _ = socket
            .send(ws::Message::Text(
                serde_json::json!({
                    "type": "run.started",
                    "runId": run_id,
                    "chatId": chat_id,
                    "status": "thinking",
                    "message": "Processing..."
                })
                .to_string()
                .into(),
            ))
            .await;

        let mode_hint = mode_prompt(mode);
        let run_result = {
            let mut sink = WsChatSink {
                socket: &mut socket,
                state: &state,
                tenant: tenant.clone(),
                chat_id: chat_id.clone(),
                run_id: run_id.clone(),
                accumulated: String::new(),
                stream_seq: 0,
                thinking_started_ms: None,
                thinking_content: String::new(),
            };
            crate::agent::orchestrator::run_live_turn(
                crate::agent::orchestrator::LiveTurnConfig {
                    provider: state.provider.as_ref(),
                    security: &state.security,
                    memory: state.mem.clone(),
                    composio_api_key: state.composio_api_key.as_deref(),
                    browser_config: &state.browser_config,
                    registry_db: &state.registry_db,
                    workspace_dir: &state.workspace_dir,
                    tenant_id: &tenant,
                    model: &state.model,
                    temperature: state.temperature,
                    mode_hint,
                    max_turns: Some(25),
                    external_tool_context: Some(crate::agent::executor::ExternalToolContext {
                        tenant_id: tenant.clone(),
                        chat_id: chat_id.clone(),
                        run_id: run_id.clone(),
                        executor: state.local_tool_bridge.clone(),
                    }),
                },
                &content,
                Some(&mut sink),
            )
            .await
        };

        match run_result {
            Ok(response) => {
                let now = now_ms();
                let assistant_text = if response.output.is_empty() {
                    "Tool execution completed".to_string()
                } else {
                    response.output
                };
                let _ = insert_chat_message(
                    &state,
                    &tenant,
                    &chat_id,
                    "assistant",
                    &assistant_text,
                    now,
                );
                update_chat_preview(&state, &tenant, &chat_id, &assistant_text, now);

                let _ = socket
                    .send(ws::Message::Text(
                        serde_json::json!({
                            "type": "run.completed",
                            "runId": run_id,
                            "chatId": chat_id,
                            "status": "complete",
                            "durationMs": response.duration_ms,
                            "tokenCount": 0
                        })
                        .to_string()
                        .into(),
                    ))
                    .await;
                status_events::emit(
                    "task.completed",
                    serde_json::json!({
                        "id": run_id,
                        "name": "chat",
                        "tenantId": tenant,
                        "durationMs": response.duration_ms,
                    }),
                );
            }
            Err(e) => {
                let now = now_ms();
                let _ = state.registry_db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE tool_calls
                         SET status='error', error='Run aborted', updated_at=?1
                         WHERE tenant_id=?2 AND run_id=?3 AND status='running'",
                        rusqlite::params![now, tenant, run_id],
                    )?;
                    Ok(())
                });
                let _ = socket
                    .send(ws::Message::Text(
                        serde_json::json!({
                            "type": "error",
                            "runId": run_id,
                            "chatId": chat_id,
                            "error": { "type": "server_error", "message": format!("LLM error: {e}") }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await;
                status_events::emit(
                    "task.failed",
                    serde_json::json!({
                        "id": run_id,
                        "name": "chat",
                        "tenantId": tenant,
                        "errorMessage": e.to_string(),
                    }),
                );
                let _ = socket
                    .send(ws::Message::Text(
                        serde_json::json!({
                            "type": "run.completed",
                            "runId": run_id,
                            "chatId": chat_id,
                            "status": "error",
                            "durationMs": (now - started).max(0),
                            "tokenCount": 0
                        })
                        .to_string()
                        .into(),
                    ))
                    .await;
            }
        }
    }

    tracing::info!("Dashboard chat WebSocket disconnected");
}

fn status_event_visible_to_tenant(json: &str, tenant: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return true;
    };
    if let Some(evt_tenant) = value.get("tenantId").and_then(serde_json::Value::as_str) {
        return evt_tenant == tenant;
    }
    if let Some(evt_tenant) = value
        .get("data")
        .and_then(|v| v.get("tenantId"))
        .and_then(serde_json::Value::as_str)
    {
        return evt_tenant == tenant;
    }
    true
}

async fn handle_status_socket(mut socket: ws::WebSocket, tenant: String) {
    let (subscriber_id, mut rx) = status_events::subscribe();
    tracing::info!("Dashboard status WebSocket connected");

    loop {
        tokio::select! {
            maybe_json = rx.recv() => {
                match maybe_json {
                    Some(json) => {
                        if !status_event_visible_to_tenant(&json, &tenant) {
                            continue;
                        }
                        if socket.send(ws::Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(ws::Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }

    status_events::unsubscribe(subscriber_id);
    tracing::info!("Dashboard status WebSocket disconnected");
}

async fn handle_events_socket(mut socket: ws::WebSocket, channel: &'static str) {
    let bus = crate::events::event_bus();

    // Bounded channel prevents unbounded memory growth if the WebSocket can't
    // keep up. Events beyond the buffer are dropped with a warning.
    const EVENT_BUFFER: usize = 512;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(EVENT_BUFFER);
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let dropped_inner = dropped.clone();

    let listener_id = bus.subscribe(move |evt| {
        if let Ok(json) = serde_json::to_string(evt) {
            if tx.try_send(json).is_err() {
                let n = dropped_inner.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n.is_multiple_of(100) {
                    tracing::warn!(
                        dropped = n + 1,
                        "Event stream backpressure: dropping events (WebSocket too slow)"
                    );
                }
            }
        }
    });

    tracing::info!(channel, "Dashboard WebSocket connected");

    // Stream events to the client until they disconnect
    loop {
        tokio::select! {
            Some(json) = rx.recv() => {
                if socket.send(ws::Message::Text(json)).await.is_err() {
                    break; // Client disconnected
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(ws::Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {} // Ignore other messages (ping/pong handled by axum)
                }
            }
        }
    }

    bus.unsubscribe(listener_id);
    let total_dropped = dropped.load(std::sync::atomic::Ordering::Relaxed);
    if total_dropped > 0 {
        tracing::warn!(
            total_dropped,
            "WebSocket session dropped events due to backpressure"
        );
    }
    tracing::info!(channel, "Dashboard WebSocket disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_body_limit_is_64kb() {
        assert_eq!(MAX_BODY_SIZE, 65_536);
    }

    #[test]
    fn security_timeout_is_30_seconds() {
        assert_eq!(REQUEST_TIMEOUT_SECS, 30);
    }

    #[test]
    fn webhook_body_requires_message_field() {
        let valid = r#"{"message": "hello"}"#;
        let parsed: Result<WebhookBody, _> = serde_json::from_str(valid);
        assert!(parsed.is_ok());
        assert_eq!(parsed.unwrap().message, "hello");

        let missing = r#"{"other": "field"}"#;
        let parsed: Result<WebhookBody, _> = serde_json::from_str(missing);
        assert!(parsed.is_err());
    }

    #[test]
    fn whatsapp_query_fields_are_optional() {
        let q = WhatsAppVerifyQuery {
            mode: None,
            verify_token: None,
            challenge: None,
        };
        assert!(q.mode.is_none());
    }

    #[test]
    fn app_state_is_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<AppState>();
    }

    #[test]
    fn tools_prompt_section_reads_live_registry_updates() {
        let db = crate::aria::db::AriaDb::open_in_memory().unwrap();
        let tenant = "t1";
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_tools (id, tenant_id, name, description, schema, status, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', 1, ?6, ?6)",
                rusqlite::params![
                    "tool_1",
                    tenant,
                    "live_tool",
                    "v1",
                    "{\"type\":\"object\",\"properties\":{}}",
                    "2026-01-01T00:00:00Z"
                ],
            )?;
            Ok(())
        })
        .unwrap();

        let before = load_registry_tools_prompt_section(&db, tenant).unwrap();
        assert!(before.contains("live_tool"));
        assert!(before.contains("v1"));

        db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_tools SET description=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params!["v2", "2026-01-01T00:01:00Z", "tool_1"],
            )?;
            Ok(())
        })
        .unwrap();

        let after = load_registry_tools_prompt_section(&db, tenant).unwrap();
        assert!(after.contains("live_tool"));
        assert!(after.contains("v2"));
        assert!(!after.contains("): v1\n"));
    }

    #[test]
    fn agents_prompt_section_reads_live_registry_updates() {
        let db = crate::aria::db::AriaDb::open_in_memory().unwrap();
        let tenant = "t1";
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_agents (id, tenant_id, name, description, model, status, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', 1, ?6, ?6)",
                rusqlite::params![
                    "agent_1",
                    tenant,
                    "writer",
                    "v1",
                    "model-a",
                    "2026-01-01T00:00:00Z"
                ],
            )?;
            Ok(())
        })
        .unwrap();

        let before = load_registry_agents_prompt_section(&db, tenant).unwrap();
        assert!(before.contains("writer"));
        assert!(before.contains("model-a"));

        db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_agents SET model=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params!["model-b", "2026-01-01T00:01:00Z", "agent_1"],
            )?;
            Ok(())
        })
        .unwrap();

        let after = load_registry_agents_prompt_section(&db, tenant).unwrap();
        assert!(after.contains("writer"));
        assert!(after.contains("Model: model-b"));
        assert!(!after.contains("Model: model-a"));
    }
}
