//! Minimal ACP (Agent Client Protocol) client for `grok agent stdio`.
//!
//! One-shot: initialize → authenticate → session/new → session/prompt,
//! collecting `session/update` agent message chunks. No multi-turn resume.

use serde_json::{Value, json};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

/// Run a single ACP prompt against an already-spawned `grok agent stdio` child.
///
/// `cwd` is sent in `session/new` (Grok project config / sandbox CWD).
pub async fn run_oneshot_prompt(
    stdin: &mut ChildStdin,
    stdout: ChildStdout,
    prompt: &str,
    cwd: &Path,
    model: Option<&str>,
) -> anyhow::Result<String> {
    let mut reader = BufReader::new(stdout);
    let mut next_id: u64 = 1;
    let mut assistant = String::new();

    let init = rpc_request(
        stdin,
        &mut reader,
        &mut next_id,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false
            },
            "_meta": {
                "startupHints": {
                    "nonInteractive": true,
                    "skipGitStatus": true,
                    "skipProjectLayout": true
                },
                "clientType": "zeroclaw-grok-cli",
                "clientVersion": env!("CARGO_PKG_VERSION")
            }
        }),
        &mut assistant,
    )
    .await?;

    let method_id = select_auth_method_id(&init)?;
    let _auth = rpc_request(
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

    let mut new_params = json!({
        "cwd": cwd,
        "mcpServers": []
    });
    if let Some(model_id) = model {
        new_params["_meta"] = json!({ "modelId": model_id });
    }

    let new_session = rpc_request(
        stdin,
        &mut reader,
        &mut next_id,
        "session/new",
        new_params,
        &mut assistant,
    )
    .await?;

    let session_id = new_session
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "Grok ACP session/new missing sessionId: {new_session}"
            ))
        })?
        .to_string();

    // Clear any noise collected before the real prompt turn.
    assistant.clear();

    let _prompt_result = rpc_request(
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

    let trimmed = assistant.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "Grok ACP session/prompt completed without agent message text. \
             Check authentication and that the agent produced a reply."
        );
    }
    Ok(trimmed.to_string())
}

fn select_auth_method_id(init_result: &Value) -> anyhow::Result<String> {
    let methods = init_result
        .get("authMethods")
        .or_else(|| init_result.get("auth_methods"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let ids: Vec<String> = methods
        .iter()
        .filter_map(|m| {
            m.get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
                .or_else(|| {
                    // Some wires nest id as { "id": "..." } already string.
                    m.get("methodId")
                        .and_then(|id| id.as_str())
                        .map(str::to_string)
                })
        })
        .collect();

    // Prefer API key when present; otherwise cached login / first method.
    for prefer in ["xai.api_key", "cached_token", "xai.oauth"] {
        if ids.iter().any(|id| id == prefer) {
            return Ok(prefer.to_string());
        }
    }
    if let Some(first) = ids.first() {
        return Ok(first.clone());
    }
    // Grok often works with ambient CLI login even if methods are empty;
    // still require an explicit method when the server listed none.
    anyhow::bail!(
        "Grok ACP initialize returned no auth methods. \
         Run `grok login` or set XAI_API_KEY."
    )
}

async fn rpc_request(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    next_id: &mut u64,
    method: &str,
    params: Value,
    assistant: &mut String,
) -> anyhow::Result<Value> {
    let id = *next_id;
    *next_id += 1;
    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    write_line(stdin, &req).await?;
    read_until_response(stdin, reader, &json!(id), assistant).await
}

async fn write_line(stdin: &mut ChildStdin, value: &Value) -> anyhow::Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

async fn read_until_response(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    expected_id: &Value,
    assistant: &mut String,
) -> anyhow::Result<Value> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            anyhow::bail!("Grok ACP process closed stdout before completing the request");
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = serde_json::from_str(trimmed).map_err(|err| {
            anyhow::Error::msg(format!("Grok ACP invalid JSON line: {err}: {trimmed}"))
        })?;

        // Server → client request (permission, etc.)
        if msg.get("method").is_some() && msg.get("id").is_some() {
            handle_server_request(stdin, &msg).await?;
            continue;
        }

        // Notification (no id)
        if msg.get("method").is_some() && msg.get("id").is_none() {
            if let Some(chunk) = extract_agent_message_chunk(&msg) {
                assistant.push_str(&chunk);
            }
            continue;
        }

        // Response
        if msg.get("id") == Some(expected_id) {
            if let Some(err) = msg.get("error") {
                let summary = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("ACP error");
                anyhow::bail!("Grok ACP error: {summary}");
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

async fn handle_server_request(stdin: &mut ChildStdin, msg: &Value) -> anyhow::Result<()> {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");

    if method.contains("permission") || method.ends_with("request_permission") {
        // Auto-select first option when present; otherwise cancelled.
        let option_id = msg
            .pointer("/params/options/0/optionId")
            .or_else(|| msg.pointer("/params/options/0/option_id"))
            .or_else(|| msg.pointer("/params/options/0/id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // Prefer AllowOnce-style options when labeled.
        let option_id = msg
            .pointer("/params/options")
            .and_then(|o| o.as_array())
            .and_then(|opts| {
                opts.iter()
                    .find(|o| {
                        o.get("kind")
                            .and_then(|k| k.as_str())
                            .is_some_and(|k| k.eq_ignore_ascii_case("allowonce") || k == "allow")
                    })
                    .or_else(|| opts.first())
                    .and_then(|o| {
                        o.get("optionId")
                            .or_else(|| o.get("option_id"))
                            .or_else(|| o.get("id"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
            })
            .or(option_id);

        let result = if let Some(option_id) = option_id {
            json!({
                "outcome": {
                    "outcome": "selected",
                    "optionId": option_id
                }
            })
        } else {
            json!({ "outcome": { "outcome": "cancelled" } })
        };

        write_line(
            stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }),
        )
        .await?;
        return Ok(());
    }

    // Decline unknown server requests so the agent can proceed or error cleanly.
    write_line(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not supported by zeroclaw-grok-cli client: {method}")
            }
        }),
    )
    .await?;
    Ok(())
}

/// Pull assistant text from a `session/update` notification if present.
pub fn extract_agent_message_chunk(msg: &Value) -> Option<String> {
    let method = msg.get("method")?.as_str()?;
    if method != "session/update" && !method.ends_with("session/update") {
        return None;
    }
    let update = msg.pointer("/params/update")?;

    // sessionUpdate: "agent_message_chunk" | "agent_message" | ...
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !(kind.contains("agent_message")
        || kind.contains("agentMessage")
        || kind == "message"
        || kind.is_empty())
    {
        // Still try content extraction for forward-compat shapes.
    }

    let content = update.get("content")?;
    if let Some(text) = content.get("text").and_then(|t| t.as_str()) {
        return Some(text.to_string());
    }
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    // Array of content blocks
    if let Some(arr) = content.as_array() {
        let mut out = String::new();
        for block in arr {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                out.push_str(t);
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_agent_message_chunk_from_session_update() {
        let msg = json!({
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
        assert_eq!(extract_agent_message_chunk(&msg).as_deref(), Some("hello"));
    }

    #[test]
    fn select_auth_prefers_api_key() {
        let init = json!({
            "authMethods": [
                { "id": "cached_token" },
                { "id": "xai.api_key" }
            ]
        });
        assert_eq!(select_auth_method_id(&init).unwrap(), "xai.api_key");
    }

    #[test]
    fn select_auth_falls_back_to_cached() {
        let init = json!({
            "authMethods": [ { "id": "cached_token" } ]
        });
        assert_eq!(select_auth_method_id(&init).unwrap(), "cached_token");
    }
}
