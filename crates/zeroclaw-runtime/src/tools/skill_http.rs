//! HTTP-based tool derived from a skill's `[[tools]]` section.

use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

/// Maximum response body size (1 MB).
const MAX_RESPONSE_BYTES: usize = 1_048_576;
/// HTTP request timeout (seconds).
const HTTP_TIMEOUT_SECS: u64 = 30;

/// A tool derived from a skill's `[[tools]]` section that makes HTTP requests.
pub struct SkillHttpTool {
    tool_name: String,
    tool_description: String,
    url_template: String,
    args: HashMap<String, String>,
}

impl SkillHttpTool {
    /// Create a new skill HTTP tool.
    /// The tool name is prefixed with the skill name (`skill_name__tool_name`)
    /// to prevent collisions with built-in tools.
    pub fn new(skill_name: &str, tool: &crate::skills::SkillTool) -> Self {
        Self {
            tool_name: crate::tools::skill_tool::composed_tool_name(skill_name, &tool.name),
            tool_description: tool.description.clone(),
            url_template: tool.command.clone(),
            args: tool.args.clone(),
        }
    }

    fn build_parameters_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for (name, description) in &self.args {
            properties.insert(
                name.clone(),
                serde_json::json!({
                    "type": "string",
                    "description": description
                }),
            );
            required.push(serde_json::Value::String(name.clone()));
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required
        })
    }

    /// Substitute `{{arg_name}}` placeholders in the URL template with
    /// the provided argument values.
    fn substitute_args(&self, args: &serde_json::Value) -> String {
        let mut url = self.url_template.clone();
        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let placeholder = format!("{{{{{}}}}}", key);
                let replacement = value.as_str().unwrap_or_default();
                url = url.replace(&placeholder, replacement);
            }
        }
        url
    }
}

#[async_trait]
impl Tool for SkillHttpTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.build_parameters_schema()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = self.substitute_args(&args);

        let parsed = match reqwest::Url::parse(&url) {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Invalid URL: {e}")),
                });
            }
        };
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("URL userinfo is not allowed".to_string()),
            });
        }
        if !matches!(parsed.scheme(), "http" | "https") {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Only http:// and https:// URLs are allowed, got: {url}"
                )),
            });
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "skill_http tool: reqwest client build failed"
                );
                anyhow::Error::msg(format!("Failed to build HTTP client: {e}"))
            })?;

        let response = match client.get(&url).send().await {
            Ok(resp) => resp,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("HTTP request failed: {e}")),
                });
            }
        };

        let status = response.status();
        let body = match response.bytes().await {
            Ok(bytes) => {
                let mut text = String::from_utf8_lossy(&bytes).to_string();
                if text.len() > MAX_RESPONSE_BYTES {
                    let mut b = MAX_RESPONSE_BYTES.min(text.len());
                    while b > 0 && !text.is_char_boundary(b) {
                        b -= 1;
                    }
                    text.truncate(b);
                    text.push_str("\n... [response truncated at 1MB]");
                }
                text
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to read response body: {e}")),
                });
            }
        };

        Ok(ToolResult {
            success: status.is_success(),
            output: body.into(),
            error: if status.is_success() {
                None
            } else {
                Some(format!("HTTP {}", status))
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SkillTool;
    use serde_json::json;

    fn sample_http_tool() -> SkillTool {
        let mut args = HashMap::new();
        args.insert("city".to_string(), "City name to look up".to_string());

        SkillTool {
            name: "get_weather".to_string(),
            description: "Fetch weather for a city".to_string(),
            kind: "http".to_string(),
            command: "https://api.example.com/weather?city={{city}}".to_string(),
            args,
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        }
    }

    fn wttr_in_weather_tool() -> SkillTool {
        let mut args = HashMap::new();
        args.insert(
            "location".to_string(),
            "Location to get weather for".to_string(),
        );

        SkillTool {
            name: "weather_lookup".to_string(),
            description: "Fetch weather from wttr.in".to_string(),
            kind: "http".to_string(),
            command: "https://wttr.in/{{location}}?format=j1".to_string(),
            args,
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        }
    }

    #[test]
    fn skill_http_tool_name_is_prefixed() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        assert_eq!(tool.name(), "weather_skill__get_weather");
    }

    #[test]
    fn skill_http_tool_description() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        assert_eq!(tool.description(), "Fetch weather for a city");
    }

    #[test]
    fn skill_http_tool_parameters_schema() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["city"].is_object());
        assert_eq!(schema["properties"]["city"]["type"], "string");
    }

    #[test]
    fn skill_http_tool_substitute_args() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let result = tool.substitute_args(&serde_json::json!({"city": "London"}));
        assert_eq!(result, "https://api.example.com/weather?city=London");
    }

    #[test]
    fn skill_http_can_model_minimal_wttr_weather_lookup() {
        let tool = SkillHttpTool::new("weather_skill", &wttr_in_weather_tool());

        assert_eq!(tool.name(), "weather_skill__weather_lookup");
        assert_eq!(tool.description(), "Fetch weather from wttr.in");

        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["location"]["type"], "string");
        assert_eq!(
            schema["properties"]["location"]["description"],
            "Location to get weather for"
        );
        assert!(
            schema["required"]
                .as_array()
                .expect("required array")
                .iter()
                .any(|name| name == "location")
        );

        let url = tool.substitute_args(&serde_json::json!({"location": "London"}));
        assert_eq!(url, "https://wttr.in/London?format=j1");
    }

    #[test]
    fn skill_http_tool_spec_roundtrip() {
        let tool = SkillHttpTool::new("weather_skill", &sample_http_tool());
        let spec = tool.spec();
        assert_eq!(spec.name, "weather_skill__get_weather");
        assert_eq!(spec.description, "Fetch weather for a city");
        assert_eq!(spec.parameters["type"], "object");
    }

    #[test]
    fn skill_http_tool_name_sanitized_for_provider_regex() {
        // A plugin-namespaced HTTP skill (colons) or a dotted tool name must
        // still yield a provider-valid function name, the same as shell/builtin
        // tools, socannot survive through the HTTP registration path.
        let mut st = sample_http_tool();
        st.name = "fetch.weather".to_string();
        let tool = SkillHttpTool::new("pr-review-toolkit:code-reviewer", &st);
        let name = tool.name();
        assert!(
            !name.is_empty()
                && name.len() <= 64
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "HTTP skill tool name `{name}` is not provider-valid",
        );
        // A valid name must still pass through unchanged (no spurious suffix).
        let plain = SkillHttpTool::new("weather_skill", &sample_http_tool());
        assert_eq!(plain.name(), "weather_skill__get_weather");
    }

    #[test]
    fn skill_http_tool_empty_args() {
        let st = SkillTool {
            name: "ping".to_string(),
            description: "Ping endpoint".to_string(),
            kind: "http".to_string(),
            command: "https://api.example.com/ping".to_string(),
            args: HashMap::new(),
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        };
        let tool = SkillHttpTool::new("s", &st);
        let schema = tool.parameters_schema();
        assert!(schema["properties"].as_object().unwrap().is_empty());
    }

    fn http_tool_with_command(command: &str) -> SkillHttpTool {
        let st = SkillTool {
            name: "fetch".to_string(),
            description: "test".to_string(),
            kind: "http".to_string(),
            command: command.to_string(),
            args: HashMap::new(),
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        };
        SkillHttpTool::new("test_skill", &st)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_userinfo_targeting_private_host() {
        // `{{user}}` substitution yields a userinfo prefix. Parser-level
        // reject, no opt-out (the skill author cannot allow userinfo even
        // for legitimate credentials in URL).
        let tool = http_tool_with_command("https://{{user}}@api.example.com/path");
        let result = tool
            .execute(json!({"user": "attacker@127.0.0.1"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("userinfo"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_userinfo_with_password() {
        let tool = http_tool_with_command("https://{{u}}:{{p}}@api.example.com/path");
        let result = tool
            .execute(json!({"u": "alice", "p": "secret@10.0.0.1"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("userinfo"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_userinfo_even_when_host_is_public() {
        // The userinfo reject is parser-level, not policy-level. A
        // legitimate-looking public host with userinfo is still rejected
        // so the gate cannot be bypassed by mixing public/private strings.
        let tool = http_tool_with_command("https://{{u}}@example.com/path");
        let result = tool.execute(json!({"u": "anyone"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("userinfo"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_non_http_scheme() {
        // `file://`, `gopher://`, etc. must still be rejected.
        let tool = http_tool_with_command("file://etc/passwd");
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("http") || err.contains("Invalid URL"),
            "got: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_malformed_url() {
        // The skill author supplied a template with no scheme, only a host.
        // `reqwest::Url::parse` rejects it as RelativeUrlWithoutBase.
        let tool = http_tool_with_command("example.com/ping");
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("Invalid URL"), "got: {err}");
    }
}
