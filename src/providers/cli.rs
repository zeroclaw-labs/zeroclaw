use crate::providers::traits::{ChatMessage, ChatResponse, Provider, ProviderCapabilities, TokenUsage};
use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use tokio::process::Command;
use parking_lot::Mutex;

static ANSI_ESCAPE: OnceLock<Regex> = OnceLock::new();

fn strip_ansi(input: &str) -> String {
    let re = ANSI_ESCAPE.get_or_init(|| {
        Regex::new(r"[\u001b\u009b][\[()#;?]*(?:[0-9]{1,4}(?:;[0-9]{0,4})*)?[0-9A-ORZcf-nqry=><]")
            .unwrap()
    });
    re.replace_all(input, "").to_string()
}

pub struct CliProvider {
    cli_type: CliType,
    cli_path: Option<String>,
    timeout_secs: u64,
    allowed_tools: Option<Vec<String>>,
    mcp_servers: Option<Vec<String>>,
    /// Tracks the native session ID from the CLI tool to enable session resumption and caching.
    current_session_id: Arc<Mutex<Option<String>>>,
    /// Tracks the number of messages sent in the last turn to detect history compaction.
    last_messages_count: Arc<Mutex<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CliType {
    Claude,
    Gemini,
    OpenCode,
}

impl CliType {
    pub fn from_cli_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            "gemini" => Some(Self::Gemini),
            "opencode" => Some(Self::OpenCode),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ClaudeResponse {
    result: Option<ClaudeResult>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    usage: Option<ClaudeUsage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeResult {
    #[serde(default)]
    content: Vec<ClaudeContent>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ClaudeContent {
    Block(ClaudeContentBlock),
    Text(String),
}

#[derive(Debug, Deserialize)]
struct ClaudeContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeUsage {
    #[serde(rename = "input_tokens", default)]
    input_tokens: Option<u64>,
    #[serde(rename = "output_tokens", default)]
    output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default, rename = "sessionId")]
    session_id_alt: Option<String>,
}

impl CliProvider {
    pub fn new(cli_type: &str) -> Self {
        Self::new_with_options(cli_type, None, 1800, None, None)
    }

    pub fn new_with_options(
        cli_type: &str,
        cli_path: Option<&str>,
        timeout_secs: u64,
        allowed_tools: Option<Vec<String>>,
        mcp_servers: Option<Vec<String>>,
    ) -> Self {
        let cli_type = CliType::from_cli_name(cli_type).unwrap_or(CliType::Claude);
        Self {
            cli_type,
            cli_path: cli_path.map(String::from),
            timeout_secs,
            allowed_tools,
            mcp_servers,
            current_session_id: Arc::new(Mutex::new(None)),
            last_messages_count: Arc::new(Mutex::new(0)),
        }
    }

    fn cli_command(&self) -> &str {
        self.cli_path.as_deref().unwrap_or(match self.cli_type {
            CliType::Claude => "claude",
            CliType::Gemini => "gemini",
            CliType::OpenCode => "opencode",
        })
    }

    async fn get_opencode_free_model(&self) -> String {
        // Attempt to find a free model from the live registry
        let mut cmd = Command::new(self.cli_command());
        cmd.arg("models");
        
        if let Ok(output) = cmd.output().await {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let models: Vec<&str> = stdout.lines().collect();
            
            // 1. Try to load dynamic priorities from config file
            let mut priorities = Vec::new();
            if let Some(base) = directories::BaseDirs::new() {
                let priority_path = base.home_dir().join(".zeroclaw").join("model_priorities.json");
                if priority_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&priority_path) {
                        if let Ok(p) = serde_json::from_str::<Vec<String>>(&content) {
                            priorities = p;
                        }
                    }
                }
            }

            // 2. Fallback to hardcoded benchmark-driven priorities if dynamic list is empty
            if priorities.is_empty() {
                priorities = vec![
                    "openrouter/openrouter/free".to_string(),
                    "opencode/minimax-m2.5-free".to_string(),
                    "openrouter/meta-llama/llama-3.3-70b-instruct:free".to_string(),
                    "openrouter/qwen/qwen2.5-vl-72b-instruct:free".to_string(),
                    "openrouter/deepseek/deepseek-r1:free".to_string(),
                ];
            }
            
            for p in &priorities {
                if models.contains(&p.as_str()) {
                    return p.clone();
                }
            }
            
            // Fallback to any model ending in :free
            if let Some(m) = models.iter().find(|m| m.ends_with(":free")) {
                return m.to_string();
            }
        }
        
        // Final fallback
        "opencode/minimax-m2.5-free".to_string()
    }

    fn format_messages(&self, messages: &[ChatMessage]) -> String {
        let mut formatted = String::new();
        for msg in messages {
            let role_marker = match self.cli_type {
                CliType::Gemini => {
                    if msg.role == "user" { "User: " } else if msg.role == "system" { "System: " } else { "Model: " }
                }
                CliType::Claude => {
                    if msg.role == "user" { "H: " } else if msg.role == "system" { "System: " } else { "A: " }
                }
                CliType::OpenCode => {
                    if msg.role == "user" { "User:\n" } else if msg.role == "system" { "Instruction:\n" } else { "Assistant:\n" }
                }
            };
            formatted.push_str(role_marker);
            formatted.push_str(&msg.content);
            formatted.push_str("\n\n");
        }
        formatted
    }

    async fn execute_cli(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
        full_history: Option<&[ChatMessage]>,
        model: Option<&str>,
    ) -> anyhow::Result<ChatResponse> {
        let mut cmd = Command::new(self.cli_command());
        cmd.env("NODE_OPTIONS", "--max-old-space-size=8192");

        // Use a temporary file for the system prompt to avoid stdin buffer limits
        let _system_file = if let Some(sys) = system_prompt {
            if self.cli_type == CliType::Gemini || self.cli_type == CliType::Claude {
                use std::io::Write;
                let mut temp = tempfile::NamedTempFile::new()?;
                temp.write_all(sys.as_bytes())?;
                temp.flush()?;
                let path = temp.path().to_path_buf();
                tracing::debug!("Using temporary system prompt file: {}", path.display());
                cmd.env("GEMINI_SYSTEM_MD", path);
                Some(temp)
            } else {
                None
            }
        } else {
            None
        };

        // Determine if we should resume a previous session or start fresh
        let (resume_id, last_count) = {
            let session_id_lock = self.current_session_id.lock();
            let last_count_lock = self.last_messages_count.lock();
            
            let mut resume_id = None;
            if let Some(ref id) = *session_id_lock {
                if let Some(history) = full_history {
                    // If history grew incrementally, resume. 
                    // If history shrunk (compaction), start fresh.
                    if history.len() > *last_count_lock {
                        resume_id = Some(id.clone());
                        tracing::info!(id = %id, count = *last_count_lock, "CLI Provider: Resuming session");
                    } else {
                        tracing::info!("History shrunk or reset, starting fresh CLI session");
                    }
                } else {
                    resume_id = Some(id.clone());
                    tracing::info!(id = %id, "CLI Provider: Resuming session (no history provided)");
                }
            }
            (resume_id, *last_count_lock)
        };

        // Build command arguments based on provider type and resumption state
        match self.cli_type {
            CliType::Claude => {
                // Note: Claude's -r flag is unreliable in non-interactive -p mode.
                // We fallback to full history for Claude but keep prefix caching optimization.
                if let Some(m) = model {
                    if m != "default" {
                        cmd.arg("--model").arg(m);
                    }
                }
                cmd.arg("-p")
                    .arg("") // Read prompt from stdin
                    .arg("--output-format")
                    .arg("json");
            }
            CliType::Gemini => {
                if let Some(m) = model {
                    if m != "default" {
                        cmd.arg("--model").arg(m);
                    }
                }
                if let Some(ref id) = resume_id {
                    tracing::info!("Resuming CLI session {}", id);
                    cmd.arg("-r").arg(id);
                }
                cmd.arg("-p")
                    .arg("") // Read prompt from stdin
                    .arg("--output-format")
                    .arg("json");
            }
            CliType::OpenCode => {
                cmd.arg("run");
                let m = if let Some(m) = model {
                    if m == "default" { 
                        self.get_opencode_free_model().await
                    } else { 
                        m.to_string() 
                    }
                } else {
                    self.get_opencode_free_model().await
                };
                cmd.arg("--model").arg(m);
                
                if let Some(ref id) = resume_id {
                    tracing::info!("Resuming CLI session {}", id);
                    cmd.arg("--session").arg(id);
                }
                cmd.arg("--format").arg("json").arg("-");
            }
        }

        // Determine what to pipe to stdin
        let prompt_to_write = if let Some(_) = resume_id {
            if self.cli_type == CliType::Gemini || self.cli_type == CliType::OpenCode {
                if let Some(history) = full_history {
                    // Send only the new messages added since the last turn
                    let new_msgs = &history[last_count..];
                    self.format_messages(new_msgs)
                } else {
                    prompt.to_string()
                }
            } else {
                // For Claude, send full history every turn
                if let Some(history) = full_history {
                    self.format_messages(history)
                } else {
                    prompt.to_string()
                }
            }
        } else if let Some(history) = full_history {
            // We are starting a FRESH CLI session but we have ZeroClaw history.
            // We must pipe the full history + the CURRENT prompt.
            let mut h = history.to_vec();
            
            // 1. Add current turn context if not in history
            let has_prompt = h.iter().any(|m| m.role == "user" && m.content == prompt);
            if !has_prompt {
                if let Some(sys) = system_prompt {
                    if _system_file.is_none() {
                        h.push(ChatMessage::system(sys));
                    }
                }
                h.push(ChatMessage::user(prompt));
            }
            
            self.format_messages(&h)
        } else if _system_file.is_some() {
            prompt.to_string()
        } else if let Some(sys) = system_prompt {
            self.format_messages(&[ChatMessage::system(sys), ChatMessage::user(prompt)])
        } else {
            prompt.to_string()
        };

        // Update the message count tracker
        if let Some(history) = full_history {
            *self.last_messages_count.lock() = history.len();
        }

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        tracing::debug!(
            "Executing CLI command: {} with prompt length {}",
            self.cli_command(),
            prompt_to_write.len()
        );

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn CLI command {}: {}", self.cli_command(), e)
        })?;

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        let prompt_to_write_bytes = prompt_to_write.clone();

        // Write to stdin in a separate task to avoid deadlocks
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = stdin.write_all(prompt_to_write_bytes.as_bytes()).await {
                tracing::error!("Failed to write to CLI stdin: {}", e);
            }
            let _ = stdin.flush().await;
            drop(stdin);
        });

        let output = match tokio::time::timeout(
            tokio::time::Duration::from_secs(self.timeout_secs),
            child.wait_with_output(),
        )
        .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                anyhow::bail!("Failed to execute CLI command: {}", e)
            }
            Err(_) => {
                anyhow::bail!("CLI command timed out after {} seconds", self.timeout_secs)
            }
        };

        let exit_status = output.status;

        if !exit_status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!("CLI command failed: {}", stderr);
            anyhow::bail!(
                "CLI command failed with exit code {:?}: {}",
                exit_status.code(),
                stderr
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        tracing::debug!("CLI stdout: {}", stdout);

        // Parse and potentially update the session ID
        self.parse_output(stdout.trim())
    }

    fn parse_output(&self, output: &str) -> anyhow::Result<ChatResponse> {
        let cleaned = strip_ansi(output);
        match self.cli_type {
            CliType::Claude => self.parse_claude_output(&cleaned),
            CliType::Gemini => self.parse_gemini_output(&cleaned),
            CliType::OpenCode => self.parse_opencode_output(&cleaned),
        }
    }

    fn update_session_id(&self, id: Option<String>) {
        if let Some(id) = id {
            let mut lock = self.current_session_id.lock();
            if lock.as_ref() != Some(&id) {
                tracing::debug!("CLI Provider: Linked to native session {}", id);
                *lock = Some(id);
            }
        }
    }

    /// Extracts XML-wrapped tool calls from arbitrary text.
    fn parse_tool_calls_from_text(&self, text: &str) -> Vec<crate::providers::traits::ToolCall> {
        let mut tool_calls = Vec::new();
        let re = Regex::new(r"(?s)<tool_call>\s*(.*?)\s*</tool_call>").unwrap();
        
        for cap in re.captures_iter(text) {
            if let Some(json_str) = cap.get(1) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str.as_str()) {
                    // Support both ZeroClaw {"name", "arguments"} and OpenAI {"name", "parameters"} formats
                    let name = val.get("name")
                        .or_else(|| val.get("function").and_then(|f| f.get("name")))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                        
                    let args = val.get("arguments")
                        .or_else(|| val.get("parameters"))
                        .or_else(|| val.get("function").and_then(|f| f.get("arguments")))
                        .map(|v| {
                            if v.is_string() {
                                v.as_str().unwrap().to_string()
                            } else {
                                v.to_string()
                            }
                        })
                        .unwrap_or_else(|| "{}".to_string());
                    
                    tool_calls.push(crate::providers::traits::ToolCall {
                        id: uuid::Uuid::new_v4().to_string(),
                        name,
                        arguments: args,
                    });
                }
            }
        }
        tool_calls
    }

    fn parse_claude_output(&self, output: &str) -> anyhow::Result<ChatResponse> {
        let trimmed = output.trim();
        if !trimmed.starts_with('{') {
            return Ok(ChatResponse {
                tool_calls: self.parse_tool_calls_from_text(trimmed),
                text: Some(trimmed.to_string()),
                usage: None,
                reasoning_content: None,
            });
        }

        match serde_json::from_str::<ClaudeResponse>(trimmed) {
            Ok(parsed) => {
                self.update_session_id(parsed.session_id);
                let mut text = String::new();
                if let Some(result) = parsed.result {
                    for block in result.content {
                        let t = match block {
                            ClaudeContent::Block(block) => block.text,
                            ClaudeContent::Text(text) => Some(text),
                        };
                        if let Some(t) = t {
                            text.push_str(&t);
                        }
                    }
                }
                
                let usage = parsed.usage.map(|u| TokenUsage {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                });

                Ok(ChatResponse {
                    tool_calls: self.parse_tool_calls_from_text(&text),
                    text: Some(text),
                    usage,
                    reasoning_content: None,
                })
            }
            Err(_) => Ok(ChatResponse {
                tool_calls: self.parse_tool_calls_from_text(trimmed),
                text: Some(trimmed.to_string()),
                usage: None,
                reasoning_content: None,
            }),
        }
    }

    fn parse_gemini_output(&self, output: &str) -> anyhow::Result<ChatResponse> {
        let trimmed = output.trim();
        if !trimmed.starts_with('{') {
            return Ok(ChatResponse {
                tool_calls: self.parse_tool_calls_from_text(trimmed),
                text: Some(trimmed.to_string()),
                usage: None,
                reasoning_content: None,
            });
        }

        match serde_json::from_str::<GeminiResponse>(trimmed) {
            Ok(parsed) => {
                self.update_session_id(parsed.session_id.or(parsed.session_id_alt));
                let text = parsed.response.unwrap_or_else(|| trimmed.to_string());
                Ok(ChatResponse {
                    tool_calls: self.parse_tool_calls_from_text(&text),
                    text: Some(text),
                    usage: None,
                    reasoning_content: None,
                })
            }
            Err(_) => Ok(ChatResponse {
                tool_calls: self.parse_tool_calls_from_text(trimmed),
                text: Some(trimmed.to_string()),
                usage: None,
                reasoning_content: None,
            }),
        }
    }

    fn parse_opencode_output(&self, output: &str) -> anyhow::Result<ChatResponse> {
        if output.is_empty() {
            return Err(anyhow::anyhow!("OpenCode returned empty output"));
        }

        let mut combined_text = String::new();
        let mut combined_reasoning = String::new();
        let mut session_id = None;
        let mut usage = None;

        for line in output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Opencode-cli sometimes outputs "thought" or text before/after JSON lines.
            // We try to parse each line as JSON, but if it fails, we treat it as raw text.
            match serde_json::from_str::<serde_json::Value>(trimmed) {
                Ok(parsed) => {
                    // 1. Session ID (top level or in part)
                    if session_id.is_none() {
                        if let Some(sid) = parsed.get("sessionID").or_else(|| parsed.get("sessionId")).and_then(|v| v.as_str()) {
                            session_id = Some(sid.to_string());
                        }
                    }

                    // 2. Text and reasoning parts (structured)
                    if let Some(part) = parsed.get("part") {
                        if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                            combined_text.push_str(t);
                        }
                        if let Some(r) = part.get("thought").or_else(|| part.get("reasoning")).and_then(|v| v.as_str()) {
                            combined_reasoning.push_str(r);
                        }
                        
                        // Handle tokens/usage inside part
                        if let Some(tokens) = part.get("tokens") {
                            let input = tokens.get("input").and_then(|v| v.as_u64());
                            let output = tokens.get("output").and_then(|v| v.as_u64());
                            if input.is_some() || output.is_some() {
                                usage = Some(TokenUsage {
                                    input_tokens: input,
                                    output_tokens: output,
                                });
                            }
                        }
                    }

                    // 3. Fallback for flat JSON
                    if let Some(t) = parsed.get("text").or_else(|| parsed.get("response")).or_else(|| parsed.get("result")).and_then(|v| v.as_str()) {
                        combined_text.push_str(t);
                    }
                    if let Some(r) = parsed.get("thought").or_else(|| parsed.get("reasoning")).and_then(|v| v.as_str()) {
                        combined_reasoning.push_str(r);
                    }
                }
                Err(_) => {
                    // Not JSON? Accumulate as raw text if it looks like content.
                    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
                        combined_text.push_str(trimmed);
                        combined_text.push('\n');
                    }
                }
            }
        }

        if let Some(sid) = session_id {
            self.update_session_id(Some(sid));
        }

        let final_text = if combined_text.is_empty() {
            Some(output.to_string())
        } else {
            Some(combined_text)
        };

        let reasoning_content = if combined_reasoning.is_empty() {
            None
        } else {
            Some(combined_reasoning)
        };

        let tool_calls = if let Some(ref text) = final_text {
            self.parse_tool_calls_from_text(text)
        } else {
            Vec::new()
        };

        Ok(ChatResponse {
            text: final_text,
            tool_calls,
            usage,
            reasoning_content,
        })
    }
}

#[async_trait]
impl Provider for CliProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: false,
            vision: matches!(self.cli_type, CliType::Claude | CliType::Gemini),
        }
    }

    fn supports_native_tools(&self) -> bool {
        false
    }

    fn allowed_tools(&self) -> Option<Vec<String>> {
        self.allowed_tools.clone()
    }

    fn mcp_servers(&self) -> Option<Vec<String>> {
        self.mcp_servers.clone()
    }

    fn native_session_id(&self) -> Option<String> {
        self.current_session_id.lock().clone()
    }

    fn native_session_len(&self) -> usize {
        *self.last_messages_count.lock()
    }

    fn set_native_session_state(&self, id: String, len: usize) {
        let mut session_lock = self.current_session_id.lock();
        let mut count_lock = self.last_messages_count.lock();
        if session_lock.as_ref() != Some(&id) {
            *session_lock = Some(id);
        }
        *count_lock = len;
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        let resp = self.execute_cli(message, system_prompt, None, Some(model)).await?;
        Ok(resp.text_or_empty().to_string())
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        let system = messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        let last_user = messages
            .iter()
            .rfind(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");

        let resp = self.execute_cli(last_user, system, Some(messages), Some(model)).await?;
        Ok(resp.text_or_empty().to_string())
    }

    async fn chat(
        &self,
        request: crate::providers::traits::ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        let system = request.messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.as_str());

        let last_user = request.messages
            .iter()
            .rfind(|m| m.role == "user")
            .map(|m| m.content.as_str())
            .unwrap_or("");

        self.execute_cli(last_user, system, Some(request.messages), Some(model)).await
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        let test_prompt = "Hello";

        match self.execute_cli(test_prompt, None, None, None).await {
            Ok(_) => {
                tracing::info!("CLI provider {} warmup successful", self.cli_command());
                Ok(())
            }
            Err(e) => {
                tracing::warn!("CLI provider {} warmup failed: {}", self.cli_command(), e);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_type_from_str() {
        assert_eq!(CliType::from_cli_name("claude"), Some(CliType::Claude));
        assert_eq!(CliType::from_cli_name("CLAUDE"), Some(CliType::Claude));
        assert_eq!(CliType::from_cli_name("gemini"), Some(CliType::Gemini));
        assert_eq!(CliType::from_cli_name("opencode"), Some(CliType::OpenCode));
        assert_eq!(CliType::from_cli_name("unknown"), None);
    }

    #[test]
    fn test_provider_creation() {
        let provider = CliProvider::new("claude");
        assert_eq!(provider.cli_type, CliType::Claude);
        assert_eq!(provider.timeout_secs, 1800);

        let provider = CliProvider::new_with_options("gemini", Some("/usr/bin/gemini"), 2400, None);
        assert_eq!(provider.cli_type, CliType::Gemini);
        assert_eq!(provider.cli_path, Some("/usr/bin/gemini".to_string()));
        assert_eq!(provider.timeout_secs, 2400);
    }

    #[test]
    fn test_parse_claude_json() {
        let provider = CliProvider::new("claude");

        let json = r#"{
            "result": {
                "content": [{"type": "text", "text": "Hello, world!"}]
            },
            "session_id": "test-123",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }"#;

        let result = provider.parse_claude_output(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Hello, world!");
        assert_eq!(provider.current_session_id.lock().as_deref(), Some("test-123"));
    }

    #[test]
    fn test_parse_gemini_json() {
        let provider = CliProvider::new("gemini");

        let json = r#"{
            "response": "Gemini response text",
            "sessionId": "gemini-456"
        }"#;

        let result = provider.parse_gemini_output(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Gemini response text");
        assert_eq!(provider.current_session_id.lock().as_deref(), Some("gemini-456"));
    }

    #[test]
    fn test_parse_opencode_json() {
        let provider = CliProvider::new("opencode");

        let json = r#"{"type":"step_start","sessionID":"ses_123"}
{"type":"text","sessionID":"ses_123","part":{"text":"OpenCode "}}
{"type":"text","sessionID":"ses_123","part":{"text":"response"}}
{"type":"step_finish","sessionID":"ses_123"}"#;

        let result = provider.parse_opencode_output(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "OpenCode response");
        assert_eq!(provider.current_session_id.lock().as_deref(), Some("ses_123"));
    }

    #[test]
    fn test_parse_opencode_plain() {
        let provider = CliProvider::new("opencode");

        let result = provider.parse_opencode_output("Plain text response");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Plain text response");
    }
}
