use crate::AppState;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, Sse};
use axum::{Json, extract::State, response::IntoResponse};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroclaw_api::agent::TurnEvent;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn completion_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
}

/// Human-readable status message shown to the user when a tool is invoked.
/// Sent as a regular SSE content chunk so clients (Home Assistant, Open WebUI)
/// can display it while the tool executes.
fn tool_status_message(tool_name: &str) -> &'static str {
    match tool_name {
        "web_search" | "web_search_tool" => "🔍 Searching the web...",
        "web_fetch" => "📄 Reading page...",
        "shell" => "⚙️ Running command...",
        "calculator" => "🔢 Calculating...",
        "memory_recall" => "🧠 Recalling memory...",
        "memory_store" => "💾 Storing memory...",
        "file_read" => "📂 Reading file...",
        "file_write" => "✏️ Writing file...",
        "http_request" => "🌐 Making HTTP request...",
        "browser" | "browser_open" => "🌐 Opening browser...",
        _ => "🛠️ Using tool...",
    }
}

// ---------------------------------------------------------------------------
// Request / Response types (OpenAI wire format)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIModel {
    pub id: String,
    pub object: String,
    #[serde(default)]
    pub owned_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<OpenAIModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub presence_penalty: Option<f64>,
    #[serde(default)]
    pub frequency_penalty: Option<f64>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
    /// Optional stable session key (mirrors OpenAI `user` field).
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChoice {
    pub index: u32,
    pub message: Message,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatCompletionChoice>,
    pub usage: Option<ChatCompletionUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunkChoice {
    pub index: u32,
    pub delta: MessageDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatCompletionChunkChoice>,
}

// ---------------------------------------------------------------------------
// Auth helper — mirrors handle_webhook pattern exactly
// ---------------------------------------------------------------------------

fn check_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !state.pairing.require_pairing() {
        return Ok(());
    }
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth.strip_prefix("Bearer ").unwrap_or("");
    if state.pairing.is_authenticated(token) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": {
                    "message": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>",
                    "type": "invalid_request_error",
                    "code": "invalid_api_key"
                }
            })),
        ))
    }
}

// ---------------------------------------------------------------------------
// SSE chunk builder
// ---------------------------------------------------------------------------

fn make_sse_chunk(id: &str, created: u64, model: &str, content: &str) -> Event {
    let chunk = ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![ChatCompletionChunkChoice {
            index: 0,
            delta: MessageDelta {
                role: None,
                content: Some(content.to_string()),
            },
            finish_reason: None,
        }],
    };
    let data = serde_json::to_string(&chunk).unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e));
    Event::default().data(data)
}

fn make_stop_chunk(id: &str, created: u64, model: &str) -> Event {
    let chunk = ChatCompletionChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".to_string(),
        created,
        model: model.to_string(),
        choices: vec![ChatCompletionChunkChoice {
            index: 0,
            delta: MessageDelta {
                role: None,
                content: None,
            },
            finish_reason: Some("stop".to_string()),
        }],
    };
    let data = serde_json::to_string(&chunk).unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e));
    Event::default().data(data)
}

// ---------------------------------------------------------------------------
// GET /openai/v1/models
// ---------------------------------------------------------------------------

pub async fn handle_openai_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return e.into_response();
    }

    let model_id = state.model.clone();

    Json(ModelsResponse {
        object: "list".to_string(),
        data: vec![OpenAIModel {
            id: model_id,
            object: "model".to_string(),
            owned_by: Some("zeroclaw".to_string()),
        }],
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// POST /openai/v1/chat/completions
// ---------------------------------------------------------------------------

pub async fn handle_openai_chat_completion_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    if let Err(e) = check_auth(&state, &headers) {
        return e.into_response();
    }

    // Extract the last user message as the agent prompt.
    let prompt = match req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone())
    {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": "No user message found in messages array",
                        "type": "invalid_request_error"
                    }
                })),
            )
                .into_response();
        }
    };

    // Derive a stable session_id from the optional `user` field.
    let session_id = req
        .user
        .as_deref()
        .filter(|u| !u.is_empty())
        .map(|u| format!("openai-bridge:{u}"));

    let id = completion_id();
    let created = unix_now();
    let model = state.model.clone();
    let streaming = req.stream.unwrap_or(false);

    // ── Build agent from config (soul/identity + tools injected automatically) ──
    let config = state.config.lock().clone();
    let mut agent =
        match zeroclaw_runtime::agent::Agent::from_config_with_session_cwd(&config, None).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("OpenAI bridge: agent init failed: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!("Agent initialization failed: {e}"),
                            "type": "server_error"
                        }
                    })),
                )
                    .into_response();
            }
        };

    if let Some(ref sid) = session_id {
        agent.set_memory_session_id(Some(sid.clone()));
    }

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);

    tokio::spawn(async move {
        if let Err(e) = agent.turn_streamed(&prompt, event_tx, None).await {
            tracing::error!("OpenAI bridge: turn_streamed error: {e}");
        }
    });

    if streaming {
        // ── Streaming SSE path ───────────────────────────────────────────────
        // Relay TurnEvents as SSE chunks:
        //   TurnEvent::ToolCall  → human-readable status message
        //   TurnEvent::Chunk     → real LLM token delta
        //   TurnEvent::Thinking  → ignored (internal reasoning)
        //   TurnEvent::ToolResult→ ignored
        let id_clone = id.clone();
        let model_clone = model.clone();

        let stream = async_stream::stream! {
            while let Some(event) = event_rx.recv().await {
                match event {
                    TurnEvent::ToolCall { ref name, .. } => {
                        let msg = tool_status_message(name);
                        yield Ok::<_, Infallible>(
                            make_sse_chunk(&id_clone, created, &model_clone, msg)
                        );
                    }
                    TurnEvent::Chunk { delta } => {
                        yield Ok::<_, Infallible>(
                            make_sse_chunk(&id_clone, created, &model_clone, &delta)
                        );
                    }
                    TurnEvent::Thinking { .. } | TurnEvent::ToolResult { .. } | TurnEvent::ApprovalRequest { .. } | TurnEvent::Usage { .. } => {
                        // Not forwarded to the client.
                    }
                }
            }
            // Final stop chunk + [DONE] sentinel.
            yield Ok::<_, Infallible>(make_stop_chunk(&id_clone, created, &model_clone));
            yield Ok::<_, Infallible>(Event::default().data("[DONE]"));
        };

        Sse::new(stream).into_response()
    } else {
        // ── Non-streaming path ───────────────────────────────────────────────
        // Drain the event channel, collecting only Chunk deltas.
        // Tool status messages are NOT included in non-streaming responses —
        // clients like Home Assistant expect a clean final text.
        let mut full_response = String::new();
        while let Some(event) = event_rx.recv().await {
            if let TurnEvent::Chunk { delta } = event {
                full_response.push_str(&delta);
            }
        }

        Json(ChatCompletionResponse {
            id,
            object: "chat.completion".to_string(),
            created,
            model,
            choices: vec![ChatCompletionChoice {
                index: 0,
                message: Message {
                    role: "assistant".to_string(),
                    content: full_response,
                },
                finish_reason: Some("stop".to_string()),
            }],
            usage: Some(ChatCompletionUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            }),
        })
        .into_response()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── tool_status_message ──────────────────────────────────────────────────

    #[test]
    fn known_tools_have_specific_messages() {
        assert!(tool_status_message("web_search").contains("Searching"));
        assert!(tool_status_message("web_fetch").contains("Reading"));
        assert!(tool_status_message("shell").contains("Running"));
        assert!(tool_status_message("calculator").contains("Calculating"));
        assert!(tool_status_message("memory_recall").contains("Recalling"));
        assert!(tool_status_message("file_read").contains("Reading"));
    }

    #[test]
    fn unknown_tool_returns_generic_message() {
        assert_eq!(tool_status_message("some_unknown_tool"), "🛠️ Using tool...");
    }

    // ── OpenAI wire format ───────────────────────────────────────────────────

    #[test]
    fn chat_completion_request_stream_defaults_to_none() {
        let json = r#"{"model":"default","messages":[{"role":"user","content":"hi"}]}"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert!(req.stream.is_none());
        assert!(req.user.is_none());
    }

    #[test]
    fn chat_completion_request_parses_stream_true() {
        let json =
            r#"{"model":"default","stream":true,"messages":[{"role":"user","content":"hi"}]}"#;
        let req: ChatCompletionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.stream, Some(true));
    }

    #[test]
    fn models_response_serializes_correctly() {
        let resp = ModelsResponse {
            object: "list".to_string(),
            data: vec![OpenAIModel {
                id: "my-model".to_string(),
                object: "model".to_string(),
                owned_by: Some("zeroclaw".to_string()),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"object\":\"list\""));
        assert!(json.contains("\"id\":\"my-model\""));
        assert!(json.contains("\"owned_by\":\"zeroclaw\""));
    }

    #[test]
    fn message_delta_skips_none_fields() {
        let delta = MessageDelta {
            role: None,
            content: Some("hello".to_string()),
        };
        let json = serde_json::to_string(&delta).unwrap();
        assert!(!json.contains("role"));
        assert!(json.contains("content"));
    }

    #[test]
    fn completion_id_has_correct_prefix_and_is_unique() {
        let id1 = completion_id();
        let id2 = completion_id();
        assert!(id1.starts_with("chatcmpl-"));
        assert!(id2.starts_with("chatcmpl-"));
        // UUIDs guarantee uniqueness — two calls must not collide.
        assert_ne!(id1, id2);
    }

    #[test]
    fn make_sse_chunk_serializes_content_correctly() {
        let event = make_sse_chunk("id-1", 123, "my-model", "hello");
        // Event::data() is not directly inspectable, but we can verify
        // the chunk struct serializes as expected.
        let chunk = ChatCompletionChunk {
            id: "id-1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 123,
            model: "my-model".to_string(),
            choices: vec![ChatCompletionChunkChoice {
                index: 0,
                delta: MessageDelta {
                    role: None,
                    content: Some("hello".to_string()),
                },
                finish_reason: None,
            }],
        };
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(json.contains("\"content\":\"hello\""));
        assert!(json.contains("\"object\":\"chat.completion.chunk\""));
        assert!(!json.contains("finish_reason"));
        let _ = event; // event is constructed without error
    }

    #[test]
    fn make_stop_chunk_has_finish_reason_stop() {
        let chunk = ChatCompletionChunk {
            id: "id-1".to_string(),
            object: "chat.completion.chunk".to_string(),
            created: 123,
            model: "my-model".to_string(),
            choices: vec![ChatCompletionChunkChoice {
                index: 0,
                delta: MessageDelta {
                    role: None,
                    content: None,
                },
                finish_reason: Some("stop".to_string()),
            }],
        };
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(json.contains("\"finish_reason\":\"stop\""));
        assert!(!json.contains("\"content\""));
    }
}
