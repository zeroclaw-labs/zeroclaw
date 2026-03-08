use crate::providers::traits::{ChatMessage, Provider, ProviderCapabilities};
use async_trait::async_trait;
use serde::Deserialize;
use std::process::Stdio;
use tokio::process::Command;

pub struct CliProvider {
    cli_type: CliType,
    cli_path: Option<String>,
    timeout_secs: u64,
    allowed_tools: Option<Vec<String>>,
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
        Self::new_with_options(cli_type, None, 300, None)
    }

    pub fn new_with_options(
        cli_type: &str,
        cli_path: Option<&str>,
        timeout_secs: u64,
        allowed_tools: Option<Vec<String>>,
    ) -> Self {
        let cli_type = CliType::from_cli_name(cli_type).unwrap_or(CliType::Claude);
        Self {
            cli_type,
            cli_path: cli_path.map(String::from),
            timeout_secs,
            allowed_tools,
        }
    }

    fn cli_command(&self) -> &str {
        self.cli_path.as_deref().unwrap_or(match self.cli_type {
            CliType::Claude => "claude",
            CliType::Gemini => "gemini",
            CliType::OpenCode => "opencode",
        })
    }

    async fn execute_cli(
        &self,
        prompt: &str,
        system_prompt: Option<&str>,
    ) -> anyhow::Result<String> {
        let full_prompt = match system_prompt {
            Some(sys) => format!("{}\n\n{}", sys, prompt),
            None => prompt.to_string(),
        };

        let mut cmd = Command::new(self.cli_command());

        match self.cli_type {
            CliType::Claude | CliType::Gemini => {
                cmd.arg("-p")
                    .arg("") // Read prompt from stdin
                    .arg("--output-format")
                    .arg("json");

                // Note: We intentionally DO NOT pass a --model flag here
                // to allow the user's local CLI configuration to determine the default.

                if let Some(ref tools) = self.allowed_tools {
                    if !tools.is_empty() {
                        let tools_arg = tools.join(",");
                        cmd.arg("--allowed-tools").arg(&tools_arg);
                    }
                }
            }
            CliType::OpenCode => {
                cmd.arg("run").arg("-");
            }
        }

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        tracing::debug!("Executing CLI command via stdin: {}", self.cli_command());

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn CLI command {}: {}", self.cli_command(), e)
        })?;

        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        let prompt_to_write = full_prompt.clone();

        // Write to stdin in a separate task to avoid deadlocks
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = stdin.write_all(prompt_to_write.as_bytes()).await {
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

        self.parse_output(stdout.trim())
    }

    fn parse_output(&self, output: &str) -> anyhow::Result<String> {
        match self.cli_type {
            CliType::Claude => self.parse_claude_output(output),
            CliType::Gemini => self.parse_gemini_output(output),
            CliType::OpenCode => self.parse_opencode_output(output),
        }
    }

    fn parse_claude_output(&self, output: &str) -> anyhow::Result<String> {
        let trimmed = output.trim();
        if !trimmed.starts_with('{') {
            return Ok(trimmed.to_string());
        }

        match serde_json::from_str::<ClaudeResponse>(trimmed) {
            Ok(parsed) => {
                if let Some(result) = parsed.result {
                    for block in result.content {
                        let text = match block {
                            ClaudeContent::Block(block) => block.text,
                            ClaudeContent::Text(text) => Some(text),
                        };
                        if let Some(text) = text {
                            return Ok(text);
                        }
                    }
                }
                anyhow::bail!("No text content found in Claude JSON response")
            }
            Err(_) => Ok(trimmed.to_string()),
        }
    }

    fn parse_gemini_output(&self, output: &str) -> anyhow::Result<String> {
        let trimmed = output.trim();
        if !trimmed.starts_with('{') {
            return Ok(trimmed.to_string());
        }

        match serde_json::from_str::<GeminiResponse>(trimmed) {
            Ok(parsed) => {
                if let Some(response) = parsed.response {
                    return Ok(response);
                }
                Ok(trimmed.to_string())
            }
            Err(_) => Ok(trimmed.to_string()),
        }
    }

    fn parse_opencode_output(&self, output: &str) -> anyhow::Result<String> {
        if output.is_empty() {
            return Err(anyhow::anyhow!("OpenCode returned empty output"));
        }

        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output) {
            if let Some(text) = parsed.get("text").and_then(|v| v.as_str()) {
                return Ok(text.to_string());
            }
            if let Some(text) = parsed.get("result").and_then(|v| v.as_str()) {
                return Ok(text.to_string());
            }
            if let Some(text) = parsed.get("content").and_then(|v| v.as_str()) {
                return Ok(text.to_string());
            }
        }

        Ok(output.to_string())
    }
}

#[async_trait]
impl Provider for CliProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: false,
            vision: false,
        }
    }

    fn supports_native_tools(&self) -> bool {
        false
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        _temperature: f64,
    ) -> anyhow::Result<String> {
        self.execute_cli(message, system_prompt).await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
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

        self.execute_cli(last_user, system).await
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        let test_prompt = "Hello";

        match self.execute_cli(test_prompt, None).await {
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
        assert_eq!(provider.timeout_secs, 300);

        let provider = CliProvider::new_with_options("gemini", Some("/usr/bin/gemini"), 600, None);
        assert_eq!(provider.cli_type, CliType::Gemini);
        assert_eq!(provider.cli_path, Some("/usr/bin/gemini".to_string()));
        assert_eq!(provider.timeout_secs, 600);
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
    }

    #[test]
    fn test_parse_opencode_json() {
        let provider = CliProvider::new("opencode");

        let json = r#"{"text": "OpenCode response"}"#;

        let result = provider.parse_opencode_output(json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "OpenCode response");
    }

    #[test]
    fn test_parse_opencode_plain() {
        let provider = CliProvider::new("opencode");

        let result = provider.parse_opencode_output("Plain text response");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Plain text response");
    }
}
