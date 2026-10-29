//! Lightweight LLM task tool for structured JSON-only sub-calls.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_config::schema::Config;
use zeroclaw_providers::ProviderDispatch;

/// Reads the configuration generation admitted by the owning session. The
/// resolver borrows the owner's authority; tools do not maintain another store.
pub type ModelConfigResolver = Arc<dyn Fn() -> Arc<Config> + Send + Sync>;

tokio::task_local! {
    static ADMITTED_MODEL_CONFIG: Arc<AdmittedModelConfig>;
}

struct AdmittedModelConfig {
    agent_alias: String,
    config: Arc<Config>,
    parent: Option<Arc<AdmittedModelConfig>>,
}

/// Bind model tools to the same configuration generation as their owning turn.
/// Nested turns establish their own binding; unrelated agents use their owner.
pub fn scope_model_config<F: std::future::Future>(
    config: Arc<Config>,
    agent_alias: impl Into<String>,
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    // Keep task-local wrappers from embedding the complete caller state
    // machine again at each nested delegation hop.
    let future = Box::pin(future);
    let agent_alias = agent_alias.into();
    async move {
        let parent = ADMITTED_MODEL_CONFIG.try_with(Arc::clone).ok();
        ADMITTED_MODEL_CONFIG
            .scope(
                Arc::new(AdmittedModelConfig {
                    agent_alias,
                    config,
                    parent,
                }),
                future,
            )
            .await
    }
}

/// Capture before spawning so pipeline workers and bounded delegates retain
/// the admission of the registry owner whose tools they borrow.
/// Evaluate this function in the parent task before passing its future to a
/// spawn macro, which may evaluate its body only in the child task.
pub fn inherit_model_config_scope<F: std::future::Future>(
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    let admitted = ADMITTED_MODEL_CONFIG.try_with(Arc::clone).ok();
    let future = Box::pin(future);
    async move {
        match admitted {
            Some(admitted) => ADMITTED_MODEL_CONFIG.scope(admitted, future).await,
            None => future.await,
        }
    }
}

/// Prefer the current turn's admission over a long-lived registry's fallback.
pub fn model_config_for_agent(
    fallback: ModelConfigResolver,
    agent_alias: impl Into<String>,
) -> ModelConfigResolver {
    let agent_alias = agent_alias.into();
    Arc::new(move || {
        ADMITTED_MODEL_CONFIG
            .try_with(|admitted| {
                let mut current = Some(admitted.as_ref());
                while let Some(admitted) = current {
                    if admitted.agent_alias == agent_alias {
                        return Some(Arc::clone(&admitted.config));
                    }
                    current = admitted.parent.as_deref();
                }
                None
            })
            .ok()
            .flatten()
            .unwrap_or_else(|| fallback())
    })
}

/// Tool that runs a single prompt through an LLM and optionally validates
/// the response against a JSON Schema. No tools are provided to the LLM —
/// this is a pure text-in, text-out (or JSON-out) call.
pub struct LlmTaskTool {
    security: Arc<SecurityPolicy>,
    config: ModelConfigResolver,
    agent_alias: String,
}

impl LlmTaskTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        config: Arc<Config>,
        agent_alias: impl Into<String>,
    ) -> Self {
        Self::with_config_resolver(security, Arc::new(move || Arc::clone(&config)), agent_alias)
    }

    pub fn with_config_resolver(
        security: Arc<SecurityPolicy>,
        config: ModelConfigResolver,
        agent_alias: impl Into<String>,
    ) -> Self {
        Self {
            security,
            config,
            agent_alias: agent_alias.into(),
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
        let model_override = args
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let temperature_override = args.get("temperature").and_then(|v| v.as_f64());

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

        let config = (self.config)();
        let agent_alias = self.agent_alias.clone();
        let built = zeroclaw_providers::model::build_nonblocking(move || {
            let reference = &config
                .agents
                .get(&agent_alias)
                .ok_or_else(|| {
                    anyhow::Error::msg(crate::i18n::get_required_tool_string_with_args(
                        "tool-llm-task-agent-missing",
                        &[("agent", agent_alias.as_str())],
                    ))
                })?
                .model_provider;
            let selection = config.resolve_model_selection(reference).ok_or_else(|| {
                anyhow::Error::msg(crate::i18n::get_required_tool_string_with_args(
                    "tool-llm-task-provider-invalid",
                    &[("provider", reference.as_str())],
                ))
            })?;
            let temperature = temperature_override.or_else(|| selection.temperature());
            let resolved = zeroclaw_providers::model::build_raw_provider(
                &config,
                reference,
                zeroclaw_providers::model::ProviderOverrides {
                    model: model_override.as_deref(),
                    ..Default::default()
                },
            )?;
            let model = resolved.model.ok_or_else(|| {
                anyhow::Error::msg(crate::i18n::get_required_tool_string_with_args(
                    "tool-llm-task-model-missing",
                    &[("provider", reference.as_str())],
                ))
            })?;
            Ok((resolved.provider, model, temperature))
        })
        .await;
        let (model_provider, model, temperature) = match built {
            Ok(built) => built,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(json!({"reason": "provider_construction_failed"})),
                    "llm_task: failed to construct selected provider"
                );
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
            .simple_chat(&effective_prompt, &model, temperature)
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

    #[tokio::test]
    async fn model_config_scope_isolates_concurrent_and_nested_turns() {
        let fallback = Arc::new(Config::default());
        let fallback_resolver: ModelConfigResolver = {
            let fallback = Arc::clone(&fallback);
            Arc::new(move || Arc::clone(&fallback))
        };
        let parent = model_config_for_agent(Arc::clone(&fallback_resolver), "parent");
        let child = model_config_for_agent(fallback_resolver, "child");
        let first = Arc::new(Config::default());
        let second = Arc::new(Config::default());
        let nested = Arc::new(Config::default());

        tokio::join!(
            scope_model_config(Arc::clone(&first), "parent", async {
                tokio::task::yield_now().await;
                assert!(Arc::ptr_eq(&parent(), &first));
                assert!(Arc::ptr_eq(&child(), &fallback));
                scope_model_config(Arc::clone(&nested), "child", async {
                    assert!(Arc::ptr_eq(&child(), &nested));
                    assert!(Arc::ptr_eq(&parent(), &first));
                    let parent = Arc::clone(&parent);
                    let child = Arc::clone(&child);
                    let inherited = inherit_model_config_scope(async move { (parent(), child()) });
                    let (inherited_parent, inherited_child) =
                        zeroclaw_spawn::spawn!(inherited).await.unwrap();
                    assert!(Arc::ptr_eq(&inherited_parent, &first));
                    assert!(Arc::ptr_eq(&inherited_child, &nested));
                })
                .await;
                assert!(Arc::ptr_eq(&parent(), &first));
            }),
            scope_model_config(Arc::clone(&second), "parent", async {
                tokio::task::yield_now().await;
                assert!(Arc::ptr_eq(&parent(), &second));
            }),
        );
        assert!(Arc::ptr_eq(&parent(), &fallback));
    }

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
            "test",
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
        let mut config = Config::default();
        config.agents.insert(
            "test".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "nonexistent_provider_xyz.default".into(),
                ..Default::default()
            },
        );
        let tool = LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(config),
            "test",
        );

        let result = tool
            .execute(json!({"prompt": "Hello world"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("model_provider"));
    }

    #[tokio::test]
    async fn execution_uses_admitted_generation_for_model_options_and_credentials() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_config::schema::{AliasedAgentConfig, ModelEntryConfig};

        let server = MockServer::start().await;
        let mut config = Config::default();
        let profile = config.providers.models.ensure("custom", "local").unwrap();
        profile.uri = Some(format!("{}/v1/chat/completions", server.uri()));
        profile.api_key = Some("old-test-key".into());
        profile.model = Some("legacy-model".into());
        profile.models.insert(
            "default".into(),
            ModelEntryConfig {
                id: Some("nested-old".into()),
                temperature: Some(0.2),
                ..Default::default()
            },
        );
        config.agents.insert(
            "test".into(),
            AliasedAgentConfig {
                model_provider: "custom.local".into(),
                ..Default::default()
            },
        );
        let generation = Arc::new(std::sync::RwLock::new(Arc::new(config)));
        let source = Arc::clone(&generation);
        let tool = LlmTaskTool::with_config_resolver(
            Arc::new(SecurityPolicy::default()),
            Arc::new(move || Arc::clone(&source.read().unwrap())),
            "test",
        );
        let mut next = (**generation.read().unwrap()).clone();
        let profile = next.providers.models.ensure("custom", "local").unwrap();
        profile.api_key = Some("next-test-key".into());
        let entry = profile.models.get_mut("default").unwrap();
        entry.id = Some("nested-next".into());
        entry.temperature = Some(0.6);
        *generation.write().unwrap() = Arc::new(next);

        for (args, model, temperature) in [
            (json!({"prompt": "test"}), "nested-next", 0.6),
            (
                json!({"prompt": "test", "model": "explicit", "temperature": 0.0}),
                "explicit",
                0.0,
            ),
        ] {
            let guard = Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .and(header("authorization", "Bearer next-test-key"))
                .and(body_partial_json(json!({"model": model, "temperature": temperature})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]
                })))
                .expect(1)
                .mount_as_scoped(&server).await;
            let result = tool.execute(args).await.unwrap();
            assert!(result.success, "{:?}", result.error);
            assert_eq!(guard.received_requests().await.len(), 1);
        }
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

        for endpoint_source in ["refreshed_resource", "profile_uri", "oauth_resource"] {
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
                    "resource_url": if endpoint_source == "refreshed_resource" {
                        format!("{local_base}/v1")
                    } else {
                        format!("{local_base}/ignored/v1")
                    }
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
                    base: ModelProviderConfig {
                        uri: (endpoint_source == "profile_uri").then(|| format!("{local_base}/v1")),
                        ..Default::default()
                    },
                    endpoint: Default::default(),
                    auth_mode: None,
                    oauth_refresh_token: Some("test-refresh-token".to_string()),
                    oauth_client_id: None,
                    oauth_resource_url: (endpoint_source == "oauth_resource")
                        .then(|| format!("{local_base}/v1")),
                },
            );
            config.providers.models.qwen = qwen_map;
            config
                .providers
                .models
                .ensure("qwen", "test-alias")
                .unwrap()
                .model = Some("qwen-test-model".into());
            config.agents.insert(
                "test".into(),
                zeroclaw_config::schema::AliasedAgentConfig {
                    model_provider: "qwen.test-alias".into(),
                    ..Default::default()
                },
            );

            // ── 3. Build LlmTaskTool targeting the Qwen OAuth alias ───
            let tool = LlmTaskTool::new(
                Arc::new(SecurityPolicy::default()),
                Arc::new(config),
                "test",
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
                "chat/completions must use {endpoint_source}; tool error: {:?}",
                result.error
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
        config
            .providers
            .models
            .ensure("qwen", "test-alias")
            .unwrap()
            .model = Some("qwen-test-model".into());
        config.agents.insert(
            "test".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "qwen.test-alias".into(),
                ..Default::default()
            },
        );

        let tool = LlmTaskTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(config),
            "test",
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
