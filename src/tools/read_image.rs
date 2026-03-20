//! `read_image` tool — describe/analyze an image via a vision provider cascade.
//!
//! Cascade order (same priority as channel-level vision rerouting):
//! 1. Pre-resolved vision-capable provider (native or model_routes hint="vision")
//! 2. MCP vision fallback (`vision_mcp_fallback` config)
//! 3. Error — no vision capability available

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use async_trait::async_trait;
use serde_json::json;

use crate::config::MultimodalConfig;
use crate::providers::traits::{ChatMessage, ChatRequest, Provider};
use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};

/// Timeout for vision provider / MCP calls.
const VISION_TIMEOUT_SECS: u64 = 120;

pub struct ReadImageTool {
    security: Arc<SecurityPolicy>,
    /// Pre-resolved vision provider + model name from the cascade
    /// (native default or model_routes hint="vision").
    vision_provider: Option<(Arc<dyn Provider>, String)>,
    multimodal_config: MultimodalConfig,
    /// Shared tools registry — used for MCP vision fallback lookup.
    tools_registry: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
    http_client: reqwest::Client,
}

impl ReadImageTool {
    /// Build a `ReadImageTool`, pre-resolving the vision provider at startup.
    ///
    /// The cascade tries:
    /// 1. `default_provider` if it supports vision
    /// 2. First `model_routes` entry with `hint = "vision"` (created via provider factory)
    /// 3. Falls through to MCP at execution time when both are `None`
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        security: Arc<SecurityPolicy>,
        default_provider: Option<&Arc<dyn Provider>>,
        default_model: &str,
        model_routes: &[crate::config::ModelRouteConfig],
        fallback_api_key: Option<&str>,
        provider_runtime_options: &crate::providers::ProviderRuntimeOptions,
        multimodal_config: MultimodalConfig,
        tools_registry: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
    ) -> Self {
        let vision_provider = Self::resolve_vision_provider(
            default_provider,
            default_model,
            model_routes,
            fallback_api_key,
            provider_runtime_options,
        );

        if let Some((_, ref model)) = vision_provider {
            tracing::info!(model = %model, "read_image: vision provider resolved");
        } else {
            tracing::info!("read_image: no vision provider; will try MCP fallback at runtime");
        }

        Self {
            security,
            vision_provider,
            multimodal_config,
            tools_registry,
            http_client: crate::config::build_runtime_proxy_client_with_timeouts(
                "tool.read_image",
                60,
                10,
            ),
        }
    }

    fn resolve_vision_provider(
        default_provider: Option<&Arc<dyn Provider>>,
        default_model: &str,
        model_routes: &[crate::config::ModelRouteConfig],
        fallback_api_key: Option<&str>,
        provider_runtime_options: &crate::providers::ProviderRuntimeOptions,
    ) -> Option<(Arc<dyn Provider>, String)> {
        // Step 1: default provider
        if let Some(prov) = default_provider {
            if prov.supports_vision() {
                return Some((Arc::clone(prov), default_model.to_string()));
            }
        }

        // Step 2: model_routes hint="vision"
        let vision_route = model_routes
            .iter()
            .find(|r| r.hint.eq_ignore_ascii_case("vision"))?;

        let route_key = vision_route.api_key.as_deref().or(fallback_api_key);
        let provider = crate::providers::create_provider_with_options(
            &vision_route.provider,
            route_key,
            provider_runtime_options,
        )
        .ok()?;

        if provider.supports_vision() {
            Some((Arc::from(provider), vision_route.model.clone()))
        } else {
            tracing::warn!(
                provider = %vision_route.provider,
                "Vision route provider does not report vision support"
            );
            None
        }
    }

    /// Try to describe the image via the MCP vision fallback tool.
    async fn try_mcp_fallback(&self, image_source: &str, prompt: &str) -> Option<String> {
        let server_name = self.multimodal_config.vision_mcp_fallback.as_ref()?;
        let prefix = format!("{server_name}__");

        // Clone the Arc<dyn Tool> and drop the lock before any `.await`.
        let (vision_tool, image_param) = {
            let tools = self.tools_registry.read();
            let tool = tools
                .iter()
                .find(|t| {
                    t.name().strip_prefix(&prefix).is_some_and(|part| {
                        part.contains("vision")
                            || part.contains("describe")
                            || part.contains("image")
                    })
                })?
                .clone();

            let schema = tool.parameters_schema();
            let param = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .and_then(|props| {
                    if props.contains_key("image_source") {
                        Some("image_source")
                    } else if props.contains_key("image") {
                        Some("image")
                    } else {
                        props
                            .keys()
                            .find(|k| k.contains("image"))
                            .map(|k| k.as_str())
                    }
                })
                .unwrap_or("image_source")
                .to_string();

            (tool, param)
        }; // lock dropped here

        let args = json!({
            image_param: image_source,
            "prompt": prompt,
        });

        let timeout = Duration::from_secs(VISION_TIMEOUT_SECS);
        match tokio::time::timeout(timeout, vision_tool.execute(args)).await {
            Ok(Ok(result)) if result.success => Some(result.output),
            Ok(Ok(result)) => {
                tracing::warn!("read_image MCP fallback error: {:?}", result.error);
                None
            }
            Ok(Err(e)) => {
                tracing::warn!("read_image MCP fallback failed: {e}");
                None
            }
            Err(_) => {
                tracing::warn!("read_image MCP fallback timed out ({timeout:?})");
                None
            }
        }
    }
}

#[async_trait]
impl Tool for ReadImageTool {
    fn name(&self) -> &str {
        "read_image"
    }

    fn description(&self) -> &str {
        "Describe or analyze an image. Accepts a local file path, HTTP/HTTPS URL, \
         or base64 data URI. Returns a text description of the image content. \
         Use this tool when you encounter image references that need visual interpretation."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "image_source": {
                    "type": "string",
                    "description": "Image source: local file path, HTTP/HTTPS URL, or data URI (data:image/...;base64,...)"
                },
                "prompt": {
                    "type": "string",
                    "description": "What to look for or describe in the image. Defaults to a general description."
                }
            },
            "required": ["image_source"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let image_source = match args.get("image_source").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing or empty 'image_source' parameter.".into()),
                });
            }
        };

        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("Describe this image in detail.");

        // Security check for local file paths.
        let is_local = !image_source.starts_with("data:")
            && !image_source.starts_with("http://")
            && !image_source.starts_with("https://");

        if is_local {
            if !self.security.is_path_allowed(image_source) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Path not allowed by security policy: {image_source}"
                    )),
                });
            }
            if !Path::new(image_source).exists() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("File not found: {image_source}")),
                });
            }
        }

        // ── Cascade step 1: Vision provider ──────────────────────────
        if let Some((ref provider, ref model)) = self.vision_provider {
            let (_, max_image_size_mb) = self.multimodal_config.effective_limits();
            let max_bytes = max_image_size_mb.saturating_mul(1024 * 1024);

            match crate::multimodal::normalize_image_reference(
                image_source,
                &self.multimodal_config,
                max_bytes,
                &self.http_client,
            )
            .await
            {
                Ok(data_uri) => {
                    let message = ChatMessage::user(format!("[IMAGE:{data_uri}]\n\n{prompt}"));
                    let request = ChatRequest {
                        messages: &[message],
                        tools: None,
                    };
                    let timeout = Duration::from_secs(VISION_TIMEOUT_SECS);
                    match tokio::time::timeout(timeout, provider.chat(request, model, 0.3)).await {
                        Ok(Ok(response)) => {
                            let text = response.text_or_empty().to_string();
                            if !text.is_empty() {
                                return Ok(ToolResult {
                                    success: true,
                                    output: text,
                                    error: None,
                                });
                            }
                            tracing::warn!("read_image: vision provider returned empty response");
                        }
                        Ok(Err(e)) => {
                            tracing::warn!("read_image: vision provider failed: {e}");
                        }
                        Err(_) => {
                            tracing::warn!("read_image: vision provider timed out ({timeout:?})");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("read_image: image normalization failed: {e}");
                }
            }
        }

        // ── Cascade step 2: MCP vision fallback ──────────────────────
        if let Some(description) = self.try_mcp_fallback(image_source, prompt).await {
            return Ok(ToolResult {
                success: true,
                output: description,
                error: None,
            });
        }

        // ── Cascade step 3: All fallbacks exhausted ──────────────────
        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(
                "No vision capability available. Configure a vision-capable provider, \
                 add a [[model_routes]] entry with hint=\"vision\", or set \
                 vision_mcp_fallback in [multimodal]."
                    .into(),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;
    use parking_lot::RwLock;
    use std::sync::Arc;

    fn test_tool() -> ReadImageTool {
        ReadImageTool {
            security: Arc::new(SecurityPolicy::default()),
            vision_provider: None,
            multimodal_config: MultimodalConfig::default(),
            tools_registry: Arc::new(RwLock::new(Vec::new())),
            http_client: reqwest::Client::new(),
        }
    }

    #[test]
    fn name_and_description() {
        let tool = test_tool();
        assert_eq!(tool.name(), "read_image");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_has_required_image_source() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "image_source"));
        assert!(schema["properties"]["image_source"].is_object());
        assert!(schema["properties"]["prompt"].is_object());
    }

    #[tokio::test]
    async fn missing_image_source_returns_error() {
        let tool = test_tool();
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("image_source"));
    }

    #[tokio::test]
    async fn empty_image_source_returns_error() {
        let tool = test_tool();
        let result = tool.execute(json!({"image_source": ""})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("image_source"));
    }

    #[tokio::test]
    async fn nonexistent_file_returns_error() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"image_source": "/nonexistent/path/image.png"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("File not found") || err.contains("not allowed"),
            "expected file or security error, got: {err}"
        );
    }

    #[tokio::test]
    async fn all_fallbacks_exhausted_returns_error() {
        let tool = test_tool();
        // Use a data URI to skip file-existence check but no provider available.
        let result = tool
            .execute(json!({
                "image_source": "data:image/png;base64,iVBORw0KGgo="
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("No vision capability"));
    }
}
