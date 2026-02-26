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
use crate::agent::loop_::{
    build_shell_policy_instructions, build_tool_instructions_from_specs, run_tool_call_loop,
};
use crate::approval::ApprovalManager;
use crate::providers::ChatMessage;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{header, HeaderMap},
    response::IntoResponse,
};

const EMPTY_WS_RESPONSE_FALLBACK: &str =
    "Tool execution completed, but the model returned no final text response. Please ask me to summarize the result.";

fn sanitize_ws_response(response: &str, tools: &[Box<dyn crate::tools::Tool>]) -> String {
    let sanitized = crate::channels::sanitize_channel_response(response, tools);
    if sanitized.is_empty() && !response.trim().is_empty() {
        "I encountered malformed tool-call output and could not produce a safe reply. Please try again."
            .to_string()
    } else {
        sanitized
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
) -> String {
    let sanitized = sanitize_ws_response(response, tools);
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
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Auth via Authorization header or websocket protocol token.
    if state.pairing.require_pairing() {
        let token = extract_ws_bearer_token(&headers).unwrap_or_default();
        if !state.pairing.is_authenticated(&token) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization: Bearer <token> or Sec-WebSocket-Protocol: bearer.<token>",
            )
                .into_response();
        }
    }

    ws.on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    // Maintain conversation history for this WebSocket session
    let mut history: Vec<ChatMessage> = Vec::new();

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

    // Add system message to history
    history.push(ChatMessage::system(&system_prompt));

    let approval_manager = {
        let config_guard = state.config.lock();
        ApprovalManager::from_config(&config_guard.autonomy)
    };

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

        // Full agentic loop with tools (includes WASM skills, shell, memory, etc.)
        match super::run_gateway_chat_with_tools(&state, &content).await {
            Ok(response) => {
                let safe_response =
                    finalize_ws_response(&response, &history, state.tools_registry_exec.as_ref());
                // Add assistant response to history
                history.push(ChatMessage::assistant(&safe_response));

                // Send the full response as a done message
                let done = serde_json::json!({
                    "type": "done",
                    "full_response": safe_response,
                });
                let _ = socket.send(Message::Text(done.to_string().into())).await;

                // Broadcast agent_end event
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "agent_end",
                    "provider": provider_label,
                    "model": state.model,
                }));
            }
            Err(e) => {
                let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                let err = serde_json::json!({
                    "type": "error",
                    "message": sanitized,
                });
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

fn extract_ws_bearer_token(headers: &HeaderMap) -> Option<String> {
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

    let offered = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())?;

    for protocol in offered.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(token) = protocol.strip_prefix("bearer.") {
            if !token.trim().is_empty() {
                return Some(token.trim().to_string());
            }
        }
    }

    None
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
            extract_ws_bearer_token(&headers).as_deref(),
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
            extract_ws_bearer_token(&headers).as_deref(),
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

        assert!(extract_ws_bearer_token(&headers).is_none());
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

        let result = sanitize_ws_response(input, &[]);
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

        let result = sanitize_ws_response(input, &tools);
        assert_eq!(result, "Reminder set successfully.");
        assert!(!result.contains("\"name\":\"schedule\""));
        assert!(!result.contains("\"result\""));
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

        let result = finalize_ws_response("", &history, &tools);
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

        let result = finalize_ws_response("", &history, &tools);
        assert!(result.contains("Latest tool output:"));
        assert!(result.contains("/dev/disk3s1"));
    }

    #[test]
    fn finalize_ws_response_uses_static_fallback_when_nothing_available() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];
        let history = vec![ChatMessage::system("sys")];

        let result = finalize_ws_response("", &history, &tools);
        assert_eq!(result, EMPTY_WS_RESPONSE_FALLBACK);
    }
}
