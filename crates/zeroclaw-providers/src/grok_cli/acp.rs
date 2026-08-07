//! Internal bounded one-shot ACP client for `grok agent stdio`.
//!
//! The wire sequence follows Grok Build's documented example:
//! initialize → authenticate → session/new → session/prompt. Assistant text
//! arrives in `session/update` notifications. Every input frame and aggregate
//! byte count is bounded before allocation grows. Server permission requests
//! follow the explicit per-alias policy supplied by the provider and otherwise
//! select reject-once so the tool fails closed.

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
///
/// Bounded independently of Grok's published CLI limits so a single oversized
/// NDJSON line cannot grow without bound. See the Grok Build sandbox/settings
/// reference for the agent subprocess model:
/// <https://docs.x.ai/build/settings/reference>
const MAX_ACP_FRAME_BYTES: usize = 1_048_576;

/// Default aggregate stdout budget consumed during one ACP request (4 MiB).
///
/// Caps protocol frames plus native-tool updates for one one-shot turn. Alias
/// `max_acp_stdout_bytes` may raise this up to [`MAX_ACP_STDOUT_LIMIT_BYTES`].
pub(super) const DEFAULT_ACP_STDOUT_LIMIT_BYTES: usize = 4_194_304;

/// The aggregate stdout budget must admit at least one valid ACP frame.
pub(super) const MIN_ACP_STDOUT_LIMIT_BYTES: usize = MAX_ACP_FRAME_BYTES;

/// Keep a configured transport budget bounded even for tool-heavy aliases (64 MiB).
pub(super) const MAX_ACP_STDOUT_LIMIT_BYTES: usize = 64 * 1024 * 1024;

/// How the headless ACP client answers interactive permission requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AcpPermissionPolicy {
    RejectOnce,
    AllowOnce,
}

impl AcpPermissionPolicy {
    fn option_kind(self) -> &'static str {
        match self {
            Self::RejectOnce => "reject_once",
            Self::AllowOnce => "allow_once",
        }
    }
}

/// Maximum assistant text returned to the channel/runtime.
const MAX_ACP_ASSISTANT_BYTES: usize = 1_048_576;

/// Grok's published example waits for two stable 150 ms intervals after the
/// prompt response so trailing `session/update` chunks are not lost.
const OUTPUT_SETTLE_INTERVAL: Duration = Duration::from_millis(150);
const OUTPUT_SETTLE_INTERVALS: usize = 2;

/// One content block sent to Grok Build's ACP `session/prompt` endpoint.
///
/// Keep this representation typed until the JSON-RPC boundary so image bytes
/// cannot accidentally be embedded in a text block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AcpPromptContent {
    Text(String),
    Image { data: String, mime_type: String },
}

impl AcpPromptContent {
    pub(super) fn image_from_data_uri(data_uri: &str) -> Option<Self> {
        let (header, data) = data_uri.trim().split_once(',')?;
        let mime_type = header
            .strip_prefix("data:")?
            .strip_suffix(";base64")?
            .trim();
        let data = data.trim();
        if !mime_type.starts_with("image/") || data.is_empty() {
            return None;
        }
        Some(Self::Image {
            data: data.to_string(),
            mime_type: mime_type.to_string(),
        })
    }

    fn as_json(&self) -> Value {
        match self {
            Self::Text(text) => json!({ "type": "text", "text": text }),
            Self::Image { data, mime_type } => {
                json!({ "type": "image", "data": data, "mimeType": mime_type })
            }
        }
    }
}

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
    /// Remote JSON-RPC error. Public text stays phase-stable; only a sanitized
    /// integer JSON-RPC code is retained when present (never child free-text).
    #[error("Grok ACP returned an error during {phase}{code}")]
    Remote {
        phase: &'static str,
        /// Empty, or `" (code N)"` for an integer JSON-RPC error code only.
        code: &'static str,
    },
    #[error("Grok ACP {phase} response was incomplete")]
    Incomplete { phase: &'static str },
    #[error("Grok ACP initialize returned no usable authentication method")]
    NoAuthenticationMethod,
    #[error("Grok ACP initialize protocolVersion mismatch")]
    ProtocolVersion,
    #[error("Grok ACP session/prompt completed without agent message text")]
    EmptyOutput,
    #[error("Grok ACP session/prompt ended with a non-success stopReason")]
    StopReason,
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
            Self::ProtocolVersion => "grok_cli_acp_protocol_version",
            Self::EmptyOutput => "grok_cli_acp_empty_output",
            Self::StopReason => "grok_cli_acp_stop_reason",
            Self::Encode => "grok_cli_acp_encode_failed",
        }
    }
}

/// Map a small set of well-known integer JSON-RPC codes to fixed public
/// suffixes. Unknown or non-integer codes stay empty so hostile strings never
/// escape into provider errors.
fn remote_error(phase: &'static str, message: &Value) -> AcpError {
    let code = message
        .pointer("/error/code")
        .and_then(Value::as_i64)
        .and_then(|code| match code {
            -32700 => Some(" (code -32700)"),
            -32600 => Some(" (code -32600)"),
            -32601 => Some(" (code -32601)"),
            -32602 => Some(" (code -32602)"),
            -32603 => Some(" (code -32603)"),
            _ => None,
        })
        .unwrap_or("");
    AcpError::Remote { phase, code }
}

/// Exact request/response ID equality for this client.
///
/// Accepts only integer JSON numbers equal to `expected`, or a string whose
/// entire content is the decimal form of `expected`. Fractional numbers (e.g.
/// `1.9`) must not match, per JSON-RPC 2.0's guidance against fractional IDs.
fn json_rpc_id_matches(value: Option<&Value>, expected: u64) -> bool {
    let Some(value) = value else {
        return false;
    };
    match value {
        Value::Number(number) => {
            if let Some(as_u64) = number.as_u64() {
                return as_u64 == expected;
            }
            // Reject values that only fit as f64 (including 1.9 -> 1 casts).
            false
        }
        Value::String(text) => text.as_str() == expected.to_string(),
        _ => false,
    }
}

fn message_session_id(message: &Value) -> Option<&str> {
    message
        .pointer("/params/sessionId")
        .or_else(|| message.pointer("/params/session_id"))
        .and_then(Value::as_str)
}

/// Drop notifications and server requests that do not target the one-shot
/// session this client created. Missing sessionId is treated as non-matching
/// once a session exists.
fn session_matches(message: &Value, session_id: Option<&str>) -> bool {
    let Some(expected) = session_id else {
        return true;
    };
    message_session_id(message) == Some(expected)
}

/// Run one prompt against an already-spawned `grok agent stdio` child.
pub(super) async fn run_oneshot_prompt<W, R>(
    stdin: &mut W,
    stdout: R,
    prompt: &[AcpPromptContent],
    cwd: &Path,
    xai_api_key_available: bool,
    permission_policy: AcpPermissionPolicy,
    max_stdout_bytes: usize,
) -> Result<String, AcpError>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let mut reader = AcpReader::new(stdout, max_stdout_bytes);
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
        permission_policy,
        None,
    )
    .await?;
    validate_protocol_version(&initialize)?;

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
        permission_policy,
        None,
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
        permission_policy,
        None,
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
    let prompt_result = rpc_request(
        stdin,
        &mut reader,
        &mut next_id,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": prompt.iter().map(AcpPromptContent::as_json).collect::<Vec<_>>()
        }),
        &mut assistant,
        permission_policy,
        Some(session_id.as_str()),
    )
    .await?;
    reject_failed_stop_reason(&prompt_result)?;

    settle_trailing_output(
        stdin,
        &mut reader,
        &mut assistant,
        permission_policy,
        Some(session_id.as_str()),
    )
    .await?;
    let trimmed = assistant.trim();
    if trimmed.is_empty() {
        return Err(AcpError::EmptyOutput);
    }
    Ok(trimmed.to_string())
}

struct AcpReader<R> {
    inner: BufReader<R>,
    bytes_read: usize,
    max_stdout_bytes: usize,
    /// Bytes of the current NDJSON line that have already been consumed from
    /// the underlying reader. Must survive `tokio::time::timeout` cancellation
    /// of `next_message` / `read_frame` (for example during output settle) so a
    /// frame split across a quiet interval is not dropped after `consume()`.
    pending_frame: Vec<u8>,
}

impl<R> AcpReader<R>
where
    R: AsyncRead + Unpin,
{
    fn new(reader: R, max_stdout_bytes: usize) -> Self {
        Self {
            inner: BufReader::new(reader),
            bytes_read: 0,
            max_stdout_bytes,
            pending_frame: Vec::new(),
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
        loop {
            let available = self
                .inner
                .fill_buf()
                .await
                .map_err(|_| AcpError::Read { phase })?;
            if available.is_empty() {
                return if self.pending_frame.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(std::mem::take(&mut self.pending_frame)))
                };
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |position| position + 1);
            let next_total = self
                .bytes_read
                .checked_add(take)
                .ok_or(AcpError::StdoutLimit {
                    limit: self.max_stdout_bytes,
                })?;
            if next_total > self.max_stdout_bytes {
                return Err(AcpError::StdoutLimit {
                    limit: self.max_stdout_bytes,
                });
            }
            let next_frame =
                self.pending_frame
                    .len()
                    .checked_add(take)
                    .ok_or(AcpError::FrameLimit {
                        limit: MAX_ACP_FRAME_BYTES,
                    })?;
            if next_frame > MAX_ACP_FRAME_BYTES {
                return Err(AcpError::FrameLimit {
                    limit: MAX_ACP_FRAME_BYTES,
                });
            }

            self.pending_frame.extend_from_slice(&available[..take]);
            self.inner.consume(take);
            self.bytes_read = next_total;
            if newline.is_some() {
                return Ok(Some(std::mem::take(&mut self.pending_frame)));
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
    permission_policy: AcpPermissionPolicy,
    session_id: Option<&str>,
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
            if session_matches(&message, session_id) {
                discard_non_final_output(&message, assistant);
                handle_server_request(stdin, &message, permission_policy).await?;
            } else {
                // Wrong-session permission requests still need a terminal
                // answer so the child does not wait forever.
                handle_server_request_cancelled(stdin, &message).await?;
            }
            continue;
        }
        if message.get(field::METHOD).is_some() && message.get(field::ID).is_none() {
            if session_matches(&message, session_id) {
                discard_non_final_output(&message, assistant);
                append_agent_message_chunk(&message, assistant)?;
            }
            continue;
        }
        // Bare error responses with null/missing id still fail the turn.
        if message.get(field::ERROR).is_some()
            && (message.get(field::ID).is_none() || message.get(field::ID) == Some(&Value::Null))
        {
            return Err(remote_error(method, &message));
        }
        if !json_rpc_id_matches(message.get(field::ID), id) {
            continue;
        }
        if message.get(field::ERROR).is_some() {
            return Err(remote_error(method, &message));
        }
        return Ok(message.get(field::RESULT).cloned().unwrap_or(Value::Null));
    }
}

fn validate_protocol_version(initialize: &Value) -> Result<(), AcpError> {
    let Some(version) = initialize.get("protocolVersion") else {
        return Err(AcpError::ProtocolVersion);
    };
    let accepted = match version {
        Value::Number(number) => number.as_u64() == Some(ACP_PROTOCOL_VERSION),
        // Exact decimal string form only (no float-like "1.0").
        Value::String(text) => text.as_str() == ACP_PROTOCOL_VERSION.to_string(),
        _ => false,
    };
    if accepted {
        Ok(())
    } else {
        Err(AcpError::ProtocolVersion)
    }
}

/// Fail closed on explicit non-success stop reasons from session/prompt.
/// Known success tokens only; any other value (including hostile free text)
/// becomes a stable StopReason error without echoing the child string.
fn reject_failed_stop_reason(prompt_result: &Value) -> Result<(), AcpError> {
    let Some(stop_reason) = prompt_result
        .get("stopReason")
        .or_else(|| prompt_result.get("stop_reason"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    match stop_reason {
        "end_turn" | "end_turn_tool" | "max_tokens" | "" => Ok(()),
        _ => Err(AcpError::StopReason),
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

async fn handle_server_request<W>(
    stdin: &mut W,
    message: &Value,
    permission_policy: AcpPermissionPolicy,
) -> Result<(), AcpError>
where
    W: AsyncWrite + Unpin,
{
    let id = message.get(field::ID).cloned().unwrap_or(Value::Null);
    let method = message
        .get(field::METHOD)
        .and_then(Value::as_str)
        .unwrap_or_default();

    if method == "session/request_permission" || method.ends_with("/session/request_permission") {
        let outcome = permission_outcome(message, permission_policy);
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

/// Cancel a server request without selecting allow/reject options. Used when
/// the request does not target the one-shot session this client owns.
async fn handle_server_request_cancelled<W>(stdin: &mut W, message: &Value) -> Result<(), AcpError>
where
    W: AsyncWrite + Unpin,
{
    let id = message.get(field::ID).cloned().unwrap_or(Value::Null);
    let method = message
        .get(field::METHOD)
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method == "session/request_permission" || method.ends_with("/session/request_permission") {
        let response = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION,
            result: Some(json!({ "outcome": { "outcome": "cancelled" } })),
            error: None,
            id,
        };
        return write_line(stdin, &response, "permission cancel response").await;
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

fn permission_outcome(message: &Value, permission_policy: AcpPermissionPolicy) -> Value {
    if let Some(option_id) = message
        .pointer("/params/options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|option| {
            option.get("kind").and_then(Value::as_str) == Some(permission_policy.option_kind())
        })
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
    permission_policy: AcpPermissionPolicy,
    session_id: Option<&str>,
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
                    if session_matches(&message, session_id) {
                        discard_non_final_output(&message, assistant);
                        handle_server_request(stdin, &message, permission_policy).await?;
                    } else {
                        handle_server_request_cancelled(stdin, &message).await?;
                    }
                } else if message.get(field::METHOD).is_some()
                    && session_matches(&message, session_id)
                {
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
    fn json_rpc_id_matches_requires_exact_integer_or_decimal_string() {
        assert!(json_rpc_id_matches(Some(&json!(1)), 1));
        assert!(json_rpc_id_matches(Some(&json!("1")), 1));
        assert!(!json_rpc_id_matches(Some(&json!(1.9)), 1));
        assert!(!json_rpc_id_matches(Some(&json!(1.0)), 1));
        assert!(!json_rpc_id_matches(Some(&json!("1.0")), 1));
        assert!(!json_rpc_id_matches(Some(&json!("01")), 1));
        assert!(!json_rpc_id_matches(Some(&json!(2)), 1));
        assert!(!json_rpc_id_matches(None, 1));
    }

    #[test]
    fn public_acp_errors_do_not_echo_child_controlled_strings() {
        let hostile_code = remote_error(
            "session/prompt",
            &json!({ "error": { "code": "EVIL_STRING", "message": "leak me" } }),
        );
        let display = hostile_code.to_string();
        assert!(!display.contains("EVIL_STRING"));
        assert!(!display.contains("leak me"));
        assert!(display.contains("session/prompt"));

        let known_code = remote_error(
            "initialize",
            &json!({ "error": { "code": -32602, "message": "Invalid params" } }),
        );
        assert!(known_code.to_string().contains("(code -32602)"));
        assert!(!known_code.to_string().contains("Invalid params"));

        let version_err = AcpError::ProtocolVersion;
        assert_eq!(
            version_err.to_string(),
            "Grok ACP initialize protocolVersion mismatch"
        );
        assert!(!version_err.to_string().contains("999"));

        let stop_err = AcpError::StopReason;
        assert_eq!(
            stop_err.to_string(),
            "Grok ACP session/prompt ended with a non-success stopReason"
        );
        assert!(!stop_err.to_string().contains("hostile"));
    }

    #[test]
    fn session_matches_requires_created_session_id() {
        let own = json!({
            "method": "session/update",
            "params": { "sessionId": "own-session", "update": {} }
        });
        let other = json!({
            "method": "session/update",
            "params": { "sessionId": "other-session", "update": {} }
        });
        let missing = json!({ "method": "session/update", "params": { "update": {} } });
        assert!(session_matches(&own, Some("own-session")));
        assert!(!session_matches(&other, Some("own-session")));
        assert!(!session_matches(&missing, Some("own-session")));
        assert!(session_matches(&other, None));
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
    async fn partial_ndjson_frame_survives_settle_timeout_cancellation() {
        // A quiet-interval timeout must not drop bytes already consumed from
        // the underlying reader while a newline-delimited frame is incomplete.
        let (client, mut peer) = duplex(4096);
        let mut reader = AcpReader::new(client, DEFAULT_ACP_STDOUT_LIMIT_BYTES);

        let full = concat!(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"SPLIT_FRAME_OK"}}}}"#,
            "\n"
        );
        let split_at = full.len() / 2;
        peer.write_all(&full.as_bytes()[..split_at])
            .await
            .expect("write frame prefix");
        peer.flush().await.expect("flush prefix");
        // Let the duplex deliver the prefix before the settle timer starts so
        // the cancelled read has already consumed into pending_frame.
        tokio::task::yield_now().await;

        // Cancel mid-frame after the settle interval budget; prefix must remain
        // in AcpReader::pending_frame rather than being dropped with the future.
        let cancelled = timeout(OUTPUT_SETTLE_INTERVAL, reader.next_message("output settle")).await;
        assert!(
            cancelled.is_err(),
            "prefix-only read must not complete a frame before the timeout"
        );
        assert!(
            !reader.pending_frame.is_empty(),
            "consumed frame prefix must be retained across timeout cancellation"
        );

        peer.write_all(&full.as_bytes()[split_at..])
            .await
            .expect("write frame suffix");
        peer.flush().await.expect("flush suffix");
        drop(peer);

        let message = reader
            .next_message("output settle")
            .await
            .expect("complete frame after resume")
            .expect("message present");
        assert_eq!(
            extract_agent_message_chunk(&message),
            Some("SPLIT_FRAME_OK")
        );
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
        handle_server_request(&mut client, &request, AcpPermissionPolicy::RejectOnce)
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

    #[tokio::test]
    async fn permission_requests_select_the_request_allow_once_option() {
        let (mut client, mut peer) = duplex(4096);
        let request = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "session/request_permission",
            "params": {
                "options": [
                    { "optionId": "allow", "kind": "allow_once" },
                    { "optionId": "deny", "kind": "reject_once" }
                ]
            }
        });
        handle_server_request(&mut client, &request, AcpPermissionPolicy::AllowOnce)
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
            Some(&Value::String("allow".to_string()))
        );
    }

    #[test]
    fn permission_requests_cancel_without_the_policy_option() {
        let request = json!({
            "params": {
                "options": [
                    { "optionId": "allow", "kind": "allow_once" },
                    { "optionId": "always", "kind": "allow_always" }
                ]
            }
        });
        assert_eq!(
            permission_outcome(&request, AcpPermissionPolicy::RejectOnce),
            json!({ "outcome": "cancelled" })
        );
        assert_eq!(
            permission_outcome(
                &json!({
                    "params": {
                        "options": [
                            { "optionId": "deny", "kind": "reject_once" },
                            { "optionId": "always", "kind": "allow_always" }
                        ]
                    }
                }),
                AcpPermissionPolicy::AllowOnce
            ),
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

        let mut reader = AcpReader::new(client, DEFAULT_ACP_STDOUT_LIMIT_BYTES);
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

        let mut reader = AcpReader::new(client, DEFAULT_ACP_STDOUT_LIMIT_BYTES);
        let error = reader
            .next_message("test")
            .await
            .expect_err("oversized frame must fail");
        assert!(matches!(error, AcpError::FrameLimit { .. }));
    }

    #[tokio::test]
    async fn aggregate_stdout_limit_is_configurable() {
        let (mut peer, client) = duplex(16);
        peer.write_all(b"{}\n{}\n").await.expect("write ACP frames");
        drop(peer);

        let mut reader = AcpReader::new(client, 5);
        reader
            .next_message("test")
            .await
            .expect("first frame fits configured aggregate limit");
        let error = reader
            .next_message("test")
            .await
            .expect_err("second frame must exceed configured aggregate limit");
        assert!(matches!(error, AcpError::StdoutLimit { limit: 5 }));
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
