// Image Generation Tool for ZeroClaw
// Provides AI-powered image generation capabilities via configured providers

use async_trait::async_trait;
use super::traits::{Tool, ToolResult};
use crate::config::schema::MultimodalGenerationConfig;
use serde::{Deserialize, Serialize};

/// Image generation tool name
pub const TOOL_NAME: &str = "image_generation";

/// Image generation tool description
pub const TOOL_DESCRIPTION: &str = "Generate images from text descriptions using AI providers";

/// Parameters for image generation tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationParams {
    /// The text description of the image to generate
    pub prompt: String,

    /// Number of images to generate (1-4)
    #[serde(default = "default_n")]
    pub n: usize,

    /// Image size (e.g., "1024x1024", "1792x1024", "1024x1792")
    #[serde(default)]
    pub size: Option<String>,

    /// Quality of the image ("standard", "hd")
    #[serde(default)]
    pub quality: Option<String>,

    /// Style of the image ("natural", "vivid", "anime")
    #[serde(default)]
    pub style: Option<String>,

    /// Model to use for generation (overrides config default)
    #[serde(default)]
    pub model: Option<String>,

    /// Provider to use (overrides config default)
    #[serde(default)]
    pub provider: Option<String>,
}

fn default_n() -> usize {
    1
}

/// Image generation response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationResponse {
    /// Array of generated image URLs or base64 data
    pub images: Vec<ImageData>,
    /// Provider used for generation
    pub provider: String,
    /// Model used for generation
    pub model: String,
    /// Optional revised prompt if provider supports it
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

/// Individual image data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageData {
    /// Image URL (if provider returns URL) or base64 data
    pub url: Option<String>,
    /// Base64 encoded image data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    /// Image format (png, jpeg, etc.)
    #[serde(default)]
    pub format: String,
    /// Optional seed used for generation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

/// Image Generation Tool
pub struct ImageGenerationTool {
    config: MultimodalGenerationConfig,
}

impl ImageGenerationTool {
    /// Create a new image generation tool instance
    pub fn new(config: MultimodalGenerationConfig) -> Self {
        Self { config }
    }

    /// Generate images using the configured or specified provider
    pub async fn generate(
        &self,
        params: ImageGenerationParams,
    ) -> anyhow::Result<ImageGenerationResponse> {
        // Determine provider and model to use
        let provider_name = params
            .provider
            .as_deref()
            .or(self.config.default_image_provider.as_deref())
            .unwrap_or("openai");

        let model = params
            .model
            .as_deref()
            .or(self.config.default_image_model.as_deref())
            .unwrap_or("dall-e-3");

        // Build the request payload based on provider
        let payload = self.build_provider_payload(provider_name, model, &params)?;

        // Call the provider (this would integrate with the existing provider system)
        let response = self.call_provider(provider_name, model, payload).await?;

        Ok(response)
    }

    /// Build provider-specific payload
    fn build_provider_payload(
        &self,
        provider: &str,
        model: &str,
        params: &ImageGenerationParams,
    ) -> anyhow::Result<serde_json::Value> {
        let size = params
            .size
            .as_deref()
            .or(self.config.default_image_size.as_deref())
            .unwrap_or("1024x1024");

        let quality = params.quality.as_deref().unwrap_or("standard");
        let style = params.style.as_deref().unwrap_or("natural");
        let n = params.n.clamp(1, 4);

        match provider {
            "openai" => Ok(serde_json::json!({
                "model": model,
                "prompt": params.prompt,
                "n": n,
                "size": size,
                "quality": quality,
                "style": style,
                "response_format": "url"
            })),
            "anthropic" | "claude" => Ok(serde_json::json!({
                "model": model,
                "prompt": params.prompt,
                "number_of_images": n,
                "size": size
            })),
            "gemini" => Ok(serde_json::json!({
                "model": model,
                "prompt": params.prompt,
                "number_of_images": n,
                "aspect_ratio": self.size_to_aspect_ratio(size)
            })),
            _ => Ok(serde_json::json!({
                "model": model,
                "prompt": params.prompt,
                "num_images": n,
                "size": size
            })),
        }
    }

    /// Convert size string to aspect ratio for providers that use it
    fn size_to_aspect_ratio(&self, size: &str) -> String {
        match size {
            "1024x1024" => "1:1".to_string(),
            "1792x1024" | "1024x1792" => "16:9".to_string(),
            "1024x1536" | "1536x1024" => "2:3".to_string(),
            _ => "1:1".to_string(),
        }
    }

    /// Call the provider API
    async fn call_provider(
        &self,
        provider: &str,
        model: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<ImageGenerationResponse> {
        // Get API key from config or environment
        let api_key = self
            .config
            .api_key
            .clone()
            .or_else(|| std::env::var("ZEROCLAW_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());

        if api_key.is_none() {
            anyhow::bail!("No API key configured for image generation");
        }

        // Build request URL based on provider
        let url = match provider {
            "openai" => "https://api.openai.com/v1/images/generations".to_string(),
            "anthropic" => "https://api.anthropic.com/v1/images/generations".to_string(),
            "gemini" => format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:predict",
                model
            ),
            _ => return Err(anyhow::anyhow!("Unsupported image generation provider: {}", provider)),
        };

        // Make the HTTP request
        let client = reqwest::Client::new();
        let mut request = client.post(url);

        // Add authentication and headers
        request = request.header("Authorization", format!("Bearer {}", api_key.unwrap()));
        request = request.header("Content-Type", "application/json");

        // Provider-specific headers
        match provider {
            "anthropic" => {
                request = request.header("anthropic-version", "2023-06-01");
            }
            "gemini" => {
                // Gemini uses API key in URL query param
            }
            _ => {}
        }

        // Send request
        let response = request
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to call image generation API: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Image generation API error ({}): {}",
                status,
                error_body
            );
        }

        // Parse response
        let response_json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse image generation response: {}", e))?;

        // Parse provider-specific response format
        let images = self.parse_provider_response(provider, response_json)?;

        Ok(ImageGenerationResponse {
            images,
            provider: provider.to_string(),
            model: model.to_string(),
            revised_prompt: None,
        })
    }

    /// Parse provider-specific response into ImageData array
    fn parse_provider_response(
        &self,
        provider: &str,
        response: serde_json::Value,
    ) -> anyhow::Result<Vec<ImageData>> {
        match provider {
            "openai" => {
                let data = response
                    .get("data")
                    .and_then(|d| d.as_array())
                    .ok_or_else(|| anyhow::anyhow!("Invalid OpenAI response format"))?;

                let images: Vec<ImageData> = data
                    .iter()
                    .map(|item| {
                        let url = item
                            .get("url")
                            .and_then(|u| u.as_str())
                            .map(|s| s.to_string());
                        let b64 = item
                            .get("b64_json")
                            .and_then(|b| b.as_str())
                            .map(|s| s.to_string());
                        let revised = item
                            .get("revised_prompt")
                            .and_then(|r| r.as_str())
                            .map(|s| s.to_string());

                        ImageData {
                            url,
                            b64_json: b64,
                            format: "png".to_string(),
                            seed: None,
                        }
                    })
                    .collect();

                Ok(images)
            }
            _ => {
                // Generic parsing - try common patterns
                let data = response
                    .get("images")
                    .or_else(|| response.get("data"))
                    .and_then(|d| d.as_array())
                    .ok_or_else(|| anyhow::anyhow!("Invalid response format for provider {}", provider))?;

                let images: Vec<ImageData> = data
                    .iter()
                    .map(|item| {
                        let url = item
                            .get("url")
                            .and_then(|u| u.as_str())
                            .map(|s| s.to_string());
                        let b64 = item
                            .get("base64")
                            .or_else(|| item.get("b64_json"))
                            .and_then(|b| b.as_str())
                            .map(|s| s.to_string());

                        ImageData {
                            url,
                            b64_json: b64,
                            format: "png".to_string(),
                            seed: item.get("seed").and_then(|s| s.as_u64()),
                        }
                    })
                    .collect();

                Ok(images)
            }
        }
    }
}

#[async_trait]
impl Tool for ImageGenerationTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The text description of the image to generate"
                },
                "n": {
                    "type": "integer",
                    "description": "Number of images to generate (1-4)",
                    "default": 1
                },
                "size": {
                    "type": "string",
                    "description": "Image size (e.g., 1024x1024, 1792x1024)",
                    "default": "1024x1024"
                },
                "quality": {
                    "type": "string",
                    "description": "Quality of the image (standard, hd)",
                    "default": "standard"
                },
                "style": {
                    "type": "string",
                    "description": "Style of the image (natural, vivid, anime)",
                    "default": "natural"
                },
                "model": {
                    "type": "string",
                    "description": "Model to use for generation"
                },
                "provider": {
                    "type": "string",
                    "description": "Provider to use (openai, anthropic, gemini)"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let params: ImageGenerationParams = serde_json::from_value(args)?;

        // Check if generation is enabled in config
        if !self.config.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Image generation is disabled. Enable it in config.toml: [multimodal.generation]".to_string()),
            });
        }

        match self.generate(params).await {
            Ok(response) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string(&response).unwrap_or_default(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_n() {
        assert_eq!(default_n(), 1);
    }

    #[test]
    fn test_size_to_aspect_ratio() {
        let tool = ImageGenerationTool::new(MultimodalGenerationConfig::default());
        
        assert_eq!(tool.size_to_aspect_ratio("1024x1024"), "1:1");
        assert_eq!(tool.size_to_aspect_ratio("1792x1024"), "16:9");
        assert_eq!(tool.size_to_aspect_ratio("1024x1792"), "16:9");
        assert_eq!(tool.size_to_aspect_ratio("1024x1536"), "2:3");
        assert_eq!(tool.size_to_aspect_ratio("1536x1024"), "2:3");
    }

    #[test]
    fn test_image_generation_params_serde() {
        let params = ImageGenerationParams {
            prompt: "A sunset over the ocean".to_string(),
            n: 2,
            size: Some("1024x1024".to_string()),
            quality: Some("standard".to_string()),
            style: Some("natural".to_string()),
            model: Some("dall-e-3".to_string()),
            provider: Some("openai".to_string()),
        };

        let json = serde_json::to_string(&params).unwrap();
        let parsed: ImageGenerationParams = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.prompt, "A sunset over the ocean");
        assert_eq!(parsed.n, 2);
        assert_eq!(parsed.size, Some("1024x1024".to_string()));
    }
}
