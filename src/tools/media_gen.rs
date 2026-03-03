//! Image, video, and music generation tool integrations.
//!
//! Provides structured tools for AI-powered media generation:
//! - **Image generation** via OpenAI DALL-E or compatible APIs
//! - **Video generation** via Runway or compatible APIs
//! - **Music generation** via Suno or compatible APIs
//!
//! Each tool follows the [`Tool`] trait pattern and returns structured
//! results with download URLs and metadata.

use async_trait::async_trait;
use serde_json::json;

use super::traits::{Tool, ToolResult};

// ═══════════════════════════════════════════════════════════════════
// Image Generation Tool
// ═══════════════════════════════════════════════════════════════════

/// Image generation tool using DALL-E or compatible APIs.
///
/// Supports text-to-image generation with configurable size and quality.
pub struct ImageGenTool {
    api_key: String,
    api_url: String,
    model: String,
}

impl ImageGenTool {
    /// Create a new image generation tool.
    ///
    /// Defaults to OpenAI DALL-E 3 API. Override `api_url` for compatible
    /// alternatives.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.openai.com/v1/images/generations".into(),
            model: "dall-e-3".into(),
        }
    }

    /// Create with a custom API endpoint and model.
    pub fn with_config(api_key: String, api_url: String, model: String) -> Self {
        Self {
            api_key,
            api_url,
            model,
        }
    }
}

#[async_trait]
impl Tool for ImageGenTool {
    fn name(&self) -> &str {
        "image_generate"
    }

    fn description(&self) -> &str {
        "Generate an image from a text description using AI. \
         Returns a URL to the generated image."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text description of the image to generate"
                },
                "size": {
                    "type": "string",
                    "enum": ["1024x1024", "1024x1792", "1792x1024"],
                    "default": "1024x1024",
                    "description": "Image dimensions"
                },
                "quality": {
                    "type": "string",
                    "enum": ["standard", "hd"],
                    "default": "standard",
                    "description": "Image quality level"
                },
                "style": {
                    "type": "string",
                    "enum": ["natural", "vivid"],
                    "default": "natural",
                    "description": "Image style"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: prompt"))?;

        let size = args
            .get("size")
            .and_then(|v| v.as_str())
            .unwrap_or("1024x1024");
        let quality = args
            .get("quality")
            .and_then(|v| v.as_str())
            .unwrap_or("standard");
        let style = args
            .get("style")
            .and_then(|v| v.as_str())
            .unwrap_or("natural");

        let client = crate::config::build_runtime_proxy_client("tool.image_gen");
        let body = json!({
            "model": self.model,
            "prompt": prompt,
            "n": 1,
            "size": size,
            "quality": quality,
            "style": style,
            "response_format": "url"
        });

        let resp = client
            .post(&self.api_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Image generation API error {status}: {body}")),
            });
        }

        let result: serde_json::Value = resp.json().await?;
        let image_url = result
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("url"))
            .and_then(|u| u.as_str())
            .unwrap_or("");
        let revised_prompt = result
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("revised_prompt"))
            .and_then(|p| p.as_str())
            .unwrap_or("");

        let output = format!(
            "Image generated successfully.\n\
             URL: {image_url}\n\
             Size: {size}\n\
             Quality: {quality}\n\
             Revised prompt: {revised_prompt}"
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// Video Generation Tool
// ═══════════════════════════════════════════════════════════════════

/// Video generation tool using Runway or compatible APIs.
///
/// Supports text-to-video and image-to-video generation.
pub struct VideoGenTool {
    api_key: String,
    api_url: String,
}

impl VideoGenTool {
    /// Create a new video generation tool.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.dev.runwayml.com/v1/image_to_video".into(),
        }
    }

    /// Create with a custom API endpoint.
    pub fn with_api_url(api_key: String, api_url: String) -> Self {
        Self { api_key, api_url }
    }
}

#[async_trait]
impl Tool for VideoGenTool {
    fn name(&self) -> &str {
        "video_generate"
    }

    fn description(&self) -> &str {
        "Generate a short video from a text description or source image. \
         Returns a task ID to poll for the result."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text description of the video to generate"
                },
                "image_url": {
                    "type": "string",
                    "description": "Optional source image URL for image-to-video"
                },
                "duration_secs": {
                    "type": "integer",
                    "enum": [5, 10],
                    "default": 5,
                    "description": "Video duration in seconds"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: prompt"))?;

        let image_url = args.get("image_url").and_then(|v| v.as_str());
        let duration = args
            .get("duration_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(5);

        let client = crate::config::build_runtime_proxy_client("tool.video_gen");

        let mut body = json!({
            "promptText": prompt,
            "duration": duration,
            "model": "gen4_turbo"
        });

        if let Some(img_url) = image_url {
            body["promptImage"] = json!(img_url);
        }

        let resp = client
            .post(&self.api_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("X-Runway-Version", "2024-11-06")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Video generation API error {status}: {body}")),
            });
        }

        let result: serde_json::Value = resp.json().await?;
        let task_id = result.get("id").and_then(|v| v.as_str()).unwrap_or("");

        let output = format!(
            "Video generation task submitted.\n\
             Task ID: {task_id}\n\
             Duration: {duration}s\n\
             Status: Processing — poll the task status endpoint to get the result URL."
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// Music Generation Tool
// ═══════════════════════════════════════════════════════════════════

/// Music generation tool using Suno or compatible APIs.
///
/// Supports text-to-music generation with style and duration controls.
pub struct MusicGenTool {
    api_key: String,
    api_url: String,
}

impl MusicGenTool {
    /// Create a new music generation tool.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://apibox.erweima.ai/api/v1/generate".into(),
        }
    }

    /// Create with a custom API endpoint.
    pub fn with_api_url(api_key: String, api_url: String) -> Self {
        Self { api_key, api_url }
    }
}

#[async_trait]
impl Tool for MusicGenTool {
    fn name(&self) -> &str {
        "music_generate"
    }

    fn description(&self) -> &str {
        "Generate music from a text description. Returns a task ID \
         to poll for the audio download URL."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text description of the music to generate (genre, mood, instruments)"
                },
                "title": {
                    "type": "string",
                    "description": "Title for the generated music"
                },
                "style": {
                    "type": "string",
                    "description": "Music style tags (e.g. 'jazz piano, chill, ambient')"
                },
                "instrumental": {
                    "type": "boolean",
                    "default": true,
                    "description": "Whether to generate instrumental-only (no vocals)"
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: prompt"))?;

        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled");
        let style = args.get("style").and_then(|v| v.as_str()).unwrap_or("");
        let instrumental = args
            .get("instrumental")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let client = crate::config::build_runtime_proxy_client("tool.music_gen");

        let body = json!({
            "prompt": prompt,
            "title": title,
            "tags": style,
            "make_instrumental": instrumental,
        });

        let resp = client
            .post(&self.api_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Music generation API error {status}: {body}")),
            });
        }

        let result: serde_json::Value = resp.json().await?;
        let task_id = result
            .get("data")
            .and_then(|d| d.get("taskId"))
            .and_then(|v| v.as_str())
            .or_else(|| result.get("id").and_then(|v| v.as_str()))
            .unwrap_or("");

        let output = format!(
            "Music generation task submitted.\n\
             Task ID: {task_id}\n\
             Title: {title}\n\
             Style: {style}\n\
             Instrumental: {instrumental}\n\
             Status: Processing — poll the task status endpoint to get the audio URL."
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_gen_tool_spec() {
        let tool = ImageGenTool::new("test-key".into());
        let spec = tool.spec();
        assert_eq!(spec.name, "image_generate");
        assert!(spec.description.contains("image"));

        let params = spec.parameters;
        let props = params.get("properties").unwrap();
        assert!(props.get("prompt").is_some());
        assert!(props.get("size").is_some());
        assert!(props.get("quality").is_some());
    }

    #[test]
    fn video_gen_tool_spec() {
        let tool = VideoGenTool::new("test-key".into());
        let spec = tool.spec();
        assert_eq!(spec.name, "video_generate");
        assert!(spec.description.contains("video"));

        let params = spec.parameters;
        let props = params.get("properties").unwrap();
        assert!(props.get("prompt").is_some());
        assert!(props.get("image_url").is_some());
        assert!(props.get("duration_secs").is_some());
    }

    #[test]
    fn music_gen_tool_spec() {
        let tool = MusicGenTool::new("test-key".into());
        let spec = tool.spec();
        assert_eq!(spec.name, "music_generate");
        assert!(spec.description.contains("music"));

        let params = spec.parameters;
        let props = params.get("properties").unwrap();
        assert!(props.get("prompt").is_some());
        assert!(props.get("title").is_some());
        assert!(props.get("style").is_some());
        assert!(props.get("instrumental").is_some());
    }

    #[test]
    fn image_gen_custom_config() {
        let tool = ImageGenTool::with_config(
            "key".into(),
            "https://custom.api/images".into(),
            "custom-model".into(),
        );
        assert_eq!(tool.name(), "image_generate");
        assert_eq!(tool.model, "custom-model");
    }

    #[test]
    fn video_gen_custom_url() {
        let tool = VideoGenTool::with_api_url("key".into(), "https://custom.api/video".into());
        assert_eq!(tool.name(), "video_generate");
    }

    #[test]
    fn music_gen_custom_url() {
        let tool = MusicGenTool::with_api_url("key".into(), "https://custom.api/music".into());
        assert_eq!(tool.name(), "music_generate");
    }

    #[tokio::test]
    async fn image_gen_missing_prompt_fails() {
        let tool = ImageGenTool::new("test-key".into());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn video_gen_missing_prompt_fails() {
        let tool = VideoGenTool::new("test-key".into());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn music_gen_missing_prompt_fails() {
        let tool = MusicGenTool::new("test-key".into());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }
}
