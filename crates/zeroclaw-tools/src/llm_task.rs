//! Lightweight LLM task tool for structured JSON-only sub-calls.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_providers::ProviderDispatch;

/// Tool that runs a single prompt through an LLM and optionally validates
/// the response against a JSON Schema. No tools are provided to the LLM —
/// this is a pure text-in, text-out (or JSON-out) call.
pub struct LlmTaskTool {
    security: Arc<SecurityPolicy>,
    /// Root config snapshot for alias-aware provider construction.
    /// Required so that `create_model_provider_for_alias` can look up
    /// typed alias-specific fields (e.g. `requires_openai_auth`).
    config: Arc<zeroclaw_config::schema::Config>,
    /// Provider family name (e.g. "openai", "openrouter").
    family: String,
    /// Provider alias within the family (e.g. "codex", "primary").
    /// Together with `family` and `config`, this enables alias-aware
    /// provider construction that preserves alias-specific configuration.
    alias: String,
    /// Default model from root config.
    default_model: String,
    /// Default temperature from root config. `None` means no temperature
    /// is sent on the wire; provider applies its own default.
    default_temperature: Option<f64>,
    /// API key for model_provider authentication.
    api_key: Option<String>,
    /// ModelProvider runtime options inherited from root config.
    provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions,
}

impl LlmTaskTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        config: Arc<zeroclaw_config::schema::Config>,
        family: String,
        alias: String,
        default_model: String,
        default_temperature: Option<f64>,
        api_key: Option<String>,
        provider_runtime_options: zeroclaw_providers::ModelProviderRuntimeOptions,
    ) -> Self {
        Self {
            security,
            config,
            family,
            alias,
            default_model,
            default_temperature,
            api_key,
            provider_runtime_options,
        }
    }

    /// Construct the model provider using the alias-aware factory so that
    /// typed alias-specific configuration is preserved.
    ///
    /// Runs on a blocking worker because the factory may synchronously
    /// drive `reqwest::blocking::Client` (e.g. for Qwen / MiniMax
    /// `oauth_refresh_token` exchange).  Calling that directly on a
    /// Tokio async worker is a prohibited blocking operation that may
    /// panic instead of returning a normal tool error.
    async fn build_model_provider(&self) -> anyhow::Result<Box<dyn ModelProvider>> {
        // Clone owned data so the blocking closure does not borrow `self`.
        let config = Arc::clone(&self.config);
        let family = self.family.clone();
        let alias = self.alias.clone();
        let api_key = self.api_key.clone();
        let options = self.provider_runtime_options.clone();

        let result = tokio::task::spawn_blocking(move || {
            zeroclaw_providers::create_model_provider_for_alias(
                &config,
                &family,
                &alias,
                api_key.as_deref(),
                &options,
            )
        })
        .await;

        match result {
            Ok(provider_result) => provider_result,
            Err(join_error) => {
                if join_error.is_cancelled() {
                    anyhow::bail!("Provider construction task cancelled: {join_error}");
                } else {
                    // The blocking task panicked — surface the panic provenance
                    // rather than disguising it as an authentication error.
                    anyhow::bail!(
                        "Provider construction panicked on the blocking worker: {join_error}"
                    );
                }
            }
        }
    }
}

#[async_trait]
impl Tool for LlmTaskTool {
    fn name(&self) -> &str {
        "llm_task"
    }

    fn description(&self) -> &str {
        "Run a prompt through an LLM with no tool access and return the response. \
         Optionally validates the output against a JSON Schema. Ideal for structured \
         data extraction, classification, summarization, and transformation tasks."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The prompt to send to the LLM."
                },
                "schema": {
                    "type": "object",
                    "description": "Optional JSON Schema to validate the LLM response against. \
                                    When provided, the LLM is instructed to return valid JSON \
                                    matching this schema."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override (e.g. 'anthropic/claude-sonnet-4-6'). \
                                    Defaults to the configured default model."
                },
                "temperature": {
                    "type": "number",
                    "description": "Optional temperature override (0.0-2.0). \
                                    Defaults to the configured default temperature."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "llm_task")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        // Extract required prompt
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing or empty required parameter: prompt".to_string()),
                });
            }
        };

        // Extract optional overrides
        let schema = args.get("schema").and_then(|v| v.as_object());
        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.default_model);
        let temperature = args
            .get("temperature")
            .and_then(|v| v.as_f64())
            .or(self.default_temperature);

        // Build the effective prompt, adding JSON schema instructions when needed
        let effective_prompt = if let Some(schema_obj) = schema {
            let schema_json =
                serde_json::to_string_pretty(&serde_json::Value::Object(schema_obj.clone()))
                    .unwrap_or_else(|_| "{}".to_string());
            format!(
                "{prompt}\n\n\
                 IMPORTANT: You MUST respond with valid JSON that conforms to this schema:\n\
                 ```json\n{schema_json}\n```\n\
                 Respond ONLY with the JSON object, no explanation or markdown."
            )
        } else {
            prompt.to_string()
        };

        // Create model_provider via the alias-aware factory.
        // The factory is dispatched on a blocking worker (see `build_model_provider`)
        // because it may synchronously drive `reqwest::blocking::Client`.
        let model_provider: Box<dyn ModelProvider> = match self.build_model_provider().await {
            Ok(p) => p,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to create model_provider: {e}")),
                });
            }
        };

        // Make the LLM call (no tools, no agent loop). `temperature` is
        // already Option<f64>; pass straight through. None omits the field
        // on the wire so the provider applies its own default.
        let response = match ProviderDispatch::from_ref(&*model_provider)
            .simple_chat(&effective_prompt, model, temperature)
            .await
        {
            Ok(text) => text,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("LLM call failed: {e}")),
                });
            }
        };

        // If schema was provided, validate the response
        if let Some(schema_obj) = schema {
            let schema_value = serde_json::Value::Object(schema_obj.clone());
            match validate_json_response(&response, &schema_value) {
                Ok(validated_json) => Ok(ToolResult {
                    success: true,
                    output: validated_json.into(),
                    error: None,
                }),
                Err(validation_error) => Ok(ToolResult {
                    success: false,
                    output: response.into(),
                    error: Some(format!("Schema validation failed: {validation_error}")),
                }),
            }
        } else {
            Ok(ToolResult {
                success: true,
                output: response.into(),
                error: None,
            })
        }
    }
}

fn validate_json_response(response: &str, schema: &serde_json::Value) -> Result<String, String> {
    // Strip markdown code fences if the LLM wrapped the response
    let trimmed = response.trim();
    let json_str = if trimmed.starts_with("```") {
        trimmed
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
    } else {
        trimmed
    };

    // Parse as JSON
    let parsed: serde_json::Value =
        serde_json::from_str(json_str).map_err(|e| format!("Invalid JSON: {e}"))?;

    // Check required fields
    if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
        for req in required {
            if let Some(field_name) = req.as_str()
                && parsed.get(field_name).is_none()
            {
                return Err(format!("Missing required field: {field_name}"));
            }
        }
    }

    // Check property types
    if let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) {
        for (prop_name, prop_schema) in properties {
            if let Some(value) = parsed.get(prop_name)
                && let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str())
                && !type_matches(value, expected_type)
            {
                return Err(format!(
                    "Field '{prop_name}' has wrong type: expected {expected_type}, \
                             got {}",
                    json_type_name(value)
                ));
            }
        }
    }

    // Return the cleaned, re-serialized JSON
    serde_json::to_string(&parsed).map_err(|e| format!("JSON serialization error: {e}"))
}

/// Check whether a JSON value matches an expected JSON Schema type string.
fn type_matches(value: &serde_json::Value, expected: &str) -> bool {
    match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true, // Unknown type — accept
    }
}

/// Return a human-readable type name for a JSON value.
fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Schema validation tests ──────────────────────────────────────

    #[test]
    fn validate_valid_json_against_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name", "age"]
        });

        let response = r#"{"name": "Alice", "age": 30}"#;
        let result = validate_json_response(response, &schema);
        assert!(result.is_ok());

        let parsed: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["name"], "Alice");
        assert_eq!(parsed["age"], 30);
    }

    #[test]
    fn validate_missing_required_field() {
        let schema = json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "score": { "type": "number" }
            },
            "required": ["title", "score"]
        });

        let response = r#"{"title": "Test"}"#;
        let result = validate_json_response(response, &schema);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("Missing required field: score")
        );
    }

    #[test]
    fn validate_wrong_type() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer" }
            },
            "required": ["count"]
        });

        let response = r#"{"count": "not_a_number"}"#;
        let result = validate_json_response(response, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("wrong type"));
    }

    #[test]
    fn validate_strips_markdown_code_fences() {
        let schema = json!({
            "type": "object",
            "properties": {
                "result": { "type": "string" }
            },
            "required": ["result"]
        });

        let response = "```json\n{\"result\": \"ok\"}\n```";
        let result = validate_json_response(response, &schema);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_invalid_json() {
        let schema = json!({ "type": "object" });
        let response = "this is not json at all";
        let result = validate_json_response(response, &schema);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid JSON"));
    }

    #[test]
    fn validate_optional_fields_accepted() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "bio": { "type": "string" }
            },
            "required": ["name"]
        });

        // bio is optional, so this should pass
        let response = r#"{"name": "Bob"}"#;
        let result = validate_json_response(response, &schema);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_all_type_checks() {
        assert!(type_matches(&json!("hello"), "string"));
        assert!(!type_matches(&json!(42), "string"));

        assert!(type_matches(&json!(2.72), "number"));
        assert!(type_matches(&json!(42), "number"));
        assert!(!type_matches(&json!("42"), "number"));

        assert!(type_matches(&json!(42), "integer"));
        assert!(!type_matches(&json!(2.72), "integer"));

        assert!(type_matches(&json!(true), "boolean"));
        assert!(!type_matches(&json!(1), "boolean"));

        assert!(type_matches(&json!([1, 2]), "array"));
        assert!(!type_matches(&json!({}), "array"));

        assert!(type_matches(&json!({}), "object"));
        assert!(!type_matches(&json!([]), "object"));

        assert!(type_matches(&json!(null), "null"));

        // Unknown types are accepted
        assert!(type_matches(&json!("anything"), "custom_type"));
    }

    // ── Tool trait tests ─────────────────────────────────────────────

    /// Helper: build an LlmTaskTool with default config for tests that
    /// don't exercise alias-specific behavior.
    fn default_tool() -> LlmTaskTool {
        LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(zeroclaw_config::schema::Config::default()),
            "openrouter".to_string(),
            "default".to_string(),
            "test-model".to_string(),
            Some(0.7),
            None,
            zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        )
    }

    #[test]
    fn tool_metadata() {
        let tool = default_tool();

        assert_eq!(tool.name(), "llm_task");
        assert!(tool.description().contains("LLM"));

        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["prompt"].is_object());
        assert!(schema["properties"]["schema"].is_object());
        assert!(schema["properties"]["model"].is_object());
        assert!(schema["properties"]["temperature"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "prompt");
    }

    #[tokio::test]
    async fn execute_missing_prompt_returns_error() {
        let tool = default_tool();

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("prompt"));
    }

    #[tokio::test]
    async fn execute_empty_prompt_returns_error() {
        let tool = default_tool();

        let result = tool.execute(json!({"prompt": "  "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("prompt"));
    }

    #[tokio::test]
    async fn execute_with_invalid_provider_returns_error() {
        let tool = LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(zeroclaw_config::schema::Config::default()),
            "nonexistent_provider_xyz".to_string(),
            "default".to_string(),
            "test-model".to_string(),
            Some(0.7),
            None,
            zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        );

        let result = tool
            .execute(json!({"prompt": "Hello world"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("model_provider"));
    }

    // ── Regression: alias-specific provider config ───────────────────

    /// Control: ordinary providers without alias-specific config are
    /// unaffected by the alias-aware factory.  Must pass both before
    /// and after the fix.
    #[test]
    fn llm_task_normal_provider_unaffected() {
        use zeroclaw_config::schema::Config;

        let config = Config::default();
        let default_opts = zeroclaw_providers::ModelProviderRuntimeOptions::default();

        // Both factories should produce equivalent providers for a
        // normal (non-alias-specific) provider like "openrouter".
        let alias_aware = zeroclaw_providers::create_model_provider_for_alias(
            &config,
            "openrouter",
            "default",
            None,
            &default_opts,
        )
        .expect("alias-aware construction must succeed");

        let legacy = zeroclaw_providers::create_model_provider_with_options(
            "openrouter",
            None,
            &default_opts,
        )
        .expect("legacy construction must succeed");

        // Both should agree on capabilities for a normal provider.
        assert_eq!(
            legacy.capabilities().native_tool_calling,
            alias_aware.capabilities().native_tool_calling,
            "normal provider must produce identical capabilities from both factories"
        );
    }

    // ── Async-safety regression: OAuth refresh-token construction ────

    /// Serialisation lock for tests that mutate the process-global
    /// `QWEN_OAUTH_ENDPOINT_OVERRIDE`.  Without this, two `#[tokio::test]`
    /// tasks running in parallel can stomp on each other's mock endpoint.
    static OAUTH_TEST_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

    /// Regression for the async-unsafe provider construction path.
    ///
    /// When an alias carries `oauth_refresh_token`, the provider factory
    /// calls `refresh_qwen_oauth_access_token`, which synchronously
    /// drives `reqwest::blocking::Client`.  On the old (unfixed) code
    /// this construction ran directly on the Tokio async worker — a
    /// prohibited blocking operation that may panic instead of returning
    /// a normal tool error.
    ///
    /// This test exercises the full `LlmTaskTool::execute()` path with
    /// a Qwen alias whose `oauth_refresh_token` is set.  Both the OAuth
    /// token exchange AND the subsequent chat request are intercepted by
    /// a single local Wiremock server — no external DNS, proxy, or
    /// network dependency exists.
    ///
    /// Asserts:
    /// - The OAuth token endpoint was hit exactly once.
    /// - The chat endpoint was hit exactly once (proves provider
    ///   construction succeeded AND the LLM call was dispatched).
    /// - The tool returns a successful result with the exact mocked
    ///   response content — a provider construction failure cannot
    ///   satisfy this assertion.
    // Holding `std::sync::Mutex` across `.await` is intentional here:
    // the lock serialises access to the process-global endpoint override
    // and is never contended from within the same task.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn llm_task_oauth_refresh_token_construction_is_async_safe() {
        use std::collections::HashMap;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_config::schema::{Config, ModelProviderConfig, QwenModelProviderConfig};

        let _lock = OAUTH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // ── 1. Local mock server for both OAuth and chat ───────────
        let mock_server = MockServer::start().await;
        let local_base = mock_server.uri();

        // OAuth token exchange — returns the LOCAL server URL as
        // resource_url so the provider's chat request also lands here.
        let token_guard = Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "test-access-token-from-mock",
                "refresh_token": "new-refresh-token",
                "expires_in": 3600,
                "resource_url": format!("{local_base}/v1")
            })))
            .expect(1)
            .mount_as_scoped(&mock_server)
            .await;

        // Chat completions endpoint — returns a deterministic success.
        let chat_guard = Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "mock-chatcmpl-001",
                "object": "chat.completion",
                "model": "qwen-test-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "mock-qwen-response"
                    },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
            })))
            .expect(1)
            .mount_as_scoped(&mock_server)
            .await;

        let mock_endpoint = format!("{local_base}/oauth2/token");

        // Point the Qwen OAuth refresh function at the controlled mock.
        // Guard ensures the override is removed even on panic.
        let _endpoint_guard = scopeguard::guard((), |_| {
            zeroclaw_providers::set_qwen_oauth_endpoint_for_test(None);
        });
        zeroclaw_providers::set_qwen_oauth_endpoint_for_test(Some(mock_endpoint));

        // ── 2. Config with Qwen alias carrying oauth_refresh_token ─
        let mut config = Config::default();
        let mut qwen_map = HashMap::new();
        qwen_map.insert(
            "test-alias".to_string(),
            QwenModelProviderConfig {
                base: ModelProviderConfig::default(),
                endpoint: Default::default(),
                auth_mode: None,
                oauth_refresh_token: Some("test-refresh-token".to_string()),
                oauth_client_id: None,
                oauth_resource_url: None,
            },
        );
        config.providers.models.qwen = qwen_map;

        // ── 3. Build LlmTaskTool targeting the Qwen OAuth alias ───
        let tool = LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(config),
            "qwen".to_string(),
            "test-alias".to_string(),
            "qwen-test-model".to_string(),
            Some(0.7),
            None,
            zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        );

        // ── 4. Execute on the async runtime ────────────────────────
        // Under the old code this would run `reqwest::blocking::Client`
        // directly on the Tokio worker — a prohibited blocking operation
        // that may panic.  Under the fix the construction runs on a
        // `spawn_blocking` worker and completes normally.
        //
        // Both OAuth and chat are served by the local Wiremock server.
        // The tool must succeed end-to-end: OAuth → provider → chat.
        let result = tool
            .execute(serde_json::json!({ "prompt": "test-prompt" }))
            .await
            .unwrap();

        // ── 5. Assert deterministic success ────────────────────────
        // Token exchange happened exactly once.
        assert_eq!(
            token_guard.received_requests().await.len(),
            1,
            "OAuth token endpoint must be called exactly once"
        );

        // Chat endpoint was reached — proves provider construction
        // succeeded and the LLM call was dispatched to the local mock.
        assert_eq!(
            chat_guard.received_requests().await.len(),
            1,
            "chat/completions endpoint must be called exactly once"
        );

        // Tool reports success with the exact mocked content.
        assert!(
            result.success,
            "execute must succeed end-to-end — got error: {:?}",
            result.error
        );
        assert!(
            result.error.is_none(),
            "no error expected on successful path — got: {:?}",
            result.error
        );
        let output = result.output.to_string();
        assert_eq!(
            output, "mock-qwen-response",
            "output must match the deterministic mock response"
        );
    }

    /// Controlled failure: the external token exchange returns an HTTP
    /// error.  The provider-construction error must surface the OAuth
    /// failure provenance — no panic, no swallowed provenance.
    // Holding `std::sync::Mutex` across `.await` is intentional here:
    // the lock serialises access to the process-global endpoint override
    // and is never contended from within the same task.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn llm_task_oauth_refresh_token_failure_surfaced_normally() {
        use std::collections::HashMap;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_config::schema::{Config, ModelProviderConfig, QwenModelProviderConfig};

        let _lock = OAUTH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let mock_server = MockServer::start().await;
        let mock_guard = Mock::given(method("POST"))
            .and(path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "Refresh token expired or revoked"
            })))
            .expect(1)
            .mount_as_scoped(&mock_server)
            .await;

        let mock_endpoint = format!("{}/oauth2/token", mock_server.uri());

        let _endpoint_guard = scopeguard::guard((), |_| {
            zeroclaw_providers::set_qwen_oauth_endpoint_for_test(None);
        });
        zeroclaw_providers::set_qwen_oauth_endpoint_for_test(Some(mock_endpoint));

        let mut config = Config::default();
        let mut qwen_map = HashMap::new();
        qwen_map.insert(
            "test-alias".to_string(),
            QwenModelProviderConfig {
                base: ModelProviderConfig::default(),
                endpoint: Default::default(),
                auth_mode: None,
                oauth_refresh_token: Some("expired-refresh-token".to_string()),
                oauth_client_id: None,
                oauth_resource_url: None,
            },
        );
        config.providers.models.qwen = qwen_map;

        let tool = LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(config),
            "qwen".to_string(),
            "test-alias".to_string(),
            "qwen-test-model".to_string(),
            Some(0.7),
            None,
            zeroclaw_providers::ModelProviderRuntimeOptions::default(),
        );

        let result = tool
            .execute(serde_json::json!({ "prompt": "test" }))
            .await
            .unwrap();

        // Verify the mock OAuth endpoint was actually hit — proves the
        // refresh path ran through the controlled 401 exchange.
        let requests = mock_guard.received_requests().await;
        assert_eq!(
            requests.len(),
            1,
            "mock OAuth endpoint must have been called exactly once"
        );

        assert!(!result.success, "must fail on expired token");
        let error = result.error.unwrap();
        // The error must surface the OAuth failure provenance, not
        // a misleading generic authentication error.  Require the
        // specific invalid_grant / OAuth / Refresh token signal.
        assert!(
            error.contains("invalid_grant")
                || error.contains("OAuth")
                || error.contains("Refresh token"),
            "error must surface OAuth failure provenance: {error}"
        );
        assert!(!error.contains("panicked"), "must not panic — got: {error}");
    }
}
