use crate::providers::traits::{
    ChatCompletionResponse, ChatContent, ChatMessage, ContentBlock, Provider, ProviderStreamSink,
    ToolDefinition,
};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub struct ClaudeCliProvider;

impl ClaudeCliProvider {
    pub fn new() -> Self {
        Self
    }

    fn normalize_model(model: &str) -> String {
        let raw = model.trim();
        if raw.is_empty() {
            return "sonnet".to_string();
        }

        let id = raw
            .rsplit_once('/')
            .map(|(_, tail)| tail)
            .unwrap_or(raw)
            .to_ascii_lowercase();

        if id.contains("opus") {
            return "opus".to_string();
        }
        if id.contains("haiku") {
            return "haiku".to_string();
        }
        if id.contains("sonnet") {
            return "sonnet".to_string();
        }

        id
    }

    fn resolve_exec_cwd() -> Option<PathBuf> {
        let from_env = std::env::var("ARIA_PROVIDER_CWD")
            .ok()
            .or_else(|| std::env::var("AFW_PROVIDER_CWD").ok())
            .or_else(|| std::env::var("HOME").ok());
        from_env.and_then(|raw| {
            let p = PathBuf::from(raw);
            if p.exists() {
                Some(p)
            } else {
                None
            }
        })
    }

    fn parse_add_dirs_env(raw: &str) -> Vec<PathBuf> {
        raw.split(|c| c == ',' || c == ':' || c == ';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect()
    }

    fn resolve_add_dirs(exec_cwd: Option<&PathBuf>) -> Vec<PathBuf> {
        if let Ok(raw) = std::env::var("ARIA_CLAUDE_ADD_DIRS")
            .or_else(|_| std::env::var("AFW_CLAUDE_ADD_DIRS"))
        {
            let parsed = Self::parse_add_dirs_env(&raw);
            if !parsed.is_empty() {
                return parsed;
            }
        }

        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(cwd) = exec_cwd {
            dirs.push(cwd.clone());
        }
        if let Ok(home) = std::env::var("HOME") {
            let p = PathBuf::from(home);
            if p.exists() && !dirs.iter().any(|d| d == &p) {
                dirs.push(p);
            }
        }
        dirs
    }

    fn apply_common_cli_flags(cmd: &mut Command, exec_cwd: Option<&PathBuf>) {
        // Non-interactive automation should not block on permission prompts.
        // Default to don't-ask; override with ARIA_CLAUDE_PERMISSION_MODE if needed.
        let permission_mode = std::env::var("ARIA_CLAUDE_PERMISSION_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "dontAsk".to_string());
        cmd.arg("--permission-mode").arg(permission_mode);

        for dir in Self::resolve_add_dirs(exec_cwd) {
            cmd.arg("--add-dir").arg(dir);
        }
    }

    fn parse_output(stdout: &str) -> anyhow::Result<String> {
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            anyhow::bail!("Claude CLI returned empty output")
        }

        let parsed: Value = serde_json::from_str(trimmed)
            .map_err(|e| anyhow::anyhow!("Invalid Claude CLI JSON output: {e}"))?;

        if let Some(result) = parsed.get("result").and_then(Value::as_str) {
            return Ok(result.to_string());
        }

        if let Some(obj) = parsed.as_object() {
            if obj.get("type").and_then(Value::as_str) == Some("result") {
                if let Some(result) = obj.get("result") {
                    return Ok(result
                        .as_str()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| result.to_string()));
                }
            }
        }

        anyhow::bail!("Claude CLI response did not contain a result field")
    }

    fn extract_json_value(text: &str) -> Option<Value> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return Some(v);
        }

        let start = trimmed.find('{')?;
        let end = trimmed.rfind('}')?;
        if end <= start {
            return None;
        }
        serde_json::from_str::<Value>(&trimmed[start..=end]).ok()
    }

    fn render_messages(messages: &[ChatMessage]) -> String {
        let mut out = String::new();

        for msg in messages {
            out.push_str(&msg.role.to_ascii_uppercase());
            out.push_str(":\n");

            match &msg.content {
                ChatContent::Text(t) => {
                    out.push_str(t);
                    out.push('\n');
                }
                ChatContent::Blocks(blocks) => {
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                out.push_str(text);
                                out.push('\n');
                            }
                            ContentBlock::ToolUse { id, name, input } => {
                                out.push_str(&format!(
                                    "[tool_use id={} name={} input={}]\n",
                                    id, name, input
                                ));
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => {
                                out.push_str(&format!(
                                    "[tool_result id={} error={} content={}]\n",
                                    tool_use_id, is_error, content
                                ));
                            }
                        }
                    }
                }
            }

            out.push('\n');
        }

        out
    }

    fn tool_contract(tools: &[ToolDefinition]) -> String {
        let mut s = String::new();
        s.push_str("You are in tool-calling mode.\n");
        s.push_str("If a tool is required, respond ONLY with strict JSON object:\n");
        s.push_str("{\"tool_uses\":[{\"id\":\"call_1\",\"name\":\"<tool>\",\"input\":{...}}]}\n");
        s.push_str("If no tool is needed, respond with normal plain text.\n\n");
        s.push_str("Available tools:\n");
        for tool in tools {
            s.push_str("- ");
            s.push_str(&tool.name);
            s.push_str(": ");
            s.push_str(&tool.description);
            s.push_str("\n  schema: ");
            s.push_str(&tool.input_schema.to_string());
            s.push('\n');
        }
        s
    }

    fn build_system_prompt(system_prompt: Option<&str>, tools: &[ToolDefinition]) -> String {
        let mut system = system_prompt.unwrap_or("").to_string();
        if !tools.is_empty() {
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str(&Self::tool_contract(tools));
        }
        system
    }

    fn parse_tool_uses_json(json: &Value) -> Vec<ContentBlock> {
        let mut content: Vec<ContentBlock> = Vec::new();
        let Some(calls) = json.get("tool_uses").and_then(Value::as_array) else {
            return content;
        };

        for (idx, call) in calls.iter().enumerate() {
            let Some(obj) = call.as_object() else {
                continue;
            };
            let name = obj
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if name.is_empty() {
                continue;
            }
            let id = obj
                .get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("call_{}", idx + 1));
            let input = obj
                .get("input")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            content.push(ContentBlock::ToolUse { id, name, input });
        }
        content
    }

    async fn run_stream_json_completion(
        &self,
        system_prompt: Option<&str>,
        transcript: &str,
        model: &str,
        sink: Option<&mut dyn ProviderStreamSink>,
    ) -> anyhow::Result<ChatCompletionResponse> {
        let resolved_model = Self::normalize_model(model);
        let mut cmd = Command::new("claude");
        let exec_cwd = Self::resolve_exec_cwd();
        cmd.arg("-p")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--model")
            .arg(resolved_model)
            .arg(transcript)
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_API_KEY_OLD")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self::apply_common_cli_flags(&mut cmd, exec_cwd.as_ref());
        if let Some(cwd) = exec_cwd {
            cmd.current_dir(cwd);
        }

        if let Some(system) = system_prompt {
            if !system.trim().is_empty() {
                cmd.arg("--append-system-prompt").arg(system);
            }
        }

        let mut child = cmd.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Claude CLI missing stdout pipe"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Claude CLI missing stderr pipe"))?;

        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut all = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if !all.is_empty() {
                    all.push('\n');
                }
                all.push_str(&line);
            }
            all
        });

        let mut emitted_len: usize = 0;
        let mut accumulated = String::new();
        let mut latest_result = String::new();
        let mut seen_tool_ids: HashSet<String> = HashSet::new();
        let mut tool_uses: Vec<ContentBlock> = Vec::new();
        let mut last_snapshot_text = String::new();
        let mut last_snapshot_thinking = String::new();
        let mut thinking_active = false;

        let mut out_lines = BufReader::new(stdout).lines();
        let mut sink = sink;
        while let Some(line) = out_lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            let msg_type = value.get("type").and_then(Value::as_str).unwrap_or("");

            // CLI assistant snapshots are cumulative; emit only appended text.
            if msg_type == "assistant" {
                let message = value.get("message").unwrap_or(&Value::Null);
                if let Some(content) = message.get("content").and_then(Value::as_array) {
                    let mut snapshot_text = String::new();
                    let mut snapshot_thinking = String::new();
                    for block in content {
                        match block.get("type").and_then(Value::as_str).unwrap_or("") {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(Value::as_str) {
                                    snapshot_text.push_str(text);
                                }
                            }
                            "tool_use" => {
                                let id = block
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                let name = block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                if !id.is_empty()
                                    && !name.is_empty()
                                    && !seen_tool_ids.contains(&id)
                                {
                                    seen_tool_ids.insert(id.clone());
                                    let input = block
                                        .get("input")
                                        .cloned()
                                        .unwrap_or_else(|| Value::Object(Default::default()));
                                    tool_uses.push(ContentBlock::ToolUse { id, name, input });
                                }
                            }
                            "thinking" => {
                                if let Some(thinking) =
                                    block.get("thinking").and_then(Value::as_str)
                                {
                                    snapshot_thinking.push_str(thinking);
                                }
                            }
                            _ => {}
                        }
                    }

                    if snapshot_text.len() > last_snapshot_text.len() {
                        let delta = &snapshot_text[last_snapshot_text.len()..];
                        if !delta.is_empty() {
                            accumulated.push_str(delta);
                            emitted_len = emitted_len.saturating_add(delta.len());
                            if let Some(s) = sink.as_deref_mut() {
                                s.on_assistant_delta(delta).await;
                            }
                        }
                    }
                    last_snapshot_text = snapshot_text;

                    if !snapshot_thinking.is_empty() && !thinking_active {
                        thinking_active = true;
                        if let Some(s) = sink.as_deref_mut() {
                            s.on_thinking_start().await;
                        }
                    }

                    if snapshot_thinking.len() > last_snapshot_thinking.len() {
                        let delta = &snapshot_thinking[last_snapshot_thinking.len()..];
                        if !delta.is_empty() {
                            if let Some(s) = sink.as_deref_mut() {
                                s.on_thinking_delta(delta).await;
                            }
                        }
                    }
                    last_snapshot_thinking = snapshot_thinking;
                }
            }

            if msg_type == "result" {
                if let Some(result) = value.get("result").and_then(Value::as_str) {
                    latest_result = result.to_string();
                }
            }
        }

        let status = child.wait().await?;
        let stderr_text = stderr_task
            .await
            .unwrap_or_else(|_| "failed to collect stderr".to_string());
        if !status.success() {
            anyhow::bail!(
                "Claude CLI failed (status: {}). stderr: {}",
                status,
                stderr_text.trim()
            );
        }

        if thinking_active {
            if let Some(s) = sink.as_deref_mut() {
                s.on_thinking_end().await;
            }
        }

        if !latest_result.is_empty() && latest_result.len() > emitted_len {
            let delta = &latest_result[emitted_len..];
            accumulated.push_str(delta);
            if let Some(s) = sink.as_deref_mut() {
                s.on_assistant_delta(delta).await;
            }
        }

        // Tool-use JSON contract (for older prompt-based tool calling path).
        if tool_uses.is_empty() {
            let parse_source = if !latest_result.is_empty() {
                latest_result.as_str()
            } else {
                accumulated.as_str()
            };
            if let Some(json) = Self::extract_json_value(parse_source) {
                tool_uses = Self::parse_tool_uses_json(&json);
            }
        }

        if !tool_uses.is_empty() {
            let mut blocks = Vec::new();
            if !accumulated.trim().is_empty() {
                blocks.push(ContentBlock::Text { text: accumulated });
            }
            blocks.extend(tool_uses);
            return Ok(ChatCompletionResponse {
                content: blocks,
                stop_reason: Some("tool_use".to_string()),
            });
        }

        let text = if !latest_result.is_empty() {
            latest_result
        } else {
            accumulated
        };
        Ok(ChatCompletionResponse {
            content: vec![ContentBlock::Text { text }],
            stop_reason: Some("end_turn".to_string()),
        })
    }
}

#[async_trait]
impl Provider for ClaudeCliProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        let resolved_model = Self::normalize_model(model);

        let mut cmd = Command::new("claude");
        let exec_cwd = Self::resolve_exec_cwd();
        cmd.arg("-p")
            .arg("--output-format")
            .arg("json")
            .arg("--model")
            .arg(resolved_model)
            .arg(message)
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("ANTHROPIC_API_KEY_OLD");
        Self::apply_common_cli_flags(&mut cmd, exec_cwd.as_ref());
        if let Some(cwd) = exec_cwd {
            cmd.current_dir(cwd);
        }

        if let Some(system) = system_prompt {
            if !system.trim().is_empty() {
                cmd.arg("--append-system-prompt").arg(system);
            }
        }

        let output = tokio::time::timeout(std::time::Duration::from_secs(180), cmd.output())
            .await
            .map_err(|_| anyhow::anyhow!("Claude CLI timed out after 180s"))??;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "Claude CLI failed (status: {}). stderr: {} stdout: {}",
                output.status,
                stderr.trim(),
                stdout.trim()
            );
        }

        let stdout = String::from_utf8(output.stdout)
            .map_err(|e| anyhow::anyhow!("Claude CLI emitted non-UTF8 output: {e}"))?;

        Self::parse_output(&stdout)
    }

    async fn chat_completion(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        temperature: f64,
        _max_tokens: u32,
    ) -> anyhow::Result<ChatCompletionResponse> {
        let system = Self::build_system_prompt(system_prompt, tools);

        let transcript = Self::render_messages(messages);
        let text = self
            .chat_with_system(Some(&system), &transcript, model, temperature)
            .await?;

        if let Some(json) = Self::extract_json_value(&text) {
            let content = Self::parse_tool_uses_json(&json);
            if !content.is_empty() {
                return Ok(ChatCompletionResponse {
                    content,
                    stop_reason: Some("tool_use".to_string()),
                });
            }
        }

        Ok(ChatCompletionResponse {
            content: vec![ContentBlock::Text { text }],
            stop_reason: Some("end_turn".to_string()),
        })
    }

    async fn chat_completion_stream(
        &self,
        system_prompt: Option<&str>,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        model: &str,
        _temperature: f64,
        _max_tokens: u32,
        sink: Option<&mut dyn ProviderStreamSink>,
    ) -> anyhow::Result<ChatCompletionResponse> {
        let system = Self::build_system_prompt(system_prompt, tools);
        let transcript = Self::render_messages(messages);
        self.run_stream_json_completion(Some(&system), &transcript, model, sink)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::ClaudeCliProvider;
    use std::path::PathBuf;

    #[test]
    fn normalize_model_aliases() {
        assert_eq!(
            ClaudeCliProvider::normalize_model("anthropic/claude-sonnet-4-5"),
            "sonnet"
        );
        assert_eq!(
            ClaudeCliProvider::normalize_model("claude-opus-4-5"),
            "opus"
        );
        assert_eq!(ClaudeCliProvider::normalize_model("haiku"), "haiku");
        assert_eq!(ClaudeCliProvider::normalize_model(""), "sonnet");
    }

    #[test]
    fn parse_result_json() {
        let out = r#"{"type":"result","result":"Hello"}"#;
        let parsed = ClaudeCliProvider::parse_output(out).unwrap();
        assert_eq!(parsed, "Hello");
    }

    #[test]
    fn parse_add_dirs_env_supports_multiple_separators() {
        let dirs = ClaudeCliProvider::parse_add_dirs_env("/tmp,/var:/usr; /bin");
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/tmp"),
                PathBuf::from("/var"),
                PathBuf::from("/usr"),
                PathBuf::from("/bin")
            ]
        );
    }
}
