// Image Generation Tool for ZeroClaw
// Provides AI-powered image generation capabilities via configured providers

use async_trait::async_trait;
use super::traits::{Tool, ToolResult};
use crate::config::schema::MultimodalGenerationConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    workspace_dir: PathBuf,
}

impl ImageGenerationTool {
    /// Create a new image generation tool instance
    pub fn new(config: MultimodalGenerationConfig, workspace_dir: PathBuf) -> Self {
        Self { config, workspace_dir }
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
                "contents": [
                    {
                        "parts": [
                            {
                                "text": params.prompt
                            }
                        ]
                    }
                ]
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

    /// Get API key for image generation
    fn get_api_key(&self, provider: &str) -> Option<String> {
        // Priority: config.api_key > ZEROCLAW_API_KEY
        if let Some(ref key) = self.config.api_key {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }

        if let Ok(key) = std::env::var("ZEROCLAW_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }

        // Provider specific environment variables
        match provider {
            "openai" => std::env::var("OPENAI_API_KEY").ok(),
            "gemini" => std::env::var("GEMINI_API_KEY")
                .ok()
                .or_else(|| std::env::var("GOOGLE_API_KEY").ok()),
            "anthropic" | "claude" => std::env::var("ANTHROPIC_API_KEY").ok(),
            _ => None,
        }
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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
            .get_api_key(provider)
            .ok_or_else(|| anyhow::anyhow!("No API key configured for image generation (provider: {})", provider))?;

        // Build request URL based on provider
        let url = match provider {
            "openai" => "https://api.openai.com/v1/images/generations".to_string(),
            "anthropic" => "https://api.anthropic.com/v1/images/generations".to_string(),
            "gemini" => format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                model, api_key
            ),
            _ => return Err(anyhow::anyhow!("Unsupported image generation provider: {}", provider)),
        };

        // Make the HTTP request
        let client = reqwest::Client::new();
        let mut request = client.post(url);

        // Add authentication and headers
        match provider {
            "gemini" => {
                // Gemini uses API key in URL query param, already added above
            }
            _ => {
                request = request.header("Authorization", format!("Bearer {}", api_key));
            }
        }

        request = request.header("Content-Type", "application/json");

        // Provider-specific headers
        match provider {
            "anthropic" => {
                request = request.header("anthropic-version", "2023-06-01");
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
                        let _revised = item
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
            "gemini" => {
                // Gemini generateContent returns images in candidates[0].content.parts[i].inlineData
                let candidates = response
                    .get("candidates")
                    .and_then(|c| c.as_array())
                    .ok_or_else(|| anyhow::anyhow!("Invalid Gemini response: missing candidates. Body: {}", response))?;

                let candidate = candidates.first().ok_or_else(|| anyhow::anyhow!("Gemini response: empty candidates"))?;
                let parts = candidate
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                    .ok_or_else(|| anyhow::anyhow!("Gemini response: missing parts"))?;

                let mut images = Vec::new();
                for part in parts {
                    if let Some(inline_data) = part.get("inlineData") {
                        let mime_type = inline_data.get("mimeType").and_then(|m| m.as_str()).unwrap_or("image/png");
                        let data = inline_data.get("data").and_then(|d| d.as_str()).ok_or_else(|| anyhow::anyhow!("Gemini part missing image data"))?;
                        
                        images.push(ImageData {
                            url: None,
                            b64_json: Some(data.to_string()),
                            format: mime_type.split('/').last().unwrap_or("png").to_string(),
                            seed: None,
                        });
                    }
                }

                if images.is_empty() {
                    anyhow::bail!("Gemini response contained no images in parts");
                }

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
            Ok(response) => {
                let mut output = format!(
                    "Successfully generated {} image(s) using {} (model: {}).

",
                    response.images.len(),
                    response.provider,
                    response.model
                );

                // Create images directory if it doesn't exist
                let images_dir = self.workspace_dir.join("generated_images");
                std::fs::create_dir_all(&images_dir).ok();

                for (i, img) in response.images.iter().enumerate() {
                    if let Some(ref b64_data) = img.b64_json {
                        // Decode base64 from provider response to binary bytes
                        use base64::Engine;
                        let image_bytes = base64::engine::general_purpose::STANDARD
                            .decode(b64_data)
                            .map_err(|e| anyhow::anyhow!("Failed to decode image data: {}", e))?;
                        
                        // Generate unique filename
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis();
                        let filename = format!("image_{}_{}.{}", timestamp, i, img.format);
                        let file_path = images_dir.join(&filename);
                        
                        // Write binary bytes to file
                        std::fs::write(&file_path, &image_bytes)
                            .map_err(|e| anyhow::anyhow!("Failed to write image file: {}", e))?;
                        
                        // Return file path as [IMAGE:/path/to/file]
                        output.push_str(&format!("[IMAGE:{}]
", file_path.display()));
                    } else if let Some(ref url) = img.url {
                        output.push_str(&format!("[IMAGE:{}]
", url));
                    }
                    if let Some(ref revised) = response.revised_prompt {
                        output.push_str(&format!("Revised prompt {}: {}
", i + 1, revised));
                    }
                }

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
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
        let tool = ImageGenerationTool::new(MultimodalGenerationConfig::default(), std::env::temp_dir());
        
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
