//! WebSocket agent chat handler.
//!
//! Protocol:
//! ```text
//! Client -> Server: {"type":"message","content":"Hello"}
//! Server -> Client: {"type":"chunk","content":"Hi! "}
//! Server -> Client: {"type":"tool_call","name":"shell","args":{...}}
//! Server -> Client: {"type":"tool_result","name":"shell","output":"..."}
//! Server -> Client: {"type":"done","full_response":"..."}
//! ```

use super::AppState;
use crate::agent::loop_::{build_shell_policy_instructions, build_tool_instructions_from_specs};
use crate::memory::MemoryCategory;
use crate::providers::ChatMessage;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        RawQuery, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap},
    response::IntoResponse,
};
use uuid::Uuid;

const EMPTY_WS_RESPONSE_FALLBACK: &str =
    "Tool execution completed, but the model returned no final text response. Please ask me to summarize the result.";

// ── WebSocket Sync Handler ──────────────────────────────────────────────

/// WebSocket endpoint for real-time cross-device memory sync.
///
/// Protocol: Devices exchange `BroadcastMessage` JSON frames.
/// The coordinator handles message dispatch and responds with
/// zero or more outbound messages per inbound frame.
pub async fn handle_ws_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let query_params = parse_ws_query_params(query.as_deref());

    // Auth
    if state.pairing.require_pairing() {
        let token =
            extract_ws_bearer_token(&headers, query_params.token.as_deref()).unwrap_or_default();
        if !state.pairing.is_authenticated(&token) {
            return (axum::http::StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    }

    let coordinator = match state.sync_coordinator.clone() {
        Some(c) => c,
        None => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Sync not enabled",
            )
                .into_response();
        }
    };

    ws.on_upgrade(move |socket| handle_sync_socket(socket, coordinator))
        .into_response()
}

/// Process sync WebSocket messages via the SyncCoordinator.
async fn handle_sync_socket(
    socket: WebSocket,
    coordinator: std::sync::Arc<crate::sync::SyncCoordinator>,
) {
    use futures_util::{SinkExt, StreamExt};
    let (mut sender, mut receiver) = socket.split();

    tracing::info!(
        device_id = %coordinator.device_id(),
        "Sync WebSocket connected"
    );

    // Send initial SyncRequest to catch up on missed deltas
    let initial_request = serde_json::json!({
        "SyncRequest": {
            "from_device_id": coordinator.device_id(),
            "version_vector": coordinator.version(),
        }
    });
    let _ = sender
        .send(Message::Text(initial_request.to_string().into()))
        .await;

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("Sync WS receive error: {e}");
                break;
            }
        };

        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => continue,
        };

        if text.is_empty() {
            continue;
        }

        let responses = coordinator.handle_message(&text).await;
        for response_json in responses {
            if sender
                .send(Message::Text(response_json.into()))
                .await
                .is_err()
            {
                tracing::debug!("Sync WS send error, closing");
                return;
            }
        }
    }

    tracing::info!(
        device_id = %coordinator.device_id(),
        "Sync WebSocket disconnected"
    );
}
const WS_HISTORY_MEMORY_KEY_PREFIX: &str = "gateway_ws_history";
const MAX_WS_PERSISTED_TURNS: usize = 128;
const MAX_WS_SESSION_ID_LEN: usize = 128;

#[derive(Debug, Default, PartialEq, Eq)]
struct WsQueryParams {
    token: Option<String>,
    session_id: Option<String>,
    target_device_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct WsHistoryTurn {
    role: String,
    content: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
struct WsPersistedHistory {
    version: u8,
    messages: Vec<WsHistoryTurn>,
}

fn normalize_ws_session_id(candidate: Option<&str>) -> Option<String> {
    let raw = candidate?.trim();
    if raw.is_empty() || raw.len() > MAX_WS_SESSION_ID_LEN {
        return None;
    }

    if raw
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Some(raw.to_string());
    }

    None
}

fn parse_ws_query_params(raw_query: Option<&str>) -> WsQueryParams {
    let Some(query) = raw_query else {
        return WsQueryParams::default();
    };

    let mut params = WsQueryParams::default();
    for kv in query.split('&') {
        let mut parts = kv.splitn(2, '=');
        let key = parts.next().unwrap_or("").trim();
        let value = parts.next().unwrap_or("").trim();
        if value.is_empty() {
            continue;
        }

        match key {
            "token" if params.token.is_none() => {
                params.token = Some(value.to_string());
            }
            "session_id" if params.session_id.is_none() => {
                params.session_id = normalize_ws_session_id(Some(value));
            }
            "target_device_id" if params.target_device_id.is_none() => {
                params.target_device_id = Some(value.to_string());
            }
            _ => {}
        }
    }

    params
}

fn ws_history_memory_key(session_id: &str) -> String {
    format!("{WS_HISTORY_MEMORY_KEY_PREFIX}:{session_id}")
}

fn ws_history_turns_from_chat(history: &[ChatMessage]) -> Vec<WsHistoryTurn> {
    let mut turns = history
        .iter()
        .filter_map(|msg| match msg.role.as_str() {
            "user" | "assistant" => {
                let content = msg.content.trim();
                if content.is_empty() {
                    None
                } else {
                    Some(WsHistoryTurn {
                        role: msg.role.clone(),
                        content: content.to_string(),
                    })
                }
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    if turns.len() > MAX_WS_PERSISTED_TURNS {
        let keep_from = turns.len().saturating_sub(MAX_WS_PERSISTED_TURNS);
        turns.drain(0..keep_from);
    }
    turns
}

fn restore_chat_history(system_prompt: &str, turns: &[WsHistoryTurn]) -> Vec<ChatMessage> {
    let mut history = vec![ChatMessage::system(system_prompt)];
    for turn in turns {
        match turn.role.as_str() {
            "user" => history.push(ChatMessage::user(&turn.content)),
            "assistant" => history.push(ChatMessage::assistant(&turn.content)),
            _ => {}
        }
    }
    history
}

async fn load_ws_history(
    state: &AppState,
    session_id: &str,
    system_prompt: &str,
) -> Vec<ChatMessage> {
    let key = ws_history_memory_key(session_id);
    let Some(entry) = state.mem.get(&key).await.ok().flatten() else {
        return vec![ChatMessage::system(system_prompt)];
    };

    let parsed = serde_json::from_str::<WsPersistedHistory>(&entry.content)
        .map(|history| history.messages)
        .or_else(|_| serde_json::from_str::<Vec<WsHistoryTurn>>(&entry.content));

    match parsed {
        Ok(turns) => restore_chat_history(system_prompt, &turns),
        Err(err) => {
            tracing::warn!(
                "Failed to parse persisted websocket history for session {}: {}",
                session_id,
                err
            );
            vec![ChatMessage::system(system_prompt)]
        }
    }
}

async fn persist_ws_history(state: &AppState, session_id: &str, history: &[ChatMessage]) {
    let payload = WsPersistedHistory {
        version: 1,
        messages: ws_history_turns_from_chat(history),
    };
    let serialized = match serde_json::to_string(&payload) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                "Failed to serialize websocket history for session {}: {}",
                session_id,
                err
            );
            return;
        }
    };

    let key = ws_history_memory_key(session_id);
    if let Err(err) = state
        .mem
        .store(
            &key,
            &serialized,
            MemoryCategory::Conversation,
            Some(session_id),
        )
        .await
    {
        tracing::warn!(
            "Failed to persist websocket history for session {}: {}",
            session_id,
            err
        );
    }
}

/// Build a recent conversation context string from the chat history.
/// Includes the last N user/assistant turns (excluding system messages)
/// so the agent loop has conversation continuity even though it creates
/// a fresh history per request.
/// Maximum number of recent conversation turns to include as context.
/// Covers ~20 messages (10 user + 10 assistant exchanges) for continuity.
const MAX_CONTEXT_TURNS: usize = 20;

/// Maximum total bytes for conversation context to avoid context window bloat.
const MAX_CONVERSATION_CONTEXT_BYTES: usize = 15_000;

fn build_recent_conversation_context(history: &[ChatMessage]) -> String {
    // Collect non-system turns (skip the system prompt at index 0)
    let turns: Vec<&ChatMessage> = history.iter().filter(|m| m.role != "system").collect();

    if turns.is_empty() {
        return String::new();
    }

    // Take only the most recent turns (leaving out the very last user message
    // which is the current one we're about to send)
    let context_turns = if turns.len() > 1 {
        let end = turns.len() - 1; // exclude the last (current) user message
        let start = end.saturating_sub(MAX_CONTEXT_TURNS);
        &turns[start..end]
    } else {
        return String::new();
    };

    if context_turns.is_empty() {
        return String::new();
    }

    let mut context = String::from("Recent conversation context:\n");
    let mut total_bytes = context.len();
    for turn in context_turns {
        let role_label = match turn.role.as_str() {
            "user" => "User",
            "assistant" => "Assistant",
            _ => continue,
        };
        // Truncate very long messages to keep context bounded
        let content = if turn.content.len() > 1000 {
            format!("{}...", &turn.content[..1000])
        } else {
            turn.content.clone()
        };
        let line = format!("{role_label}: {content}\n");
        if total_bytes + line.len() > MAX_CONVERSATION_CONTEXT_BYTES {
            break;
        }
        total_bytes += line.len();
        context.push_str(&line);
    }
    context.push('\n');
    context
}

fn sanitize_ws_response(
    response: &str,
    tools: &[Box<dyn crate::tools::Tool>],
    leak_guard: &crate::config::OutboundLeakGuardConfig,
) -> String {
    match crate::channels::sanitize_channel_response(response, tools, leak_guard) {
        crate::channels::ChannelSanitizationResult::Sanitized(sanitized) => {
            if sanitized.is_empty() && !response.trim().is_empty() {
                "I encountered malformed tool-call output and could not produce a safe reply. Please try again."
                    .to_string()
            } else {
                sanitized
            }
        }
        crate::channels::ChannelSanitizationResult::Blocked { .. } => {
            "I blocked a draft response because it appeared to contain credential material. Please ask for a redacted summary."
                .to_string()
        }
    }
}

fn normalize_prompt_tool_results(content: &str) -> Option<String> {
    let mut cleaned_lines: Vec<&str> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("<tool_result") || trimmed == "</tool_result>" {
            continue;
        }
        cleaned_lines.push(line.trim_end());
    }

    if cleaned_lines.is_empty() {
        None
    } else {
        Some(cleaned_lines.join("\n"))
    }
}

fn extract_latest_tool_output(history: &[ChatMessage]) -> Option<String> {
    for msg in history.iter().rev() {
        match msg.role.as_str() {
            "tool" => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                    if let Some(content) = value
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                    {
                        return Some(content.to_string());
                    }
                }

                let trimmed = msg.content.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            "user" => {
                if let Some(payload) = msg.content.strip_prefix("[Tool results]") {
                    let payload = payload.trim_start_matches('\n');
                    if let Some(cleaned) = normalize_prompt_tool_results(payload) {
                        return Some(cleaned);
                    }
                }
            }
            _ => {}
        }
    }

    None
}

fn finalize_ws_response(
    response: &str,
    history: &[ChatMessage],
    tools: &[Box<dyn crate::tools::Tool>],
    leak_guard: &crate::config::OutboundLeakGuardConfig,
) -> String {
    let sanitized = sanitize_ws_response(response, tools, leak_guard);
    if !sanitized.trim().is_empty() {
        return sanitized;
    }

    if let Some(tool_output) = extract_latest_tool_output(history) {
        let excerpt = crate::util::truncate_with_ellipsis(tool_output.trim(), 1200);
        return format!(
            "Tool execution completed, but the model returned no final text response.\n\nLatest tool output:\n{excerpt}"
        );
    }

    EMPTY_WS_RESPONSE_FALLBACK.to_string()
}

fn build_ws_system_prompt(
    config: &crate::config::Config,
    model: &str,
    tools_registry: &[Box<dyn crate::tools::Tool>],
    native_tools: bool,
) -> String {
    let mut tool_specs: Vec<crate::tools::ToolSpec> =
        tools_registry.iter().map(|tool| tool.spec()).collect();
    tool_specs.sort_by(|a, b| a.name.cmp(&b.name));

    let tool_descs: Vec<(&str, &str)> = tool_specs
        .iter()
        .map(|spec| (spec.name.as_str(), spec.description.as_str()))
        .collect();

    let bootstrap_max_chars = if config.agent.compact_context {
        Some(6000)
    } else {
        None
    };

    let mut prompt = crate::channels::build_system_prompt_with_mode(
        &config.workspace_dir,
        model,
        &tool_descs,
        &[],
        Some(&config.identity),
        bootstrap_max_chars,
        native_tools,
        config.skills.prompt_injection_mode,
    );

    // Inject API key inventory so the agent knows which providers/tools are available
    let api_key_inventory = crate::config::build_api_key_inventory(config);
    prompt.push_str(&api_key_inventory.to_prompt_section());

    if !native_tools {
        prompt.push_str(&build_tool_instructions_from_specs(&tool_specs));
    }
    prompt.push_str(&build_shell_policy_instructions(&config.autonomy));

    prompt
}

/// GET /ws/chat — WebSocket upgrade for agent chat
pub async fn handle_ws_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let query_params = parse_ws_query_params(query.as_deref());
    // Auth via Authorization header or websocket protocol token.
    let auth_token = extract_ws_bearer_token(&headers, query_params.token.as_deref())
        .unwrap_or_default()
        .to_string();
    if state.pairing.require_pairing() && !state.pairing.is_authenticated(&auth_token) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            "Unauthorized — provide Authorization: Bearer <token>, Sec-WebSocket-Protocol: bearer.<token>, or ?token=<token>",
        )
            .into_response();
    }

    // Resolve user_id from session token (for device routing).
    let user_id = state
        .auth_store
        .as_ref()
        .and_then(|store| store.validate_session(&auth_token))
        .map(|session| session.user_id);

    let session_id = query_params
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let target_device_id = query_params.target_device_id;

    ws.on_upgrade(move |socket| handle_socket(socket, state, session_id, user_id, target_device_id))
        .into_response()
}

/// Attempt to relay a chat message to the user's local MoA device.
///
/// Returns a `DeviceRelayResult` indicating the outcome:
///   - `Relayed`: message was sent to local device and response streamed back
///   - `NoDevice`: no local device online (caller should use operator key)
///   - `NoLocalKey`: device is online but has no valid API key (caller should use operator key)
///
/// Full flow:
///   1. Check if user's local device is online via DeviceRouter
///   2. Send a "check_key" probe to verify the device has a valid API key
///   3. If key exists: relay the actual message → device processes with local key (free)
///   4. If no key: return NoLocalKey → caller falls back to operator key (2.2× credits)
///   5. If no device online: return NoDevice → caller uses operator key (2.2× credits)
enum DeviceRelayResult {
    /// Message relayed to local device, response streamed back successfully.
    Relayed,
    /// No local device is online.
    NoDevice,
    /// Device is online but has no valid local API key for the requested provider.
    NoLocalKey,
}

async fn try_relay_to_local_device(
    state: &AppState,
    socket: &mut WebSocket,
    user_id: Option<&str>,
    content: &str,
    _session_id: &str,
    provider_name: &str,
    target_device_id: Option<&str>,
) -> DeviceRelayResult {
    // Need both device_router and auth_store to find user's devices
    let (device_router, auth_store) =
        match (state.device_router.as_ref(), state.auth_store.as_ref()) {
            (Some(dr), Some(auth)) => (dr, auth),
            _ => return DeviceRelayResult::NoDevice,
        };

    let user_id = match user_id {
        Some(uid) => uid,
        None => return DeviceRelayResult::NoDevice,
    };

    // Find user's devices and check if any are online
    let devices = match auth_store.list_devices(user_id) {
        Ok(devs) => devs,
        Err(_) => return DeviceRelayResult::NoDevice,
    };

    // Find the target device (if specified) or the first online device
    let online_device = if let Some(target_id) = target_device_id {
        devices
            .iter()
            .find(|d| d.device_id == target_id && device_router.is_device_online(&d.device_id))
    } else {
        devices
            .iter()
            .find(|d| device_router.is_device_online(&d.device_id))
    };

    let device = match online_device {
        Some(d) => d,
        None => return DeviceRelayResult::NoDevice,
    };

    // ── Step 1: Probe device for API key availability ──
    // Send a "check_key" message to verify the device has a valid API key
    // for the requested provider before committing to the full relay.
    {
        let probe_id = uuid::Uuid::new_v4().to_string();
        let (probe_tx, mut probe_rx) =
            tokio::sync::mpsc::channel::<super::remote::RoutedMessage>(4);

        {
            use super::remote::REMOTE_RESPONSE_CHANNELS;
            REMOTE_RESPONSE_CHANNELS
                .lock()
                .insert(probe_id.clone(), probe_tx);
        }

        let probe_msg = super::remote::RoutedMessage {
            id: probe_id.clone(),
            direction: "to_device".to_string(),
            content: serde_json::json!({
                "check_provider": provider_name,
            })
            .to_string(),
            msg_type: "check_key".to_string(),
        };

        if device_router
            .send_to_device(&device.device_id, probe_msg)
            .await
            .is_err()
        {
            use super::remote::REMOTE_RESPONSE_CHANNELS;
            REMOTE_RESPONSE_CHANNELS.lock().remove(&probe_id);
            return DeviceRelayResult::NoDevice;
        }

        // Wait up to 5 seconds for the probe response
        let probe_result =
            tokio::time::timeout(tokio::time::Duration::from_secs(5), probe_rx.recv()).await;

        // Clean up probe channel
        {
            use super::remote::REMOTE_RESPONSE_CHANNELS;
            REMOTE_RESPONSE_CHANNELS.lock().remove(&probe_id);
        }

        match probe_result {
            Ok(Some(resp)) => {
                // Device responded — check if it has a valid key
                if resp.msg_type == "key_missing" {
                    tracing::info!(
                        user_id = user_id,
                        device_id = device.device_id.as_str(),
                        provider = provider_name,
                        "Local device online but no API key for provider — falling back to operator key"
                    );
                    return DeviceRelayResult::NoLocalKey;
                }
                // "key_ok" or any other response means proceed with relay
            }
            _ => {
                // Timeout or channel closed — device may be busy, fall back
                tracing::debug!(
                    device_id = device.device_id.as_str(),
                    "Device did not respond to key probe — falling back to operator key"
                );
                return DeviceRelayResult::NoLocalKey;
            }
        }
    }

    // ── Step 2: Relay the actual message ──
    tracing::info!(
        user_id = user_id,
        device_id = device.device_id.as_str(),
        device_name = device.device_name.as_str(),
        "Relaying chat message to user's local device (local API key)"
    );

    let msg_id = uuid::Uuid::new_v4().to_string();
    let (resp_tx, mut resp_rx) = tokio::sync::mpsc::channel::<super::remote::RoutedMessage>(64);

    {
        use super::remote::REMOTE_RESPONSE_CHANNELS;
        REMOTE_RESPONSE_CHANNELS
            .lock()
            .insert(msg_id.clone(), resp_tx);
    }

    let routed_msg = super::remote::RoutedMessage {
        id: msg_id.clone(),
        direction: "to_device".to_string(),
        content: content.to_string(),
        msg_type: "message".to_string(),
    };

    if let Err(e) = device_router
        .send_to_device(&device.device_id, routed_msg)
        .await
    {
        tracing::warn!(
            error = e.as_str(),
            device_id = device.device_id.as_str(),
            "Failed to relay to local device"
        );
        {
            use super::remote::REMOTE_RESPONSE_CHANNELS;
            REMOTE_RESPONSE_CHANNELS.lock().remove(&msg_id);
        }
        return DeviceRelayResult::NoDevice;
    }

    // Wait for device responses and forward to the web client
    let timeout = tokio::time::Duration::from_secs(super::remote::DEVICE_RESPONSE_TIMEOUT_SECS_PUB);
    let deadline = tokio::time::Instant::now() + timeout;
    let mut got_response = false;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            if !got_response {
                let err = serde_json::json!({
                    "type": "error",
                    "message": "Local device did not respond in time. Please check that MoA is running on your device.",
                });
                let _ = socket.send(Message::Text(err.to_string().into())).await;
            }
            break;
        }

        match tokio::time::timeout(remaining, resp_rx.recv()).await {
            Ok(Some(resp)) => {
                got_response = true;
                let frame = serde_json::json!({
                    "type": resp.msg_type,
                    "content": resp.content,
                });
                let _ = socket.send(Message::Text(frame.to_string().into())).await;
                if resp.msg_type == "done" {
                    break;
                }
            }
            Ok(None) => {
                if !got_response {
                    let err = serde_json::json!({
                        "type": "error",
                        "message": "Local device disconnected during processing.",
                    });
                    let _ = socket.send(Message::Text(err.to_string().into())).await;
                }
                break;
            }
            Err(_) => {
                if !got_response {
                    let err = serde_json::json!({
                        "type": "error",
                        "message": "Local device did not respond in time.",
                    });
                    let _ = socket.send(Message::Text(err.to_string().into())).await;
                }
                break;
            }
        }
    }

    {
        use super::remote::REMOTE_RESPONSE_CHANNELS;
        REMOTE_RESPONSE_CHANNELS.lock().remove(&msg_id);
    }

    if got_response {
        DeviceRelayResult::Relayed
    } else {
        DeviceRelayResult::NoDevice
    }
}

/// Resolve the operator's admin LLM API key from environment variables.
///
/// Railway always has these pre-configured by the operator.
fn resolve_operator_llm_key(provider_name: &str) -> Option<String> {
    let admin_env_var = match provider_name {
        "anthropic" => "ADMIN_ANTHROPIC_API_KEY",
        "openai" => "ADMIN_OPENAI_API_KEY",
        "gemini" | "google" | "google-gemini" => "ADMIN_GEMINI_API_KEY",
        "openrouter" => "ADMIN_OPENROUTER_API_KEY",
        "deepseek" => "ADMIN_DEEPSEEK_API_KEY",
        "groq" => "ADMIN_GROQ_API_KEY",
        "mistral" => "ADMIN_MISTRAL_API_KEY",
        "xai" | "grok" => "ADMIN_XAI_API_KEY",
        "perplexity" => "ADMIN_PERPLEXITY_API_KEY",
        "together" | "together-ai" => "ADMIN_TOGETHER_API_KEY",
        "fireworks" | "fireworks-ai" => "ADMIN_FIREWORKS_API_KEY",
        "cohere" => "ADMIN_COHERE_API_KEY",
        "venice" => "ADMIN_VENICE_API_KEY",
        _ => return None,
    };
    std::env::var(admin_env_var)
        .ok()
        .filter(|k| !k.trim().is_empty())
}

/// Hybrid relay: send message to the local device with a short-lived proxy token.
///
/// **Security**: The operator's LLM API key NEVER leaves the Railway server.
/// Instead, we issue a short-lived session token that the device uses to call
/// Railway's `/api/llm/proxy` endpoint for LLM requests. Railway then adds
/// the operator key server-side before forwarding to the LLM provider.
///
/// This ensures:
///   - Operator API key stays on the server (cannot be extracted by device)
///   - Local device uses its own tool API keys for all tool execution
///   - Local device applies its own config/settings
///   - LLM usage is tracked and credits deducted server-side (2.2×)
///   - Proxy token expires in 15 minutes (single-conversation scope)
///
/// The device receives a `hybrid_relay` message containing:
///   - `content`: the user's message
///   - `provider`: which LLM provider to use
///   - `proxy_token`: short-lived token for `/api/llm/proxy` calls
///   - `proxy_url`: the Railway LLM proxy endpoint URL
///
/// The device-side agent loop uses this token to make LLM calls through
/// the proxy while executing tools locally with its own API keys.
const HYBRID_PROXY_TOKEN_TTL_SECS: u64 = 15 * 60; // 15 minutes

async fn try_relay_to_local_device_with_proxy(
    state: &AppState,
    socket: &mut WebSocket,
    user_id: Option<&str>,
    content: &str,
    _session_id: &str,
    provider_name: &str,
    target_device_id: Option<&str>,
) -> DeviceRelayResult {
    let (device_router, auth_store) =
        match (state.device_router.as_ref(), state.auth_store.as_ref()) {
            (Some(dr), Some(auth)) => (dr, auth),
            _ => return DeviceRelayResult::NoDevice,
        };

    let user_id = match user_id {
        Some(uid) => uid,
        None => return DeviceRelayResult::NoDevice,
    };

    let devices = match auth_store.list_devices(user_id) {
        Ok(devs) => devs,
        Err(_) => return DeviceRelayResult::NoDevice,
    };

    let device = if let Some(target_id) = target_device_id {
        match devices
            .iter()
            .find(|d| d.device_id == target_id && device_router.is_device_online(&d.device_id))
        {
            Some(d) => d,
            None => return DeviceRelayResult::NoDevice,
        }
    } else {
        match devices
            .iter()
            .find(|d| device_router.is_device_online(&d.device_id))
        {
            Some(d) => d,
            None => return DeviceRelayResult::NoDevice,
        }
    };

    // Issue a short-lived proxy token (15 minutes) for LLM proxy calls.
    // This token can ONLY be used to call /api/llm/proxy — it cannot
    // extract the operator's API key.
    let proxy_token = match auth_store.create_session_with_ttl(
        user_id,
        Some(&device.device_id),
        Some(&device.device_name),
        HYBRID_PROXY_TOKEN_TTL_SECS,
    ) {
        Ok(token) => token,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to create proxy token for hybrid relay");
            return DeviceRelayResult::NoDevice;
        }
    };

    // Determine the Railway gateway's public URL for the LLM proxy endpoint.
    // Priority: RAILWAY_PUBLIC_DOMAIN env > gateway host:port fallback.
    let proxy_url = {
        let base = if let Ok(domain) = std::env::var("RAILWAY_PUBLIC_DOMAIN") {
            format!("https://{}", domain.trim_end_matches('/'))
        } else {
            let config_guard = state.config.lock();
            let host = &config_guard.gateway.host;
            let port = config_guard.gateway.port;
            format!("http://{}:{}", host, port)
        };
        format!("{}/api/llm/proxy", base.trim_end_matches('/'))
    };

    tracing::info!(
        user_id = user_id,
        device_id = device.device_id.as_str(),
        device_name = device.device_name.as_str(),
        provider = provider_name,
        proxy_token_ttl_secs = HYBRID_PROXY_TOKEN_TTL_SECS,
        "Hybrid relay: sending message with proxy token (operator key stays on server)"
    );

    let msg_id = uuid::Uuid::new_v4().to_string();
    let (resp_tx, mut resp_rx) = tokio::sync::mpsc::channel::<super::remote::RoutedMessage>(64);

    {
        use super::remote::REMOTE_RESPONSE_CHANNELS;
        REMOTE_RESPONSE_CHANNELS
            .lock()
            .insert(msg_id.clone(), resp_tx);
    }

    // Send the hybrid relay message.
    // ★ SECURITY: No API key in this message — only a short-lived proxy token.
    let routed_msg = super::remote::RoutedMessage {
        id: msg_id.clone(),
        direction: "to_device".to_string(),
        content: serde_json::json!({
            "content": content,
            "provider": provider_name,
            "proxy_token": proxy_token,
            "proxy_url": proxy_url,
        })
        .to_string(),
        msg_type: "hybrid_relay".to_string(),
    };

    if let Err(e) = device_router
        .send_to_device(&device.device_id, routed_msg)
        .await
    {
        tracing::warn!(
            error = e.as_str(),
            "Failed to send hybrid relay message to device"
        );
        use super::remote::REMOTE_RESPONSE_CHANNELS;
        REMOTE_RESPONSE_CHANNELS.lock().remove(&msg_id);
        return DeviceRelayResult::NoDevice;
    }

    // Stream responses from the device back to the web client
    let mut got_response = false;
    loop {
        match tokio::time::timeout(tokio::time::Duration::from_secs(120), resp_rx.recv()).await {
            Ok(Some(resp)) => {
                got_response = true;
                let ws_msg = serde_json::json!({
                    "type": resp.msg_type,
                    "content": resp.content,
                });
                let _ = socket.send(Message::Text(ws_msg.to_string().into())).await;
                if resp.msg_type == "done" || resp.msg_type == "error" {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                if !got_response {
                    let err = serde_json::json!({
                        "type": "error",
                        "message": "Local device did not respond in time (hybrid relay).",
                    });
                    let _ = socket.send(Message::Text(err.to_string().into())).await;
                }
                break;
            }
        }
    }

    {
        use super::remote::REMOTE_RESPONSE_CHANNELS;
        REMOTE_RESPONSE_CHANNELS.lock().remove(&msg_id);
    }

    if got_response {
        DeviceRelayResult::Relayed
    } else {
        DeviceRelayResult::NoDevice
    }
}

async fn handle_socket(
    mut socket: WebSocket,
    state: AppState,
    session_id: String,
    user_id: Option<String>,
    target_device_id: Option<String>,
) {
    let ws_session_id = format!("ws_{}", Uuid::new_v4());

    // Build system prompt once for the session
    let system_prompt = {
        let config_guard = state.config.lock();
        build_ws_system_prompt(
            &config_guard,
            &state.model,
            state.tools_registry_exec.as_ref(),
            state.provider.supports_native_tools(),
        )
    };

    // Restore persisted history (if any) and replay to the client before processing new input.
    let mut history = load_ws_history(&state, &session_id, &system_prompt).await;
    let persisted_turns = ws_history_turns_from_chat(&history);
    let history_payload = serde_json::json!({
        "type": "history",
        "session_id": session_id.as_str(),
        "messages": persisted_turns,
    });
    let _ = socket
        .send(Message::Text(history_payload.to_string().into()))
        .await;

    while let Some(msg) = socket.recv().await {
        let msg = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };

        // Parse incoming message
        let parsed: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => {
                let err = serde_json::json!({"type": "error", "message": "Invalid JSON"});
                let _ = socket.send(Message::Text(err.to_string().into())).await;
                continue;
            }
        };

        let msg_type = parsed["type"].as_str().unwrap_or("");
        if msg_type != "message" {
            continue;
        }

        let content = parsed["content"].as_str().unwrap_or("").to_string();
        if content.is_empty() {
            continue;
        }

        // ── SLM-first gatekeeper (★ MoA core workflow) ──
        //
        // Symmetrical to the block in openclaw_compat::handle_api_chat.
        // Runs BEFORE the device-relay attempts so the cheapest (on-device
        // SLM) path short-circuits the network probe / proxy-token issuance
        // whenever the router can answer locally. Only when SLM declines
        // does the message enter the existing Smart API Key Routing
        // (local-key → operator-key with 2.2× credit deduction).
        //
        // The decision is preserved in `ws_gatekeeper_decision` so the
        // Advisor PLAN/REVIEW blocks downstream can route by the same
        // `TaskCategory` the gatekeeper assigned.
        let mut ws_gatekeeper_decision: Option<
            crate::gatekeeper::router::RoutingDecision,
        > = None;
        if let Some(router) = state.gatekeeper.as_ref() {
            let result = router.process_message(&content).await;
            if let Some(local_reply) = result.local_response {
                tracing::info!(
                    category = ?result.decision.category,
                    confidence = result.decision.confidence,
                    reason = %result.decision.reason,
                    "WS SLM gatekeeper answered locally — skipping LLM relay"
                );
                history.push(ChatMessage::user(&content));
                history.push(ChatMessage::assistant(&local_reply));
                persist_ws_history(&state, &session_id, &history).await;

                let done = serde_json::json!({
                    "type": "done",
                    "full_response": local_reply,
                    "session_id": session_id.as_str(),
                    "active_provider": "ollama",
                    "active_model": router.model(),
                    "is_local_path": true,
                    "network_status": "local",
                    "gatekeeper": {
                        "category": format!("{:?}", result.decision.category),
                        "confidence": result.decision.confidence,
                        "reason": result.decision.reason,
                    },
                });
                let _ = socket.send(Message::Text(done.to_string().into())).await;
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "agent_end",
                    "provider": "ollama",
                    "model": router.model(),
                    "is_local_path": true,
                    "network_status": "local",
                }));
                continue;
            }
            tracing::debug!(
                category = ?result.decision.category,
                confidence = result.decision.confidence,
                reason = %result.decision.reason,
                "WS SLM gatekeeper summoning LLM for complex task"
            );
            ws_gatekeeper_decision = Some(result.decision);
        }

        // ── Apply client-provided overrides (provider, model) ──
        {
            let mut config_guard = state.config.lock();

            if let Some(client_provider) =
                parsed["provider"].as_str().filter(|p| !p.trim().is_empty())
            {
                let backend_provider = match client_provider {
                    "claude" => "anthropic",
                    p => p,
                };
                config_guard.default_provider = Some(backend_provider.to_string());
            }

            if let Some(client_model) = parsed["model"].as_str().filter(|m| !m.trim().is_empty()) {
                config_guard.default_model = Some(client_model.to_string());
            }
        }

        // ── ★ MoA Smart API Key Routing ──
        // Priority: LOCAL device key FIRST → operator key SECOND.
        //
        // 1. Always try user's local device first (free for user)
        // 2. Only fall back to operator key if local device is offline
        //    or has no valid key (credits deducted at 2.2×)
        //
        // Railway ALWAYS has operator API keys pre-configured, so
        // "credential_missing" on Railway should never happen in normal
        // operation. The key question is whether to use the FREE local
        // key or the PAID operator key.

        let provider_name = {
            let config_guard = state.config.lock();
            config_guard
                .default_provider
                .clone()
                .unwrap_or_else(|| "gemini".to_string())
        };

        // ── Step 1: Try user's local device FIRST (free path) ──
        // Check if user's local MoA device is online AND has a valid
        // API key for the requested provider. If so, relay the message
        // to the local device — the user pays nothing.
        let relay_result = try_relay_to_local_device(
            &state,
            &mut socket,
            user_id.as_deref(),
            &content,
            &session_id,
            &provider_name,
            target_device_id.as_deref(),
        )
        .await;

        match relay_result {
            DeviceRelayResult::Relayed => {
                // ✅ Response streamed from local device using local key.
                // No cost to user. Skip all server-side processing.
                continue;
            }
            DeviceRelayResult::NoLocalKey => {
                // ── Step 1b: Hybrid relay — device online but no LLM key ──
                // The local device has tools with their own API keys and settings,
                // but lacks an LLM API key. We MUST still route processing through
                // the local device so that:
                //   - All local tool API keys (web search, browser, composio, etc.) are used
                //   - All local settings/config are applied
                //   - Only LLM calls go through Railway's /api/llm/proxy endpoint
                //
                // ★ SECURITY: We do NOT send the operator's API key to the device.
                // Instead, we issue a short-lived proxy token (15 min) that the
                // device uses to call Railway's /api/llm/proxy. Railway adds the
                // operator key server-side — the key never leaves the server.
                let has_operator_key = resolve_operator_llm_key(&provider_name).is_some();
                if has_operator_key {
                    tracing::info!(
                        provider = provider_name.as_str(),
                        "Hybrid relay: device online, issuing proxy token (key stays on server)"
                    );
                    let hybrid_result = try_relay_to_local_device_with_proxy(
                        &state,
                        &mut socket,
                        user_id.as_deref(),
                        &content,
                        &session_id,
                        &provider_name,
                        target_device_id.as_deref(),
                    )
                    .await;
                    match hybrid_result {
                        DeviceRelayResult::Relayed => {
                            // ✅ Local device processed with proxy token + local tool keys.
                            // Credits deducted at 2.2× server-side via /api/llm/proxy.
                            continue;
                        }
                        _ => {
                            // Hybrid relay failed — fall through to full Railway processing
                            tracing::warn!(
                                "Hybrid relay failed, falling back to full Railway processing"
                            );
                        }
                    }
                }
                // No operator key or hybrid relay failed — fall through to Railway
            }
            DeviceRelayResult::NoDevice => {
                // Device offline — fall through to Step 2: use operator key on Railway.
            }
        }

        // ── Step 2: Full Railway processing (paid path, 2.2×) ──
        // Only reached when:
        //   - Device is completely offline, OR
        //   - Hybrid relay failed
        // In this case Railway handles BOTH LLM and tool execution.
        // ⚠️  Local tool API keys and settings are NOT used in this path.
        //
        // Resolve API key: client-provided > provider_api_keys > env > admin key.
        {
            let mut config_guard = state.config.lock();

            let client_key = parsed["api_key"]
                .as_str()
                .map(str::trim)
                .filter(|k| !k.is_empty());

            if let Some(key) = client_key {
                // Client explicitly provided a key in the message
                config_guard.api_key = Some(key.to_string());
            } else if let Some(stored_key) =
                config_guard.provider_api_keys.get(&provider_name).cloned()
            {
                if stored_key.trim().is_empty() {
                    config_guard.api_key = None;
                } else {
                    config_guard.api_key = Some(stored_key);
                }
            } else {
                config_guard.api_key = None;
            }
        }

        // Validate that we have a credential for the provider
        let credential_missing = {
            let config_guard = state.config.lock();
            if crate::providers::provider_requires_credential(&provider_name) {
                let has_key = crate::providers::has_provider_credential(
                    &provider_name,
                    config_guard.api_key.as_deref(),
                );
                !has_key
            } else {
                false
            }
        };

        if credential_missing {
            // Try ADMIN_*_API_KEY env vars (operator's pre-configured keys)
            if let Some(key) = resolve_operator_llm_key(&provider_name) {
                tracing::info!(
                    provider = provider_name.as_str(),
                    "Full Railway processing: using operator API key (credits 2.2×)"
                );
                let mut config_guard = state.config.lock();
                config_guard.api_key = Some(key);
                // Fall through to normal LLM processing below
            } else {
                // No key at all — shouldn't happen if operator set up Railway
                let env_hint = match provider_name.as_str() {
                    "anthropic" => "ANTHROPIC_API_KEY",
                    "openai" => "OPENAI_API_KEY",
                    "gemini" | "google" | "google-gemini" => "GEMINI_API_KEY",
                    _ => "<PROVIDER>_API_KEY",
                };
                let err = serde_json::json!({
                    "type": "error",
                    "code": "missing_api_key",
                    "message": format!(
                        "No API key configured for provider '{}'. Please add your API key in Settings or set {} env var.",
                        provider_name, env_hint
                    ),
                });
                let _ = socket.send(Message::Text(err.to_string().into())).await;
                continue;
            }
        }

        let perplexity_cfg = { state.config.lock().security.perplexity_filter.clone() };
        if let Some(assessment) =
            crate::security::detect_adversarial_suffix(&content, &perplexity_cfg)
        {
            let err = serde_json::json!({
                "type": "error",
                "message": format!(
                    "Input blocked by security.perplexity_filter: perplexity={:.2} (threshold {:.2}), symbol_ratio={:.2} (threshold {:.2}), suspicious_tokens={}.",
                    assessment.perplexity,
                    perplexity_cfg.perplexity_threshold,
                    assessment.symbol_ratio,
                    perplexity_cfg.symbol_ratio_threshold,
                    assessment.suspicious_token_count
                ),
            });
            let _ = socket.send(Message::Text(err.to_string().into())).await;
            continue;
        }

        // Add user message to history
        history.push(ChatMessage::user(&content));
        persist_ws_history(&state, &session_id, &history).await;

        // Get provider info
        let provider_label = state
            .config
            .lock()
            .default_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // Broadcast agent_start event
        let _ = state.event_tx.send(serde_json::json!({
            "type": "agent_start",
            "provider": provider_label,
            "model": state.model,
        }));

        // Build recent conversation context so the agent loop has continuity.
        // The agent creates a fresh LLM history per request, so without this
        // context the model has no knowledge of prior turns in this session.
        let recent_context = build_recent_conversation_context(&history);
        let mut enriched_content = if recent_context.is_empty() {
            content.clone()
        } else {
            format!("{recent_context}Current message:\n{content}")
        };

        // ── Advisor PLAN checkpoint (WS) ──
        // Symmetric to handle_api_chat — runs only when the gatekeeper
        // flagged the task as Complex/Specialized and the advisor is
        // configured. Prepends the plan block to `enriched_content`.
        let mut ws_advisor_plan: Option<crate::advisor::PlanOutput> = None;
        if let (Some(advisor), Some(decision)) =
            (state.advisor.as_ref(), ws_gatekeeper_decision.as_ref())
        {
            let policy = crate::advisor::AdvisorPolicy::for_category(decision.category);
            if policy.plan {
                let kind = crate::advisor::TaskKind::infer(
                    decision.category,
                    decision.tool_needed.as_deref(),
                    &content,
                );
                let req = crate::advisor::AdvisorRequest {
                    task_summary: &content,
                    background: "",
                    recent_output: "",
                    question: "Produce a strategic plan for this WS user request before execution.",
                    kind,
                };
                match advisor.plan(&req).await {
                    Ok(plan) => {
                        let steps = plan
                            .critical_path
                            .iter()
                            .enumerate()
                            .map(|(i, s)| format!("  {}. {}", i + 1, s))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let tools_hint = if plan.suggested_tools.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "\nSuggested Tools (use these first):\n{}\n",
                                plan.suggested_tools
                                    .iter()
                                    .map(|t| format!("  - {t}"))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            )
                        };
                        let plan_block = format!(
                            "[Advisor Plan — follow this strategy]\n\
                             End State: {}\n\
                             First Move: {}\n\
                             Critical Path:\n{}{}\n\
                             ---\n\n",
                            plan.end_state, plan.first_move, steps, tools_hint,
                        );
                        enriched_content = format!("{plan_block}{enriched_content}");
                        ws_advisor_plan = Some(plan);
                        tracing::info!(
                            model = advisor.model(),
                            kind = kind.label(),
                            "WS Advisor PLAN checkpoint completed"
                        );
                    }
                    Err(e) => tracing::warn!(
                        error = %e,
                        "WS Advisor PLAN failed — proceeding without plan"
                    ),
                }
            }
        }

        // ── Phase 3 — SLM executor attempt (WS) ──
        // Symmetric to handle_api_chat: try the on-device SLM executor
        // first for Medium / tool-hinted tasks. On success, short-circuit
        // the cloud agent loop and hand the SLM's answer to the advisor
        // REVIEW checkpoint downstream. On exceeded-iterations / error,
        // fall through to `run_gateway_chat_with_tools` exactly as before.
        let mut ws_slm_reply: Option<String> = None;
        let mut ws_slm_tools: Vec<String> = Vec::new();
        if let (Some(executor), Some(decision)) = (
            state.slm_executor.as_ref(),
            ws_gatekeeper_decision.as_ref(),
        ) {
            let eligible = matches!(
                decision.category,
                crate::gatekeeper::router::TaskCategory::Medium
            ) || decision.tool_needed.is_some();
            if eligible {
                // Same safe_for_slm filtering as the REST path — the
                // on-device SLM never sees shell/delegate/file_write etc.
                let tool_refs: Vec<&dyn crate::tools::Tool> = state
                    .tools_registry_exec
                    .as_ref()
                    .iter()
                    .filter_map(|boxed| {
                        let t = boxed.as_ref();
                        t.safe_for_slm().then_some(t)
                    })
                    .collect();
                match executor.run(&enriched_content, &tool_refs).await {
                    Ok(outcome) if !outcome.exceeded_iterations => {
                        tracing::info!(
                            iterations = outcome.iterations,
                            tools = outcome.tools_invoked.len(),
                            "WS SLM executor closed the task locally"
                        );
                        ws_slm_tools = outcome.tools_invoked;
                        ws_slm_reply = Some(outcome.reply);
                    }
                    Ok(_) => tracing::info!(
                        "WS SLM executor exceeded iteration budget — falling back to cloud LLM"
                    ),
                    Err(e) => tracing::warn!(
                        error = %e,
                        "WS SLM executor errored — falling back to cloud LLM"
                    ),
                }
            }
        }

        // Full agentic loop with tools (includes WASM skills, shell, memory, etc.)
        let agent_outcome = if let Some(r) = ws_slm_reply.clone() {
            Ok(r)
        } else {
            Box::pin(super::run_gateway_chat_with_tools(
                &state,
                &enriched_content,
                Some(&ws_session_id),
            ))
            .await
        };
        match agent_outcome {
            Ok(response) => {
                let leak_guard_cfg = { state.config.lock().security.outbound_leak_guard.clone() };
                let safe_response = finalize_ws_response(
                    &response,
                    &history,
                    state.tools_registry_exec.as_ref(),
                    &leak_guard_cfg,
                );

                // ── Advisor REVIEW checkpoint (WS) ──
                // No revision loop in WS path for now — the /ws pipeline
                // streams partial tokens, so a silent rerun would confuse
                // the client. When the advisor blocks or flags revision
                // we surface the verdict in `done.advisor` and (on Block)
                // prepend a warning banner to `full_response`.
                let mut ws_advisor_review: Option<crate::advisor::ReviewOutput> = None;
                if let (Some(advisor), Some(decision)) =
                    (state.advisor.as_ref(), ws_gatekeeper_decision.as_ref())
                {
                    let policy = crate::advisor::AdvisorPolicy::for_category(decision.category);
                    if policy.review {
                        let kind = crate::advisor::TaskKind::infer(
                            decision.category,
                            decision.tool_needed.as_deref(),
                            &content,
                        );
                        let plan_background = ws_advisor_plan
                            .as_ref()
                            .map(|p| {
                                format!(
                                    "Plan end state: {}\nFirst move: {}",
                                    p.end_state, p.first_move
                                )
                            })
                            .unwrap_or_default();
                        let req = crate::advisor::AdvisorRequest {
                            task_summary: &content,
                            background: plan_background.as_str(),
                            recent_output: &safe_response,
                            question: "Review the executor's answer for correctness, architecture, security, and silent failures.",
                            kind,
                        };
                        match advisor.review(&req).await {
                            Ok(review) => {
                                tracing::info!(
                                    verdict = ?review.verdict,
                                    "WS Advisor REVIEW checkpoint completed"
                                );
                                ws_advisor_review = Some(review);
                            }
                            Err(e) => tracing::warn!(
                                error = %e,
                                "WS Advisor REVIEW failed — returning raw answer"
                            ),
                        }
                    }
                }

                let safe_response = if ws_advisor_review
                    .as_ref()
                    .is_some_and(|r| r.verdict == crate::advisor::ReviewVerdict::Block)
                {
                    format!(
                        "⚠️ Advisor flagged this answer — review before relying on it.\n\n{safe_response}"
                    )
                } else {
                    safe_response
                };

                // Add assistant response to history
                history.push(ChatMessage::assistant(&safe_response));
                persist_ws_history(&state, &session_id, &history).await;

                // ── Active-provider metadata (PR #3.5) ──
                // Same shape as `handle_api_chat` so the same client-side
                // badge logic works for both REST and WebSocket flows.
                let net_online = crate::local_llm::shared_health().is_online();
                let is_local_path = provider_label.eq_ignore_ascii_case("ollama");

                // Attach advisor review metadata so the WS client can
                // render a "reviewed / blocked / needs revision" badge
                // matching the REST /api/chat response shape.
                let advisor_meta = ws_advisor_review.as_ref().map(|r| {
                    serde_json::json!({
                        "verdict": format!("{:?}", r.verdict).to_ascii_lowercase(),
                        "summary": r.summary,
                        "correctness_issues": r.correctness_issues,
                        "architecture_concerns": r.architecture_concerns,
                        "security_flags": r.security_flags,
                        "silent_failures": r.silent_failures,
                        "model": state.advisor.as_ref().map(|a| a.model().to_string()),
                    })
                });

                let slm_meta = ws_slm_reply.as_ref().map(|_| {
                    serde_json::json!({
                        "used": true,
                        "model": state.slm_executor.as_ref().map(|e| e.model().to_string()),
                        "tools_invoked": ws_slm_tools,
                    })
                });

                // Send the full response as a done message
                let done = serde_json::json!({
                    "type": "done",
                    "full_response": safe_response,
                    "active_provider": if ws_slm_reply.is_some() { "ollama" } else { provider_label.as_str() },
                    "active_model": state.model,
                    "is_local_path": is_local_path || ws_slm_reply.is_some(),
                    "network_status": if net_online { "online" } else { "offline" },
                    "advisor": advisor_meta,
                    "slm_executor": slm_meta,
                });
                let _ = socket.send(Message::Text(done.to_string().into())).await;

                // Broadcast agent_end event
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "agent_end",
                    "provider": provider_label,
                    "model": state.model,
                    "is_local_path": is_local_path,
                    "network_status": if net_online { "online" } else { "offline" },
                }));
            }
            Err(e) => {
                let sanitized = crate::providers::sanitize_api_error(&e.to_string());

                // Detect provider authentication errors (401 Unauthorized) so
                // the client can fall back to relay or prompt the user.
                let is_auth_error = sanitized.contains("401")
                    || sanitized.contains("Unauthorized")
                    || sanitized.contains("authentication");

                let provider_label = state
                    .config
                    .lock()
                    .default_provider
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());

                let err = if is_auth_error {
                    serde_json::json!({
                        "type": "error",
                        "code": "provider_auth_error",
                        "message": format!(
                            "API key for '{}' is invalid or expired. Please update your API key in Settings.",
                            provider_label
                        ),
                        "detail": sanitized,
                        "fallback_to_relay": true,
                    })
                } else {
                    serde_json::json!({
                        "type": "error",
                        "message": sanitized,
                    })
                };
                let _ = socket.send(Message::Text(err.to_string().into())).await;

                // Broadcast error event
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "error",
                    "component": "ws_chat",
                    "message": sanitized,
                }));
            }
        }
    }
}

fn extract_ws_bearer_token(headers: &HeaderMap, query_token: Option<&str>) -> Option<String> {
    if let Some(auth_header) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    {
        if let Some(token) = auth_header.strip_prefix("Bearer ") {
            if !token.trim().is_empty() {
                return Some(token.trim().to_string());
            }
        }
    }

    if let Some(offered) = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
    {
        for protocol in offered.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(token) = protocol.strip_prefix("bearer.") {
                if !token.trim().is_empty() {
                    return Some(token.trim().to_string());
                }
            }
        }
    }

    query_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
}

fn extract_query_token(raw_query: Option<&str>) -> Option<String> {
    parse_ws_query_params(raw_query).token
}

// ── Voice interpretation WebSocket ────────────────────────────────

/// GET /ws/voice — WebSocket upgrade for simultaneous interpretation
pub async fn handle_ws_voice(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let query_params = parse_ws_query_params(query.as_deref());

    // Auth via Authorization header or websocket protocol token.
    if state.pairing.require_pairing() {
        let token =
            extract_ws_bearer_token(&headers, query_params.token.as_deref()).unwrap_or_default();
        if !state.pairing.is_authenticated(&token) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization: Bearer <token>, Sec-WebSocket-Protocol: bearer.<token>, or ?token=<token>",
            )
                .into_response();
        }
    }

    // Check voice feature is enabled
    {
        let config_guard = state.config.lock();
        if !config_guard.voice.enabled {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Voice interpretation is disabled",
            )
                .into_response();
        }
    }

    ws.on_upgrade(move |socket| handle_voice_socket(socket, state))
        .into_response()
}

/// Voice WebSocket handler — manages a simultaneous interpretation session.
///
/// Protocol:
/// ```text
/// Client -> Server: {"type":"session_start","sessionId":"...","sourceLang":"ko","targetLang":"en",...}
/// Client -> Server: {"type":"audio_chunk","sessionId":"...","seq":0,"ts":...,"pcm16le":"base64..."}
/// Client -> Server: {"type":"session_stop","sessionId":"..."}
/// Server -> Client: {"type":"session_ready","sessionId":"...","liveSessionId":"..."}
/// Server -> Client: {"type":"partial_src","sessionId":"...","text":"...","stablePrefixLen":5,"final":false}
/// Server -> Client: {"type":"commit_src","sessionId":"...","commitId":1,"text":"..."}
/// Server -> Client: {"type":"commit_tgt","sessionId":"...","commitId":1,"text":"..."}
/// Server -> Client: {"type":"audio_out","sessionId":"...","seq":0,"pcm16le":"base64..."}
/// Server -> Client: {"type":"session_ended","sessionId":"...","totalSegments":5}
/// ```
/// Unified session handle — wraps either Gemini-based or Deepgram-based session.
///
/// Both providers share the same `send_audio` / `event_rx` / `stop` interface,
/// so this enum lets the voice WebSocket handler route to either without duplication.
enum VoiceSessionHandle {
    Gemini(crate::voice::simul_session::SimulSession),
    Deepgram(crate::voice::deepgram_simul::DeepgramSimulSession),
    Gemma(crate::voice::gemma_simul::GemmaSimulSession),
    TypecastPipeline(crate::voice::typecast_interp::TypecastInterpSession),
}

impl VoiceSessionHandle {
    async fn send_audio(&self, pcm_data: Vec<u8>) -> anyhow::Result<()> {
        match self {
            Self::Gemini(s) => s.send_audio(pcm_data).await,
            Self::Deepgram(s) => s.send_audio(pcm_data).await,
            Self::Gemma(s) => s.send_audio(pcm_data).await,
            Self::TypecastPipeline(s) => s.send_audio(pcm_data).await,
        }
    }

    fn event_rx(
        &self,
    ) -> std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<crate::voice::events::ServerMessage>>>
    {
        match self {
            Self::Gemini(s) => s.event_rx.clone(),
            Self::Deepgram(s) => s.event_rx.clone(),
            Self::Gemma(s) => s.event_rx.clone(),
            Self::TypecastPipeline(s) => s.event_rx.clone(),
        }
    }

    async fn stop(&self) {
        match self {
            Self::Gemini(s) => s.stop().await,
            Self::Deepgram(s) => s.stop().await,
            Self::Gemma(s) => s.stop().await,
            Self::TypecastPipeline(s) => s.stop().await,
        }
    }
}

async fn handle_voice_socket(mut socket: WebSocket, state: AppState) {
    use crate::voice::{
        deepgram_simul::{DeepgramSimulConfig, DeepgramSimulSession},
        events::{ClientMessage, ServerMessage},
        gemma_simul::{GemmaSimulConfig, GemmaSimulSession},
        pipeline::{Domain, Formality, LanguageCode, VoiceAge, VoiceGender},
        simul::SegmentationConfig,
        simul_session::{SimulSession, SimulSessionConfig},
        typecast_interp::{TypecastInterpConfig, TypecastInterpSession},
    };
    use base64::Engine;

    // Read voice config for session defaults
    let voice_config = {
        let config_guard = state.config.lock();
        config_guard.voice.clone()
    };

    // Active session handle — set when SessionStart is received
    let mut active_session: Option<VoiceSessionHandle> = None;
    // Event relay task handle — cancelled when session stops
    let mut relay_handle: Option<tokio::task::JoinHandle<()>> = None;

    while let Some(msg) = socket.recv().await {
        let text = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                let err = ServerMessage::Error {
                    session_id: String::new(),
                    code: "INVALID_MESSAGE".into(),
                    message: format!("Invalid message: {e}"),
                };
                let _ = socket
                    .send(Message::Text(
                        serde_json::to_string(&err).unwrap_or_default().into(),
                    ))
                    .await;
                continue;
            }
        };

        match client_msg {
            ClientMessage::SessionStart {
                session_id,
                source_lang,
                target_lang,
                mode,
                provider,
                domain,
                formality,
                voice_gender,
                voice_age,
                voice_clone_id,
                device_id: _,
            } => {
                // Stop any existing session
                if let Some(session) = active_session.take() {
                    session.stop().await;
                }
                if let Some(handle) = relay_handle.take() {
                    handle.abort();
                }

                // Determine provider: explicit from client, or config default
                let provider_str = provider
                    .as_deref()
                    .or(voice_config.default_provider.as_deref())
                    .unwrap_or("gemini");

                // Resolve primary API key based on provider
                let api_key = match provider_str {
                    "deepgram" => voice_config
                        .deepgram_api_key
                        .clone()
                        .or_else(|| std::env::var("DEEPGRAM_API_KEY").ok())
                        .unwrap_or_default(),
                    "typecast_pipeline" => voice_config
                        .deepgram_api_key
                        .clone()
                        .or_else(|| std::env::var("DEEPGRAM_API_KEY").ok())
                        .unwrap_or_default(),
                    _ => voice_config.gemini_api_key.clone().unwrap_or_default(),
                };

                if api_key.is_empty() {
                    let key_name = match provider_str {
                        "deepgram" | "typecast_pipeline" => "Deepgram",
                        _ => "Gemini",
                    };
                    let err = ServerMessage::Error {
                        session_id: session_id.clone(),
                        code: "NO_API_KEY".into(),
                        message: format!(
                            "Voice interpretation requires your own {key_name} API key. \
                             Please enter it in Settings. \
                             (Operator keys are not available for voice features)"
                        ),
                    };
                    let _ = socket
                        .send(Message::Text(
                            serde_json::to_string(&err).unwrap_or_default().into(),
                        ))
                        .await;
                    continue;
                }

                // Parse language codes
                let src_lang = LanguageCode::from_str_code(&source_lang).unwrap_or_else(|| {
                    LanguageCode::from_str_code(&voice_config.default_source_language)
                        .unwrap_or(LanguageCode::Ko)
                });
                let tgt_lang = LanguageCode::from_str_code(&target_lang).unwrap_or_else(|| {
                    LanguageCode::from_str_code(&voice_config.default_target_language)
                        .unwrap_or(LanguageCode::En)
                });

                let segmentation = SegmentationConfig {
                    min_commit_chars: voice_config.min_commit_chars,
                    max_uncommitted_chars: voice_config.max_uncommitted_chars,
                    silence_commit_ms: voice_config.silence_commit_ms,
                    ..SegmentationConfig::default()
                };

                tracing::info!(
                    session_id = %session_id,
                    source = src_lang.as_str(),
                    target = tgt_lang.as_str(),
                    provider = provider_str,
                    mode = ?mode,
                    "Voice WebSocket: starting interpretation session"
                );

                // Parse voice profile (shared across providers)
                let gender_val = voice_gender
                    .as_deref()
                    .and_then(VoiceGender::from_str_opt)
                    .unwrap_or_default();
                let age_val = voice_age
                    .as_deref()
                    .and_then(VoiceAge::from_str_opt)
                    .unwrap_or_default();

                // Start the session with the selected provider
                let session_result: anyhow::Result<VoiceSessionHandle> = match provider_str {
                    "deepgram" => {
                        let dg_config = DeepgramSimulConfig {
                            session_id: session_id.clone(),
                            api_key,
                            source_lang: src_lang,
                            model: voice_config.deepgram_model.clone(),
                            segmentation,
                        };
                        DeepgramSimulSession::start(dg_config)
                            .await
                            .map(VoiceSessionHandle::Deepgram)
                    }

                    "gemma" => {
                        // PR #6: on-device STT via Gemma 4 E4B (replaces Deepgram).
                        // Reads Ollama base URL + model from env so config layout
                        // can stay frozen until the broader voice config refactor.
                        let base_url = std::env::var("OLLAMA_BASE_URL")
                            .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
                        let model = std::env::var("GEMMA_ASR_MODEL")
                            .unwrap_or_else(|_| "gemma4:e4b".to_string());
                        let cfg = GemmaSimulConfig {
                            session_id: session_id.clone(),
                            source_lang: src_lang,
                            base_url,
                            model,
                        };
                        // segmentation engine and Deepgram api_key are not needed
                        // here — Gemma utterance-segments internally via VAD.
                        let _ = (segmentation, api_key);
                        GemmaSimulSession::start(cfg)
                            .await
                            .map(VoiceSessionHandle::Gemma)
                    }

                    "typecast_pipeline" => {
                        // STT+LLM+TTS mode: Deepgram → LLM translation → Typecast TTS
                        let typecast_key = std::env::var("TYPECAST_API_KEY").unwrap_or_default();
                        let llm_key = std::env::var("GEMINI_API_KEY")
                            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
                            .unwrap_or_default();
                        let llm_model = std::env::var("TYPECAST_INTERP_LLM_MODEL")
                            .unwrap_or_else(|_| "gemini-3.1-flash-lite-preview".to_string());
                        let llm_base = std::env::var("TYPECAST_INTERP_LLM_BASE_URL")
                            .unwrap_or_else(|_| "https://generativelanguage.googleapis.com".to_string());

                        let tc_config = TypecastInterpConfig {
                            session_id: session_id.clone(),
                            deepgram_api_key: api_key,
                            deepgram_model: voice_config.deepgram_model.clone(),
                            source_lang: src_lang,
                            target_lang: tgt_lang,
                            typecast_api_key: typecast_key,
                            voice_clone_id: voice_clone_id.clone(),
                            voice_gender: gender_val,
                            voice_age: age_val,
                            fallback_voice_id: None, // TODO: resolve from cached voice list
                            llm_api_key: llm_key,
                            llm_model,
                            llm_base_url: llm_base,
                            segmentation,
                            bidirectional: mode == crate::voice::events::InterpretationMode::Bidirectional,
                        };
                        TypecastInterpSession::start(tc_config)
                            .await
                            .map(VoiceSessionHandle::TypecastPipeline)
                    }

                    _ => {
                        // Default: Gemini Live (full S2S interpretation)
                        let domain_val = domain
                            .as_deref()
                            .map(|d| match d {
                                "business" => Domain::Business,
                                "medical" => Domain::Medical,
                                "legal" => Domain::Legal,
                                "technical" => Domain::Technical,
                                _ => Domain::General,
                            })
                            .unwrap_or(Domain::General);

                        let formality_val = formality
                            .as_deref()
                            .map(|f| match f {
                                "formal" => Formality::Formal,
                                "casual" => Formality::Casual,
                                _ => Formality::Neutral,
                            })
                            .unwrap_or(Formality::Neutral);

                        let config = SimulSessionConfig {
                            session_id: session_id.clone(),
                            api_key,
                            source_lang: src_lang,
                            target_lang: tgt_lang,
                            mode,
                            domain: domain_val,
                            formality: formality_val,
                            segmentation,
                            voice_gender: gender_val,
                            voice_age: age_val,
                        };
                        SimulSession::start(config)
                            .await
                            .map(VoiceSessionHandle::Gemini)
                    }
                };

                match session_result {
                    Ok(session) => {
                        // Spawn event relay task: forward ServerMessages → WebSocket
                        let event_rx = session.event_rx();
                        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::channel::<String>(256);

                        let relay = tokio::spawn(async move {
                            let mut rx = event_rx.lock().await;
                            while let Some(event) = rx.recv().await {
                                if let Ok(json) = serde_json::to_string(&event) {
                                    if ws_tx.send(json).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        });
                        relay_handle = Some(relay);

                        active_session = Some(session);

                        // Drain any initial events (like session_ready) immediately
                        while let Ok(json) = ws_rx.try_recv() {
                            let _ = socket.send(Message::Text(json.into())).await;
                        }

                        // Break into the voice session event loop
                        voice_session_loop_unified(
                            &mut socket,
                            active_session.as_ref().unwrap(),
                            &mut ws_rx,
                        )
                        .await;

                        // Session ended — cleanup
                        if let Some(session) = active_session.take() {
                            session.stop().await;
                        }
                        if let Some(handle) = relay_handle.take() {
                            handle.abort();
                        }
                        return;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, provider = provider_str, "Failed to start voice session");
                        let err = ServerMessage::Error {
                            session_id,
                            code: "SESSION_START_FAILED".into(),
                            message: format!("Failed to start {provider_str} session: {e}"),
                        };
                        let _ = socket
                            .send(Message::Text(
                                serde_json::to_string(&err).unwrap_or_default().into(),
                            ))
                            .await;
                    }
                }
            }

            ClientMessage::AudioChunk { pcm16le, .. } => {
                if let Some(ref session) = active_session {
                    if let Ok(pcm_data) = base64::engine::general_purpose::STANDARD.decode(&pcm16le)
                    {
                        if let Err(e) = session.send_audio(pcm_data).await {
                            tracing::warn!(error = %e, "Failed to send audio to session");
                        }
                    }
                }
            }

            ClientMessage::SessionStop { session_id } => {
                tracing::info!(session_id = %session_id, "Voice WebSocket: session stop requested");
                if let Some(session) = active_session.take() {
                    session.stop().await;
                }
                if let Some(handle) = relay_handle.take() {
                    handle.abort();
                }
            }

            ClientMessage::ActivitySignal { .. } => {
                // Activity signals are informational; Gemini handles VAD internally.
                // Deepgram uses server-side endpointing — no client signals needed.
            }
        }
    }

    // Connection closed — cleanup
    if let Some(session) = active_session.take() {
        session.stop().await;
    }
    if let Some(handle) = relay_handle.take() {
        handle.abort();
    }
}

/// Inner loop for an active voice interpretation session (unified for all providers).
///
/// Simultaneously drains relay events (ServerMessage → WebSocket) and receives
/// new client messages (AudioChunk, SessionStop, etc.).
async fn voice_session_loop_unified(
    socket: &mut WebSocket,
    session: &VoiceSessionHandle,
    relay_rx: &mut tokio::sync::mpsc::Receiver<String>,
) {
    use crate::voice::events::{ClientMessage, ServerMessage};
    use base64::Engine;

    loop {
        tokio::select! {
            // Forward server events to client
            Some(json) = relay_rx.recv() => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }

            // Receive client messages
            msg = socket.recv() => {
                let Some(msg) = msg else { break };
                let text = match msg {
                    Ok(Message::Text(text)) => text,
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => continue,
                };

                let client_msg: ClientMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                match client_msg {
                    ClientMessage::AudioChunk { pcm16le, .. } => {
                        if let Ok(pcm_data) = base64::engine::general_purpose::STANDARD.decode(&pcm16le) {
                            if let Err(e) = session.send_audio(pcm_data).await {
                                tracing::warn!(error = %e, "Failed to send audio");
                                break;
                            }
                        }
                    }
                    ClientMessage::SessionStop { session_id } => {
                        tracing::info!(session_id = %session_id, "Voice session stop requested");
                        session.stop().await;

                        // Drain remaining relay events before exiting
                        while let Ok(json) = relay_rx.try_recv() {
                            let _ = socket.send(Message::Text(json.into())).await;
                        }
                        return;
                    }
                    ClientMessage::ActivitySignal { .. } => {
                        // Informational only — Gemini uses client VAD, Deepgram uses server-side
                    }
                    ClientMessage::SessionStart { session_id, .. } => {
                        // Cannot start a new session from within an active one
                        let err = ServerMessage::Error {
                            session_id,
                            code: "SESSION_ACTIVE".into(),
                            message: "A session is already active. Stop it first.".into(),
                        };
                        if let Ok(json) = serde_json::to_string(&err) {
                            let _ = socket.send(Message::Text(json.into())).await;
                        }
                    }
                }
            }
        }
    }
}

// ── Chat STT WebSocket (Deepgram voice input for chat) ──────────

/// GET /ws/stt — WebSocket upgrade for Deepgram speech-to-text in chat.
///
/// Lightweight endpoint: client sends raw PCM16 audio (Binary frames),
/// server streams back SttEvent JSON (partial/final transcripts).
/// No interpretation or segmentation — just pure STT for voice typing.
pub async fn handle_ws_stt(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let query_params = parse_ws_query_params(query.as_deref());

    // Auth
    if state.pairing.require_pairing() {
        let token =
            extract_ws_bearer_token(&headers, query_params.token.as_deref()).unwrap_or_default();
        if !state.pairing.is_authenticated(&token) {
            return (axum::http::StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    }

    ws.on_upgrade(move |socket| handle_stt_socket(socket, state))
        .into_response()
}

/// Chat STT WebSocket handler.
///
/// Protocol:
/// ```text
/// Client -> Server: Binary(PCM16LE 16kHz mono audio)
/// Client -> Server: {"type":"start","language":"ko"}     (optional, configure session)
/// Client -> Server: {"type":"finalize"}                   (flush buffered audio)
/// Client -> Server: {"type":"stop"}                       (end session)
/// Server -> Client: {"type":"stt_ready","request_id":"..."}
/// Server -> Client: {"type":"stt_partial","text":"...","confidence":0.8}
/// Server -> Client: {"type":"stt_final","text":"...","confidence":0.95,"speech_final":true}
/// Server -> Client: {"type":"stt_speech_started","timestamp":1.5}
/// Server -> Client: {"type":"stt_utterance_end","last_word_end":2.3}
/// Server -> Client: {"type":"stt_closed"}
/// ```
async fn handle_stt_socket(mut socket: WebSocket, state: AppState) {
    use crate::voice::deepgram_stt::{DeepgramConfig, DeepgramSttSession, SttEvent};

    // Read config
    let voice_config = {
        let config_guard = state.config.lock();
        config_guard.voice.clone()
    };

    // Resolve Deepgram API key
    let api_key = voice_config
        .deepgram_api_key
        .clone()
        .or_else(|| std::env::var("DEEPGRAM_API_KEY").ok())
        .unwrap_or_default();

    if api_key.is_empty() {
        let err_json = serde_json::json!({
            "type": "stt_error",
            "message": "Deepgram API key not configured. Set it in voice.deepgram_api_key or DEEPGRAM_API_KEY env var."
        });
        let _ = socket
            .send(Message::Text(err_json.to_string().into()))
            .await;
        return;
    }

    // Default config — can be overridden by a "start" message
    let mut language = "multi".to_string();
    let model = voice_config.deepgram_model.clone();

    // Wait for optional "start" message or first audio
    // Peek at first message to see if it's config or audio
    let first_msg = match socket.recv().await {
        Some(Ok(msg)) => msg,
        _ => return,
    };

    let mut first_audio: Option<Vec<u8>> = None;

    match &first_msg {
        Message::Text(text) => {
            // Parse start config
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
                if val.get("type").and_then(|t| t.as_str()) == Some("start") {
                    if let Some(lang) = val.get("language").and_then(|l| l.as_str()) {
                        language = lang.to_string();
                    }
                }
            }
        }
        Message::Binary(data) => {
            // First message is audio — use defaults
            first_audio = Some(data.to_vec());
        }
        Message::Close(_) => return,
        _ => {}
    }

    // Connect to Deepgram
    let session_id = uuid::Uuid::new_v4().to_string();
    let dg_config = DeepgramConfig {
        api_key,
        model,
        language,
        interim_results: true,
        smart_format: true,
        punctuate: true,
        endpointing_ms: Some(300),
        utterance_end_ms: Some(1000),
        vad_events: true,
        ..DeepgramConfig::default()
    };

    let dg_session = match DeepgramSttSession::connect(session_id.clone(), &dg_config).await {
        Ok(s) => s,
        Err(e) => {
            let err_json = serde_json::json!({
                "type": "stt_error",
                "message": format!("Failed to connect to Deepgram: {e}")
            });
            let _ = socket
                .send(Message::Text(err_json.to_string().into()))
                .await;
            return;
        }
    };

    let dg_session = std::sync::Arc::new(dg_session);

    // Send any buffered first audio
    if let Some(audio) = first_audio {
        let _ = dg_session.send_audio(audio).await;
    }

    // Spawn event relay: SttEvent → WebSocket JSON
    let dg_for_relay = std::sync::Arc::clone(&dg_session);
    let (relay_tx, mut relay_rx) = tokio::sync::mpsc::channel::<String>(256);
    tokio::spawn(async move {
        while let Some(event) = dg_for_relay.recv_event().await {
            if let Ok(json) = serde_json::to_string(&event) {
                if relay_tx.send(json).await.is_err() {
                    break;
                }
            }
            // Stop on session close
            if matches!(event, SttEvent::Closed) {
                break;
            }
        }
    });

    // Main loop: forward audio + relay events
    loop {
        tokio::select! {
            Some(json) = relay_rx.recv() => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }

            msg = socket.recv() => {
                let Some(msg) = msg else { break };
                match msg {
                    Ok(Message::Binary(data)) => {
                        if let Err(e) = dg_session.send_audio(data.to_vec()).await {
                            tracing::warn!(error = %e, "Failed to send audio to Deepgram");
                            break;
                        }
                    }
                    Ok(Message::Text(text)) => {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                            match val.get("type").and_then(|t| t.as_str()) {
                                Some("finalize") => {
                                    let _ = dg_session.finalize().await;
                                }
                                Some("stop") => {
                                    dg_session.close().await;
                                    // Drain remaining events
                                    while let Ok(json) = relay_rx.try_recv() {
                                        let _ = socket.send(Message::Text(json.into())).await;
                                    }
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
        }
    }

    dg_session.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolResult};
    use async_trait::async_trait;
    use axum::http::HeaderValue;

    #[test]
    fn extract_ws_bearer_token_prefers_authorization_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer from-auth-header"),
        );
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("zeroclaw.v1, bearer.from-protocol"),
        );

        assert_eq!(
            extract_ws_bearer_token(&headers, None).as_deref(),
            Some("from-auth-header")
        );
    }

    #[test]
    fn extract_ws_bearer_token_reads_websocket_protocol_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("zeroclaw.v1, bearer.protocol-token"),
        );

        assert_eq!(
            extract_ws_bearer_token(&headers, None).as_deref(),
            Some("protocol-token")
        );
    }

    #[test]
    fn extract_ws_bearer_token_rejects_empty_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer    "),
        );
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("zeroclaw.v1, bearer."),
        );

        assert!(extract_ws_bearer_token(&headers, None).is_none());
    }

    #[test]
    fn extract_ws_bearer_token_reads_query_token_fallback() {
        let headers = HeaderMap::new();
        assert_eq!(
            extract_ws_bearer_token(&headers, Some("query-token")).as_deref(),
            Some("query-token")
        );
    }

    #[test]
    fn extract_ws_bearer_token_prefers_protocol_over_query_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("zeroclaw.v1, bearer.protocol-token"),
        );

        assert_eq!(
            extract_ws_bearer_token(&headers, Some("query-token")).as_deref(),
            Some("protocol-token")
        );
    }

    #[test]
    fn extract_query_token_reads_token_param() {
        assert_eq!(
            extract_query_token(Some("foo=1&token=query-token&bar=2")).as_deref(),
            Some("query-token")
        );
        assert!(extract_query_token(Some("foo=1")).is_none());
    }

    #[test]
    fn parse_ws_query_params_reads_token_and_session_id() {
        let parsed = parse_ws_query_params(Some("foo=1&session_id=sess_123&token=query-token"));
        assert_eq!(parsed.token.as_deref(), Some("query-token"));
        assert_eq!(parsed.session_id.as_deref(), Some("sess_123"));
    }

    #[test]
    fn parse_ws_query_params_rejects_invalid_session_id() {
        let parsed = parse_ws_query_params(Some("session_id=../../etc/passwd"));
        assert!(parsed.session_id.is_none());
    }

    #[test]
    fn ws_history_turns_from_chat_skips_system_and_non_dialog_turns() {
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user(" hello "),
            ChatMessage {
                role: "tool".to_string(),
                content: "ignored".to_string(),
            },
            ChatMessage::assistant(" world "),
        ];

        let turns = ws_history_turns_from_chat(&history);
        assert_eq!(
            turns,
            vec![
                WsHistoryTurn {
                    role: "user".to_string(),
                    content: "hello".to_string()
                },
                WsHistoryTurn {
                    role: "assistant".to_string(),
                    content: "world".to_string()
                }
            ]
        );
    }

    #[test]
    fn restore_chat_history_applies_system_prompt_once() {
        let turns = vec![
            WsHistoryTurn {
                role: "user".to_string(),
                content: "u1".to_string(),
            },
            WsHistoryTurn {
                role: "assistant".to_string(),
                content: "a1".to_string(),
            },
        ];

        let restored = restore_chat_history("sys", &turns);
        assert_eq!(restored.len(), 3);
        assert_eq!(restored[0].role, "system");
        assert_eq!(restored[0].content, "sys");
        assert_eq!(restored[1].role, "user");
        assert_eq!(restored[1].content, "u1");
        assert_eq!(restored[2].role, "assistant");
        assert_eq!(restored[2].content, "a1");
    }

    struct MockScheduleTool;

    #[async_trait]
    impl Tool for MockScheduleTool {
        fn name(&self) -> &str {
            "schedule"
        }

        fn description(&self) -> &str {
            "Mock schedule tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" }
                }
            })
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "ok".to_string(),
                error: None,
            })
        }
    }

    #[test]
    fn sanitize_ws_response_removes_tool_call_tags() {
        let input = r#"Before
<tool_call>
{"name":"schedule","arguments":{"action":"create"}}
</tool_call>
After"#;

        let leak_guard = crate::config::OutboundLeakGuardConfig::default();
        let result = sanitize_ws_response(input, &[], &leak_guard);
        let normalized = result
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(normalized, "Before\nAfter");
        assert!(!result.contains("<tool_call>"));
        assert!(!result.contains("\"name\":\"schedule\""));
    }

    #[test]
    fn sanitize_ws_response_removes_isolated_tool_json_artifacts() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];
        let input = r#"{"name":"schedule","parameters":{"action":"create"}}
{"result":{"status":"scheduled"}}
Reminder set successfully."#;

        let leak_guard = crate::config::OutboundLeakGuardConfig::default();
        let result = sanitize_ws_response(input, &tools, &leak_guard);
        assert_eq!(result, "Reminder set successfully.");
        assert!(!result.contains("\"name\":\"schedule\""));
        assert!(!result.contains("\"result\""));
    }

    #[test]
    fn sanitize_ws_response_blocks_detected_credentials_when_configured() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let leak_guard = crate::config::OutboundLeakGuardConfig {
            enabled: true,
            action: crate::config::OutboundLeakGuardAction::Block,
            sensitivity: 0.7,
        };

        let result =
            sanitize_ws_response("Temporary key: AKIAABCDEFGHIJKLMNOP", &tools, &leak_guard);
        assert!(result.contains("blocked a draft response"));
    }

    #[test]
    fn build_ws_system_prompt_includes_tool_protocol_for_prompt_mode() {
        let config = crate::config::Config::default();
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];

        let prompt = build_ws_system_prompt(&config, "test-model", &tools, false);

        assert!(prompt.contains("## Tool Use Protocol"));
        assert!(prompt.contains("**schedule**"));
        assert!(prompt.contains("## Shell Policy"));
    }

    #[test]
    fn build_ws_system_prompt_omits_xml_protocol_for_native_mode() {
        let config = crate::config::Config::default();
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];

        let prompt = build_ws_system_prompt(&config, "test-model", &tools, true);

        assert!(!prompt.contains("## Tool Use Protocol"));
        assert!(prompt.contains("**schedule**"));
        assert!(prompt.contains("## Shell Policy"));
    }

    #[test]
    fn finalize_ws_response_uses_prompt_mode_tool_output_when_final_text_empty() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"schedule\">\nDisk usage: 72%\n</tool_result>",
            ),
        ];

        let leak_guard = crate::config::OutboundLeakGuardConfig::default();
        let result = finalize_ws_response("", &history, &tools, &leak_guard);
        assert!(result.contains("Latest tool output:"));
        assert!(result.contains("Disk usage: 72%"));
        assert!(!result.contains("<tool_result"));
    }

    #[test]
    fn finalize_ws_response_uses_native_tool_message_output_when_final_text_empty() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];
        let history = vec![ChatMessage {
            role: "tool".to_string(),
            content: r#"{"tool_call_id":"call_1","content":"Filesystem /dev/disk3s1: 210G free"}"#
                .to_string(),
        }];

        let leak_guard = crate::config::OutboundLeakGuardConfig::default();
        let result = finalize_ws_response("", &history, &tools, &leak_guard);
        assert!(result.contains("Latest tool output:"));
        assert!(result.contains("/dev/disk3s1"));
    }

    #[test]
    fn finalize_ws_response_uses_static_fallback_when_nothing_available() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];
        let history = vec![ChatMessage::system("sys")];

        let leak_guard = crate::config::OutboundLeakGuardConfig::default();
        let result = finalize_ws_response("", &history, &tools, &leak_guard);
        assert_eq!(result, EMPTY_WS_RESPONSE_FALLBACK);
    }
}
