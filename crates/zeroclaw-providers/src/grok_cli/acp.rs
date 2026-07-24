//! Internal bounded one-shot ACP client for `grok agent stdio`.
//!
//! The wire sequence follows Grok Build's documented example:
//! initialize → authenticate → session/new → session/prompt. Assistant text
//! arrives in `session/update` notifications. Every input frame and aggregate
//! byte count is bounded before allocation grows. Server permission requests
//! select reject-once when offered so the tool fails closed without cancelling
//! the complete agent turn.

use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use zeroclaw_api::jsonrpc::{
    ACP_PROTOCOL_VERSION, JSONRPC_VERSION, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    error_codes::METHOD_NOT_FOUND, field,
};

/// Maximum size of one newline-delimited JSON-RPC frame.
const MAX_ACP_FRAME_BYTES: usize = 1_048_576;

/// Maximum aggregate stdout consumed during one ACP request.
const MAX_ACP_STDOUT_BYTES: usize = 4_194_304;

/// Maximum assistant text returned to the channel/runtime.
const MAX_ACP_ASSISTANT_BYTES: usize = 1_048_576;

/// Grok's published example waits for two stable 150 ms intervals after the
/// prompt response so trailing `session/update` chunks are not lost.
const OUTPUT_SETTLE_INTERVAL: Duration = Duration::from_millis(150);
const OUTPUT_SETTLE_INTERVALS: usize = 2;

#[derive(Debug, Error)]
pub(super) enum AcpError {
    #[error("Grok ACP transport failed while writing {phase}")]
    Write { phase: &'static str },
    #[error("Grok ACP transport failed while reading {phase}")]
    Read { phase: &'static str },
    #[error("Grok ACP process closed before {phase} completed")]
    Closed { phase: &'static str },
    #[error("Grok ACP stdout frame exceeded {limit} bytes")]
    FrameLimit { limit: usize },
    #[error("Grok ACP stdout exceeded {limit} bytes")]
    StdoutLimit { limit: usize },
    #[error("Grok ACP assistant output exceeded {limit} bytes")]
    AssistantLimit { limit: usize },
    #[error("Grok ACP returned invalid JSON during {phase}")]
    InvalidJson { phase: &'static str },
    #[error("Grok ACP returned an error during {phase}")]
    Remote { phase: &'static str },
    #[error("Grok ACP {phase} response was incomplete")]
    Incomplete { phase: &'static str },
    #[error("Grok ACP initialize returned no usable authentication method")]
    NoAuthenticationMethod,
    #[error("Grok ACP session/prompt completed without agent message text")]
    EmptyOutput,
    #[error("Grok ACP could not encode an internal request")]
    Encode,
}

impl AcpError {
    pub(super) fn error_key(&self) -> &'static str {
        match self {
            Self::Write { .. } => "grok_cli_acp_write_failed",
            Self::Read { .. } => "grok_cli_acp_read_failed",
            Self::Closed { .. } => "grok_cli_acp_closed",
            Self::FrameLimit { .. } => "grok_cli_acp_frame_limit",
            Self::StdoutLimit { .. } => "grok_cli_acp_stdout_limit",
            Self::AssistantLimit { .. } => "grok_cli_acp_assistant_limit",
            Self::InvalidJson { .. } => "grok_cli_acp_invalid_json",
            Self::Remote { .. } => "grok_cli_acp_remote_error",
            Self::Incomplete { .. } => "grok_cli_acp_incomplete_response",
            Self::NoAuthenticationMethod => "grok_cli_acp_auth_unavailable",
            Self::EmptyOutput => "grok_cli_acp_empty_output",
            Self::Encode => "grok_cli_acp_encode_failed",
        }
    }
}

/// Run one prompt against an already-spawned `grok agent stdio` child.
pub(super) async fn run_oneshot_prompt<W, R>(
    stdin: &mut W,
    stdout: R,
    prompt: &str,
    cwd: &Path,
    xai_api_key_available: bool,
) -> Result<String, AcpError>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let mut reader = AcpReader::new(stdout);
    let mut next_id = 1_u64;
    let mut assistant = String::new();

    let initialize = rpc_request(
        stdin,
        &mut reader,
        &mut next_id,
        "initialize",
        json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false
            }
        }),
        &mut assistant,
    )
    .await?;

    let method_id = select_auth_method_id(&initialize, xai_api_key_available)?;
    rpc_request(
        stdin,
        &mut reader,
        &mut next_id,
        "authenticate",
        json!({
            "methodId": method_id,
            "_meta": { "headless": true }
        }),
        &mut assistant,
    )
    .await?;

    let new_session = rpc_request(
        stdin,
        &mut reader,
        &mut next_id,
        "session/new",
        json!({
            "cwd": cwd,
            "mcpServers": []
        }),
        &mut assistant,
    )
    .await?;
    let session_id = new_session
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or(AcpError::Incomplete {
            phase: "session/new",
        })?
        .to_string();

    // Authentication/session notifications are not part of the answer.
    assistant.clear();
    rpc_request(
        stdin,
        &mut reader,
        &mut next_id,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": prompt }]
        }),
        &mut assistant,
    )
    .await?;

    settle_trailing_output(stdin, &mut reader, &mut assistant).await?;
    let trimmed = assistant.trim();
    if trimmed.is_empty() {
        return Err(AcpError::EmptyOutput);
    }
    Ok(trimmed.to_string())
}

struct AcpReader<R> {
    inner: BufReader<R>,
    bytes_read: usize,
}

impl<R> AcpReader<R>
where
    R: AsyncRead + Unpin,
{
    fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
            bytes_read: 0,
        }
    }

    async fn next_message(&mut self, phase: &'static str) -> Result<Option<Value>, AcpError> {
        loop {
            let Some(frame) = self.read_frame(phase).await? else {
                return Ok(None);
            };
            let trimmed = trim_ascii_whitespace(&frame);
            if trimmed.is_empty() {
                continue;
            }
            let message =
                serde_json::from_slice(trimmed).map_err(|_| AcpError::InvalidJson { phase })?;
            return Ok(Some(message));
        }
    }

    async fn read_frame(&mut self, phase: &'static str) -> Result<Option<Vec<u8>>, AcpError> {
        let mut frame = Vec::new();
        loop {
            let available = self
                .inner
                .fill_buf()
                .await
                .map_err(|_| AcpError::Read { phase })?;
            if available.is_empty() {
                return if frame.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(frame))
                };
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |position| position + 1);
            let next_total = self
                .bytes_read
                .checked_add(take)
                .ok_or(AcpError::StdoutLimit {
                    limit: MAX_ACP_STDOUT_BYTES,
                })?;
            if next_total > MAX_ACP_STDOUT_BYTES {
                return Err(AcpError::StdoutLimit {
                    limit: MAX_ACP_STDOUT_BYTES,
                });
            }
            let next_frame = frame.len().checked_add(take).ok_or(AcpError::FrameLimit {
                limit: MAX_ACP_FRAME_BYTES,
            })?;
            if next_frame > MAX_ACP_FRAME_BYTES {
                return Err(AcpError::FrameLimit {
                    limit: MAX_ACP_FRAME_BYTES,
                });
            }

            frame.extend_from_slice(&available[..take]);
            self.inner.consume(take);
            self.bytes_read = next_total;
            if newline.is_some() {
                return Ok(Some(frame));
            }
        }
    }
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

async fn rpc_request<W, R>(
    stdin: &mut W,
    reader: &mut AcpReader<R>,
    next_id: &mut u64,
    method: &'static str,
    params: Value,
    assistant: &mut String,
) -> Result<Value, AcpError>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let id = *next_id;
    *next_id = next_id.saturating_add(1);
    let request = JsonRpcRequest::new(method, params, Value::from(id));
    write_line(stdin, &request, method).await?;

    loop {
        let Some(message) = reader.next_message(method).await? else {
            return Err(AcpError::Closed { phase: method });
        };

        if message.get(field::METHOD).is_some() && message.get(field::ID).is_some() {
            discard_non_final_output(&message, assistant);
            handle_server_request(stdin, &message).await?;
            continue;
        }
        if message.get(field::METHOD).is_some() && message.get(field::ID).is_none() {
            discard_non_final_output(&message, assistant);
            append_agent_message_chunk(&message, assistant)?;
            continue;
        }
        if message.get(field::ID).and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if message.get(field::ERROR).is_some() {
            return Err(AcpError::Remote { phase: method });
        }
        return Ok(message.get(field::RESULT).cloned().unwrap_or(Value::Null));
    }
}

async fn write_line<W, T>(stdin: &mut W, value: &T, phase: &'static str) -> Result<(), AcpError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut encoded = serde_json::to_vec(value).map_err(|_| AcpError::Encode)?;
    encoded.push(b'\n');
    stdin
        .write_all(&encoded)
        .await
        .map_err(|_| AcpError::Write { phase })?;
    stdin.flush().await.map_err(|_| AcpError::Write { phase })
}

async fn handle_server_request<W>(stdin: &mut W, message: &Value) -> Result<(), AcpError>
where
    W: AsyncWrite + Unpin,
{
    let id = message.get(field::ID).cloned().unwrap_or(Value::Null);
    let method = message
        .get(field::METHOD)
        .and_then(Value::as_str)
        .unwrap_or_default();

    if method == "session/request_permission" || method.ends_with("/session/request_permission") {
        let outcome = permission_rejection_outcome(message);
        let response = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION,
            result: Some(json!({ "outcome": outcome })),
            error: None,
            id,
        };
        return write_line(stdin, &response, "permission response").await;
    }

    let response = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION,
        result: None,
        error: Some(JsonRpcError {
            code: METHOD_NOT_FOUND,
            message: "Method not supported by the ZeroClaw ACP client".to_string(),
            data: None,
        }),
        id,
    };
    write_line(stdin, &response, "unsupported server request response").await
}

fn permission_rejection_outcome(message: &Value) -> Value {
    if let Some(option_id) = message
        .pointer("/params/options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|option| option.get("kind").and_then(Value::as_str) == Some("reject_once"))
        .and_then(|option| option.get("optionId"))
        .and_then(Value::as_str)
    {
        return json!({ "outcome": "selected", "optionId": option_id });
    }
    json!({ "outcome": "cancelled" })
}

async fn settle_trailing_output<W, R>(
    stdin: &mut W,
    reader: &mut AcpReader<R>,
    assistant: &mut String,
) -> Result<(), AcpError>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let mut quiet_intervals = 0_usize;
    while quiet_intervals < OUTPUT_SETTLE_INTERVALS {
        match timeout(OUTPUT_SETTLE_INTERVAL, reader.next_message("output settle")).await {
            Err(_) => quiet_intervals += 1,
            Ok(Ok(None)) => return Ok(()),
            Ok(Err(error)) => return Err(error),
            Ok(Ok(Some(message))) => {
                quiet_intervals = 0;
                if message.get(field::METHOD).is_some() && message.get(field::ID).is_some() {
                    discard_non_final_output(&message, assistant);
                    handle_server_request(stdin, &message).await?;
                } else if message.get(field::METHOD).is_some() {
                    discard_non_final_output(&message, assistant);
                    append_agent_message_chunk(&message, assistant)?;
                }
            }
        }
    }
    Ok(())
}

fn append_agent_message_chunk(message: &Value, assistant: &mut String) -> Result<(), AcpError> {
    let Some(chunk) = extract_agent_message_chunk(message) else {
        return Ok(());
    };
    let next_len = assistant
        .len()
        .checked_add(chunk.len())
        .ok_or(AcpError::AssistantLimit {
            limit: MAX_ACP_ASSISTANT_BYTES,
        })?;
    if next_len > MAX_ACP_ASSISTANT_BYTES {
        return Err(AcpError::AssistantLimit {
            limit: MAX_ACP_ASSISTANT_BYTES,
        });
    }
    assistant.push_str(chunk);
    Ok(())
}

/// Grok can emit user-visible progress as `agent_message_chunk` before it
/// starts a plan or tool call. That text is not the completed answer for a
/// one-shot provider. Keep only the latest message segment after a non-final
/// ACP update; classify by protocol event, never by the model's wording.
fn discard_non_final_output(message: &Value, assistant: &mut String) {
    if is_permission_request(message) {
        assistant.clear();
        return;
    }

    let Some(method) = message.get(field::METHOD).and_then(Value::as_str) else {
        return;
    };
    if method != "session/update" && !method.ends_with("/session/update") {
        return;
    }
    let Some(update) = message.pointer("/params/update") else {
        return;
    };
    let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return;
    };
    if kind == "agent_thought_chunk" || kind == "plan" || kind.starts_with("tool_") {
        assistant.clear();
    }
}

fn is_permission_request(message: &Value) -> bool {
    let Some(method) = message.get(field::METHOD).and_then(Value::as_str) else {
        return false;
    };
    method == "session/request_permission" || method.ends_with("/session/request_permission")
}

fn select_auth_method_id(
    initialize: &Value,
    xai_api_key_available: bool,
) -> Result<String, AcpError> {
    let ids: Vec<&str> = initialize
        .get("authMethods")
        .or_else(|| initialize.get("auth_methods"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|method| {
            method
                .get("id")
                .or_else(|| method.get("methodId"))
                .and_then(Value::as_str)
        })
        .collect();

    if xai_api_key_available && ids.contains(&"xai.api_key") {
        return Ok("xai.api_key".to_string());
    }
    for preferred in ["cached_token", "xai.oauth"] {
        if ids.contains(&preferred) {
            return Ok(preferred.to_string());
        }
    }
    Err(AcpError::NoAuthenticationMethod)
}

/// Extract only documented agent text chunks. User-message echoes, thoughts,
/// tool events, and non-text payloads are deliberately ignored.
fn extract_agent_message_chunk(message: &Value) -> Option<&str> {
    let method = message.get(field::METHOD)?.as_str()?;
    if method != "session/update" && !method.ends_with("/session/update") {
        return None;
    }
    let update = message.pointer("/params/update")?;
    if update.get("sessionUpdate")?.as_str()? != "agent_message_chunk" {
        return None;
    }
    let content = update.get("content")?;
    if content.get("type")?.as_str()? != "text" {
        return None;
    }
    content.get("text")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    #[test]
    fn extracts_only_agent_text_chunks() {
        let agent = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "hello" }
                }
            }
        });
        assert_eq!(extract_agent_message_chunk(&agent), Some("hello"));

        for kind in [
            "user_message_chunk",
            "agent_thought_chunk",
            "tool_call",
            "message",
        ] {
            let echo = json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "s1",
                    "update": {
                        "sessionUpdate": kind,
                        "content": {
                            "type": "text",
                            "text": "system prompt must not be returned"
                        }
                    }
                }
            });
            assert_eq!(extract_agent_message_chunk(&echo), None);
        }
    }

    #[test]
    fn non_final_updates_discard_preceding_progress_text() {
        let mut assistant = String::new();
        let progress = json!({
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "internal progress" }
                }
            }
        });
        append_agent_message_chunk(&progress, &mut assistant).expect("progress text");
        assert_eq!(assistant, "internal progress");

        let tool_call = json!({
            "method": "session/update",
            "params": { "update": { "sessionUpdate": "tool_call" } }
        });
        discard_non_final_output(&tool_call, &mut assistant);
        assert!(assistant.is_empty());

        let final_message = json!({
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "final answer" }
                }
            }
        });
        append_agent_message_chunk(&final_message, &mut assistant).expect("final text");
        assert_eq!(assistant, "final answer");
    }

    #[test]
    fn permission_requests_discard_preceding_progress_text() {
        let mut assistant = "internal progress".to_string();
        let permission = json!({
            "method": "session/request_permission",
            "id": 7,
            "params": { "options": [] }
        });
        discard_non_final_output(&permission, &mut assistant);
        assert!(assistant.is_empty());
    }

    #[test]
    fn auth_selection_uses_explicit_api_key_then_cli_login() {
        let initialize = json!({
            "authMethods": [
                { "id": "xai.api_key" },
                { "id": "cached_token" }
            ]
        });
        assert_eq!(
            select_auth_method_id(&initialize, true).expect("API-key auth"),
            "xai.api_key"
        );
        assert_eq!(
            select_auth_method_id(&initialize, false).expect("cached auth"),
            "cached_token"
        );

        let api_key_only = json!({
            "authMethods": [{ "id": "xai.api_key" }]
        });
        assert_eq!(
            select_auth_method_id(&api_key_only, true).expect("API-key auth"),
            "xai.api_key"
        );
        assert!(matches!(
            select_auth_method_id(&api_key_only, false),
            Err(AcpError::NoAuthenticationMethod)
        ));
    }

    #[tokio::test]
    async fn permission_requests_select_the_request_reject_once_option() {
        let (mut client, mut peer) = duplex(4096);
        let request = json!({
            "jsonrpc": "2.0",
            "id": 41,
            "method": "session/request_permission",
            "params": {
                "options": [
                    { "optionId": "allow", "kind": "allow_once" },
                    { "optionId": "deny", "kind": "reject_once" }
                ]
            }
        });
        handle_server_request(&mut client, &request)
            .await
            .expect("permission response");
        drop(client);

        let mut encoded = String::new();
        peer.read_to_string(&mut encoded)
            .await
            .expect("read response");
        let response: Value = serde_json::from_str(encoded.trim()).expect("valid response");
        assert_eq!(
            response.pointer("/result/outcome/outcome"),
            Some(&Value::String("selected".to_string()))
        );
        assert_eq!(
            response.pointer("/result/outcome/optionId"),
            Some(&Value::String("deny".to_string()))
        );
        assert!(!encoded.contains("allow"));
    }

    #[test]
    fn permission_requests_cancel_without_a_reject_once_option() {
        let request = json!({
            "params": {
                "options": [
                    { "optionId": "allow", "kind": "allow_once" },
                    { "optionId": "always", "kind": "allow_always" }
                ]
            }
        });
        assert_eq!(
            permission_rejection_outcome(&request),
            json!({ "outcome": "cancelled" })
        );
    }

    #[tokio::test]
    async fn invalid_json_error_does_not_echo_the_frame() {
        let secret = "RAW_FRAME_SECRET_MUST_NOT_ESCAPE";
        let payload = format!("not-json-{secret}\n");
        let (mut peer, client) = duplex(payload.len() + 1);
        peer.write_all(payload.as_bytes())
            .await
            .expect("write frame");
        drop(peer);

        let mut reader = AcpReader::new(client);
        let error = reader
            .next_message("test")
            .await
            .expect_err("invalid JSON must fail");
        assert!(matches!(error, AcpError::InvalidJson { .. }));
        assert!(!error.to_string().contains(secret));
    }

    #[tokio::test]
    async fn frame_limit_is_enforced_before_unbounded_growth() {
        let payload = vec![b'x'; MAX_ACP_FRAME_BYTES + 1];
        let (mut peer, client) = duplex(payload.len() + 1);
        peer.write_all(&payload)
            .await
            .expect("write oversized frame");
        drop(peer);

        let mut reader = AcpReader::new(client);
        let error = reader
            .next_message("test")
            .await
            .expect_err("oversized frame must fail");
        assert!(matches!(error, AcpError::FrameLimit { .. }));
    }

    #[test]
    fn assistant_limit_is_hard_not_posthoc_truncation() {
        let mut assistant = "x".repeat(MAX_ACP_ASSISTANT_BYTES);
        let message = json!({
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "y" }
                }
            }
        });
        let error = append_agent_message_chunk(&message, &mut assistant)
            .expect_err("assistant overflow must fail");
        assert!(matches!(error, AcpError::AssistantLimit { .. }));
        assert_eq!(assistant.len(), MAX_ACP_ASSISTANT_BYTES);
    }
}
