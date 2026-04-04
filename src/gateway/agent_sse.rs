use super::AppState;
use crate::agent::loop_::{DraftEvent, run_tool_call_loop, scrub_credentials, trim_history};
use crate::approval::ApprovalManager;
use crate::providers::ChatMessage;
use crate::security::SecurityPolicy;
use crate::tools::{self, Tool, ToolResult};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, sse::Event, sse::KeepAlive, sse::Sse},
};
use serde::Deserialize;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, OnceLock};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct AgentSseRequest {
    pub message: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentClearRequest {
    pub session_id: String,
}

#[derive(Debug, Clone)]
struct AgentEvent {
    event: &'static str,
    data: serde_json::Value,
}

impl AgentEvent {
    fn tool_call(name: String, args: serde_json::Value) -> Self {
        Self {
            event: "tool_call",
            data: serde_json::json!({ "name": name, "args": args }),
        }
    }

    fn tool_result(name: String, result: ToolResult) -> Self {
        Self {
            event: "tool_result",
            data: serde_json::json!({ "name": name, "result": result }),
        }
    }

    fn chunk(content: String) -> Self {
        Self {
            event: "chunk",
            data: serde_json::json!({ "content": content }),
        }
    }

    fn done(session_id: String, response: String, error: Option<String>) -> Self {
        Self {
            event: "done",
            data: serde_json::json!({
                "session_id": session_id,
                "full_response": response,
                "error": error,
            }),
        }
    }
}

struct StreamingTool {
    name: String,
    description: String,
    parameters: serde_json::Value,
    registry: Arc<Vec<Box<dyn Tool>>>,
    event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
}

impl StreamingTool {
    fn new(
        tool: &dyn Tool,
        registry: Arc<Vec<Box<dyn Tool>>>,
        event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> Self {
        Self {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            parameters: tool.parameters_schema(),
            registry,
            event_tx,
        }
    }

    fn find_inner(&self) -> Option<&dyn Tool> {
        self.registry
            .iter()
            .find(|tool| tool.name() == self.name)
            .map(|tool| tool.as_ref())
    }
}

#[async_trait::async_trait]
impl Tool for StreamingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let _ = self
            .event_tx
            .send(AgentEvent::tool_call(self.name.clone(), args.clone()))
            .await;

        let result = if let Some(tool) = self.find_inner() {
            tool.execute(args).await
        } else {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown tool: {}", self.name)),
            })
        };

        let event_result = match &result {
            Ok(tool_result) => ToolResult {
                success: tool_result.success,
                output: scrub_credentials(&tool_result.output),
                error: tool_result.error.clone().map(|err| scrub_credentials(&err)),
            },
            Err(err) => ToolResult {
                success: false,
                output: String::new(),
                error: Some(scrub_credentials(&err.to_string())),
            },
        };

        let _ = self
            .event_tx
            .send(AgentEvent::tool_result(self.name.clone(), event_result))
            .await;

        result
    }
}

static AGENT_SESSIONS: OnceLock<Mutex<HashMap<String, Vec<ChatMessage>>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, Vec<ChatMessage>>> {
    AGENT_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn handle_agent_clear(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<AgentClearRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
                })),
            )
                .into_response();
        }
    }

    let body = match body {
        Ok(Json(body)) => body,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Invalid JSON body: {e}") })),
            )
                .into_response();
        }
    };

    let session_id = body.session_id.trim().to_string();
    if session_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "session_id must not be empty" })),
        )
            .into_response();
    }

    {
        let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&session_id);
    }

    let history_key = format!("session:{session_id}:history");
    let _ = state.mem.forget(&history_key).await;

    (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
}

pub async fn handle_agent_sse(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<AgentSseRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
                })),
            )
                .into_response();
        }
    }

    let body = match body {
        Ok(Json(body)) => body,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Invalid JSON body: {e}") })),
            )
                .into_response();
        }
    };

    let message = body.message.trim().to_string();
    if message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "message must not be empty" })),
        )
            .into_response();
    }

    let session_id = body
        .session_id
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let mut history = {
        let guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&session_id).cloned().unwrap_or_default()
    };

    let had_prior_history = history.len() > 1;
    if history.is_empty() {
        let config_guard = state.config.lock();
        let prompt = crate::channels::build_system_prompt(
            &config_guard.workspace_dir,
            &state.model,
            &[],
            &[],
            Some(&config_guard.identity),
            None,
        );
        history.push(ChatMessage::system(&prompt));
    }

    let user_message = if had_prior_history {
        message
    } else {
        let (min_relevance_score, session_id_for_memory) = {
            let config_guard = state.config.lock();
            (config_guard.memory.min_relevance_score, session_id.clone())
        };
        let memory_context = crate::channels::build_memory_context(
            state.mem.as_ref(),
            &message,
            min_relevance_score,
            Some(&session_id_for_memory),
        )
        .await;
        if memory_context.is_empty() {
            message
        } else {
            format!("{memory_context}{message}")
        }
    };
    history.push(ChatMessage::user(&user_message));

    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(128);
    let stream = ReceiverStream::new(event_rx).map(|event| {
        Ok::<_, Infallible>(
            Event::default()
                .event(event.event)
                .data(event.data.to_string()),
        )
    });

    let state_for_task = state.clone();
    let session_id_for_task = session_id.clone();
    tokio::spawn(async move {
        let config = state_for_task.config.lock().clone();
        let approval_manager = ApprovalManager::from_config(&config.autonomy);
        let provider_label = config
            .default_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let max_history = config.agent.max_history_messages;

        let runtime: Arc<dyn crate::runtime::RuntimeAdapter> =
            match crate::runtime::create_runtime(&config.runtime) {
                Ok(runtime) => Arc::from(runtime),
                Err(e) => {
                    let _ = event_tx
                        .send(AgentEvent::done(
                            session_id_for_task,
                            String::new(),
                            Some(format!("Runtime init failed: {e}")),
                        ))
                        .await;
                    return;
                }
            };

        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let (composio_key, composio_entity_id) = if config.composio.enabled {
            (
                config.composio.api_key.as_deref(),
                Some(config.composio.entity_id.as_str()),
            )
        } else {
            (None, None)
        };

        let (
            built_tools,
            _delegate_handle,
            _reaction_handle,
            _channel_map_handle,
            _ask_user_handle,
            _escalate_handle,
        ) = tools::all_tools_with_runtime(
            Arc::new(config.clone()),
            &security,
            runtime,
            Arc::clone(&state_for_task.mem),
            composio_key,
            composio_entity_id,
            &config.browser,
            &config.http_request,
            &config.web_fetch,
            &config.workspace_dir,
            &config.agents,
            config.api_key.as_deref(),
            &config,
            Some(state_for_task.canvas_store.clone()),
        );

        let tools_registry = Arc::new(built_tools);
        let streaming_tools: Vec<Box<dyn Tool>> = tools_registry
            .iter()
            .map(|tool| {
                Box::new(StreamingTool::new(
                    tool.as_ref(),
                    Arc::clone(&tools_registry),
                    event_tx.clone(),
                )) as Box<dyn Tool>
            })
            .collect();

        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);
        let event_tx_delta = event_tx.clone();
        tokio::spawn(async move {
            while let Some(delta) = delta_rx.recv().await {
                match delta {
                    DraftEvent::Clear | DraftEvent::Progress(_) => {}
                    DraftEvent::Content(text) => {
                        let _ = event_tx_delta.send(AgentEvent::chunk(text)).await;
                    }
                }
            }
        });

        if !state_for_task.provider.supports_native_tools() {
            if let Some(system_message) = history.first_mut() {
                system_message
                    .content
                    .push_str(&crate::agent::loop_::build_tool_instructions(
                        streaming_tools.as_ref(),
                        None,
                    ));
            }
        }

        let result = run_tool_call_loop(
            state_for_task.provider.as_ref(),
            &mut history,
            &streaming_tools,
            state_for_task.observer.as_ref(),
            &provider_label,
            &state_for_task.model,
            state_for_task.temperature,
            true,
            Some(&approval_manager),
            "agent_sse",
            Some(session_id_for_task.as_str()),
            &config.multimodal,
            config.agent.max_tool_iterations,
            None,
            Some(delta_tx),
            None,
            &[],
            config.agent.tool_call_dedup_exempt.as_slice(),
            None,
            None,
            &config.pacing,
            0,
            0,
            None,
        )
        .await;

        let (response, error) = match result {
            Ok(text) => {
                let safe =
                    crate::channels::sanitize_channel_response(&text, streaming_tools.as_ref());
                (safe, None)
            }
            Err(e) => {
                let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                (String::new(), Some(sanitized))
            }
        };

        trim_history(&mut history, max_history);

        let history_key = format!("session:{}:history", session_id_for_task);
        let to_store: Vec<&ChatMessage> = history.iter().filter(|m| m.role != "system").collect();
        if let Ok(serialized) = serde_json::to_string(&to_store) {
            let _ = state_for_task
                .mem
                .store(
                    &history_key,
                    &serialized,
                    crate::memory::MemoryCategory::Conversation,
                    None,
                )
                .await;
        }

        {
            let mut guard = sessions().lock().unwrap_or_else(|e| e.into_inner());
            guard.insert(session_id_for_task.clone(), history);
        }

        let _ = event_tx
            .send(AgentEvent::done(session_id_for_task, response, error))
            .await;
    });

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
