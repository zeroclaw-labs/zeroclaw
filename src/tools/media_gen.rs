//! Image, video, music, and TTS generation tool integrations.
//!
//! Provides structured tools for AI-powered media generation:
//! - **Image generation** via Freepik Mystic API (text-to-image, 2K/4K)
//! - **Image upscaling** via Freepik Magnific (up to 16K)
//! - **Image-to-video** via Freepik (simple motion from still image)
//! - **Video generation** via Runway Gen-4 (text/image-to-video)
//! - **Music generation** via Suno (apibox.erweima.ai)
//! - **Text-to-speech** via ElevenLabs (premium TTS with dual billing)
//!
//! Each tool follows the [`Tool`] trait pattern and returns structured
//! results with download URLs and metadata.

use async_trait::async_trait;
use serde_json::json;

use super::traits::{Tool, ToolResult};

// ═══════════════════════════════════════════════════════════════════
// Image Generation Tool — Freepik Mystic API
// ═══════════════════════════════════════════════════════════════════

/// Image generation tool using Freepik Mystic API.
///
/// Supports text-to-image generation with 2K/4K resolution, engine
/// selection, realism toggle, creative detailing, and aspect ratios.
pub struct ImageGenTool {
    api_key: String,
    api_url: String,
}

impl ImageGenTool {
    /// Create a new image generation tool with the default Freepik Mystic endpoint.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.freepik.com/v1/ai/mystic".into(),
        }
    }

    /// Create with a custom API endpoint.
    pub fn with_api_url(api_key: String, api_url: String) -> Self {
        Self { api_key, api_url }
    }
}

#[async_trait]
impl Tool for ImageGenTool {
    fn name(&self) -> &str {
        "image_generate"
    }

    fn description(&self) -> &str {
        "Generate an image from a text description using Freepik Mystic API. \
         Returns a URL to the generated image. Supports 2K/4K resolution, \
         engine selection, realism toggle, and creative detailing."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text description of the image to generate"
                },
                "resolution": {
                    "type": "string",
                    "enum": ["2k", "4k"],
                    "default": "2k",
                    "description": "Output resolution"
                },
                "engine": {
                    "type": "string",
                    "enum": ["magnific_illusio", "sharpy", "sparkle"],
                    "default": "magnific_illusio",
                    "description": "Rendering engine to use"
                },
                "realism": {
                    "type": "boolean",
                    "default": false,
                    "description": "Enable photorealistic rendering"
                },
                "creative_detailing": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 100,
                    "default": 50,
                    "description": "Creative detail level (0-100)"
                },
                "aspect_ratio": {
                    "type": "string",
                    "enum": ["1:1", "3:2", "2:3", "16:9", "9:16", "4:3", "3:4"],
                    "default": "1:1",
                    "description": "Image aspect ratio"
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

        let resolution = args
            .get("resolution")
            .and_then(|v| v.as_str())
            .unwrap_or("2k");
        let engine = args
            .get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or("magnific_illusio");
        let realism = args
            .get("realism")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let creative_detailing = args
            .get("creative_detailing")
            .and_then(|v| v.as_u64())
            .unwrap_or(50);
        let aspect_ratio = args
            .get("aspect_ratio")
            .and_then(|v| v.as_str())
            .unwrap_or("1:1");

        let client = crate::config::build_runtime_proxy_client("tool.image_gen");
        let body = json!({
            "prompt": prompt,
            "resolution": resolution,
            "engine": engine,
            "realism": realism,
            "creative_detailing": creative_detailing,
            "aspect_ratio": aspect_ratio
        });

        let resp = client
            .post(&self.api_url)
            .header("x-freepik-api-key", &self.api_key)
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

        let output = format!(
            "Image generated successfully.\n\
             URL: {image_url}\n\
             Resolution: {resolution}\n\
             Engine: {engine}\n\
             Realism: {realism}\n\
             Creative detailing: {creative_detailing}\n\
             Aspect ratio: {aspect_ratio}"
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// Image Upscale Tool — Freepik Magnific
// ═══════════════════════════════════════════════════════════════════

/// Image upscaling tool using Freepik Magnific.
///
/// Supports upscaling up to 16K with configurable scale factor,
/// optimization target, and creativity level.
pub struct ImageUpscaleTool {
    api_key: String,
    api_url: String,
}

impl ImageUpscaleTool {
    /// Create a new image upscale tool with the default Freepik Magnific endpoint.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.freepik.com/v1/ai/magnific".into(),
        }
    }

    /// Create with a custom API endpoint.
    pub fn with_api_url(api_key: String, api_url: String) -> Self {
        Self { api_key, api_url }
    }
}

#[async_trait]
impl Tool for ImageUpscaleTool {
    fn name(&self) -> &str {
        "image_upscale"
    }

    fn description(&self) -> &str {
        "Upscale an image up to 16K resolution using Freepik Magnific. \
         Supports configurable scale factor, optimization target, and creativity."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "image_url": {
                    "type": "string",
                    "description": "URL of the source image to upscale"
                },
                "scale_factor": {
                    "type": "integer",
                    "enum": [2, 4, 8],
                    "default": 2,
                    "description": "Upscale multiplier (2x, 4x, or 8x)"
                },
                "optimized_for": {
                    "type": "string",
                    "enum": ["general", "portrait", "landscape", "art"],
                    "default": "general",
                    "description": "Optimization target for the upscale"
                },
                "creativity": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 100,
                    "default": 50,
                    "description": "Creativity level (0-100) — higher adds more detail"
                }
            },
            "required": ["image_url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let image_url = args
            .get("image_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: image_url"))?;

        let scale_factor = args
            .get("scale_factor")
            .and_then(|v| v.as_u64())
            .unwrap_or(2);
        let optimized_for = args
            .get("optimized_for")
            .and_then(|v| v.as_str())
            .unwrap_or("general");
        let creativity = args
            .get("creativity")
            .and_then(|v| v.as_u64())
            .unwrap_or(50);

        let client = crate::config::build_runtime_proxy_client("tool.image_upscale");
        let body = json!({
            "image_url": image_url,
            "scale_factor": scale_factor,
            "optimized_for": optimized_for,
            "creativity": creativity
        });

        let resp = client
            .post(&self.api_url)
            .header("x-freepik-api-key", &self.api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Image upscale API error {status}: {body}")),
            });
        }

        let result: serde_json::Value = resp.json().await?;
        let upscaled_url = result
            .get("data")
            .and_then(|d| d.get("url"))
            .and_then(|u| u.as_str())
            .unwrap_or("");

        let output = format!(
            "Image upscaled successfully.\n\
             URL: {upscaled_url}\n\
             Scale factor: {scale_factor}x\n\
             Optimized for: {optimized_for}\n\
             Creativity: {creativity}"
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// Freepik Image-to-Video Tool
// ═══════════════════════════════════════════════════════════════════

/// Simple image-to-video tool using Freepik.
///
/// Converts a still image into a short video with optional motion prompt.
pub struct FreepikImageToVideoTool {
    api_key: String,
    api_url: String,
}

impl FreepikImageToVideoTool {
    /// Create a new Freepik image-to-video tool with the default endpoint.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.freepik.com/v1/ai/image-to-video".into(),
        }
    }

    /// Create with a custom API endpoint.
    pub fn with_api_url(api_key: String, api_url: String) -> Self {
        Self { api_key, api_url }
    }
}

#[async_trait]
impl Tool for FreepikImageToVideoTool {
    fn name(&self) -> &str {
        "freepik_image_to_video"
    }

    fn description(&self) -> &str {
        "Convert a still image into a short video using Freepik. \
         Requires the source image URL and accepts an optional motion prompt."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "image_url": {
                    "type": "string",
                    "description": "URL of the source image to animate"
                },
                "motion_prompt": {
                    "type": "string",
                    "description": "Optional description of the desired motion"
                }
            },
            "required": ["image_url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let image_url = args
            .get("image_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: image_url"))?;

        let motion_prompt = args.get("motion_prompt").and_then(|v| v.as_str());

        let client = crate::config::build_runtime_proxy_client("tool.freepik_i2v");
        let mut body = json!({
            "image_url": image_url
        });

        if let Some(mp) = motion_prompt {
            body["motion_prompt"] = json!(mp);
        }

        let resp = client
            .post(&self.api_url)
            .header("x-freepik-api-key", &self.api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Freepik image-to-video API error {status}: {body}")),
            });
        }

        let result: serde_json::Value = resp.json().await?;
        let task_id = result
            .get("data")
            .and_then(|d| d.get("task_id"))
            .and_then(|v| v.as_str())
            .or_else(|| result.get("id").and_then(|v| v.as_str()))
            .unwrap_or("");

        let output = format!(
            "Image-to-video task submitted.\n\
             Task ID: {task_id}\n\
             Motion prompt: {}\n\
             Status: Processing — poll the task status endpoint to get the result URL.",
            motion_prompt.unwrap_or("(none)")
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// Video Generation Tool — Runway Gen-4
// ═══════════════════════════════════════════════════════════════════

/// Video generation tool using Runway Gen-4.
///
/// Supports text-to-video and image-to-video generation with 5/10s duration.
pub struct VideoGenTool {
    api_key: String,
    api_url: String,
    model: String,
}

impl VideoGenTool {
    /// Create a new video generation tool with the default Runway endpoint.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.dev.runwayml.com/v1/image_to_video".into(),
            model: "gen4_turbo".into(),
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
impl Tool for VideoGenTool {
    fn name(&self) -> &str {
        "video_generate"
    }

    fn description(&self) -> &str {
        "Generate a short video from a text description or source image \
         using Runway Gen-4. Returns a task ID to poll for the result."
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
            "model": self.model
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
// Music Generation Tool — Suno via apibox.erweima.ai
// ═══════════════════════════════════════════════════════════════════

/// Music generation tool using Suno via apibox.erweima.ai.
///
/// Supports text-to-music with style tags, custom lyrics, and
/// instrumental mode.
pub struct MusicGenTool {
    api_key: String,
    api_url: String,
}

impl MusicGenTool {
    /// Create a new music generation tool with the default Suno endpoint.
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
        "Generate music from a text description using Suno. Returns a task ID \
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
                "custom_lyrics": {
                    "type": "string",
                    "description": "Custom lyrics for vocal tracks"
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
        let custom_lyrics = args.get("custom_lyrics").and_then(|v| v.as_str());
        let instrumental = args
            .get("instrumental")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let client = crate::config::build_runtime_proxy_client("tool.music_gen");

        let mut body = json!({
            "prompt": prompt,
            "title": title,
            "tags": style,
            "make_instrumental": instrumental,
        });

        if let Some(lyrics) = custom_lyrics {
            body["custom_lyrics"] = json!(lyrics);
        }

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

// ═══════════════════════════════════════════════════════════════════
// ElevenLabs TTS Tool
// ═══════════════════════════════════════════════════════════════════

/// Billing key type for ElevenLabs TTS.
enum ElevenLabsBillingKey {
    /// User provides their own API key — no platform credit charge.
    UserKey(String),
    /// Platform-managed key with a credit multiplier for billing.
    PlatformKey {
        api_key: String,
        credit_multiplier: f64,
    },
}

/// Premium text-to-speech tool using ElevenLabs.
///
/// Supports voice selection, model choice, stability/similarity tuning,
/// and dual billing modes (user key vs platform key with credit multiplier).
pub struct ElevenLabsTtsTool {
    billing_key: ElevenLabsBillingKey,
    api_url: String,
}

impl ElevenLabsTtsTool {
    /// Create with a user-supplied API key (no platform billing).
    pub fn new_user_key(api_key: String) -> Self {
        Self {
            billing_key: ElevenLabsBillingKey::UserKey(api_key),
            api_url: "https://api.elevenlabs.io".into(),
        }
    }

    /// Create with a platform-managed API key and credit multiplier.
    pub fn new_platform_key(api_key: String, credit_multiplier: f64) -> Self {
        Self {
            billing_key: ElevenLabsBillingKey::PlatformKey {
                api_key,
                credit_multiplier,
            },
            api_url: "https://api.elevenlabs.io".into(),
        }
    }

    /// Whether this tool uses platform billing.
    pub fn is_platform_billing(&self) -> bool {
        matches!(self.billing_key, ElevenLabsBillingKey::PlatformKey { .. })
    }

    /// Return the credit multiplier (1.0 for user keys).
    pub fn credit_multiplier(&self) -> f64 {
        match &self.billing_key {
            ElevenLabsBillingKey::UserKey(_) => 1.0,
            ElevenLabsBillingKey::PlatformKey {
                credit_multiplier, ..
            } => *credit_multiplier,
        }
    }

    /// Estimate credit cost from character count.
    fn estimate_credit_cost(&self, char_count: usize) -> f64 {
        // Base cost: 1 credit per 100 characters (rough estimate).
        let base = char_count as f64 / 100.0;
        base * self.credit_multiplier()
    }

    fn api_key(&self) -> &str {
        match &self.billing_key {
            ElevenLabsBillingKey::UserKey(key) => key,
            ElevenLabsBillingKey::PlatformKey { api_key, .. } => api_key,
        }
    }
}

#[async_trait]
impl Tool for ElevenLabsTtsTool {
    fn name(&self) -> &str {
        "elevenlabs_tts"
    }

    fn description(&self) -> &str {
        "Generate premium text-to-speech audio using ElevenLabs. \
         Supports voice selection, model choice, and stability/similarity tuning."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to synthesize into speech"
                },
                "voice_id": {
                    "type": "string",
                    "description": "ElevenLabs voice ID to use"
                },
                "model_id": {
                    "type": "string",
                    "default": "eleven_multilingual_v2",
                    "description": "TTS model to use (e.g. eleven_multilingual_v2, eleven_turbo_v2)"
                },
                "stability": {
                    "type": "number",
                    "minimum": 0.0,
                    "maximum": 1.0,
                    "default": 0.5,
                    "description": "Voice stability (0.0-1.0)"
                },
                "similarity_boost": {
                    "type": "number",
                    "minimum": 0.0,
                    "maximum": 1.0,
                    "default": 0.75,
                    "description": "Voice similarity boost (0.0-1.0)"
                },
                "output_format": {
                    "type": "string",
                    "enum": ["mp3_44100_128", "mp3_44100_192", "pcm_16000", "pcm_22050", "pcm_24000", "pcm_44100"],
                    "default": "mp3_44100_128",
                    "description": "Audio output format"
                }
            },
            "required": ["text", "voice_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: text"))?;

        let voice_id = args
            .get("voice_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: voice_id"))?;

        let model_id = args
            .get("model_id")
            .and_then(|v| v.as_str())
            .unwrap_or("eleven_multilingual_v2");
        let stability = args
            .get("stability")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);
        let similarity_boost = args
            .get("similarity_boost")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.75);
        let output_format = args
            .get("output_format")
            .and_then(|v| v.as_str())
            .unwrap_or("mp3_44100_128");

        let char_count = text.len();
        let estimated_cost = self.estimate_credit_cost(char_count);

        let client = crate::config::build_runtime_proxy_client("tool.elevenlabs_tts");
        let url = format!(
            "{}/v1/text-to-speech/{}",
            self.api_url.trim_end_matches('/'),
            voice_id
        );

        let body = json!({
            "text": text,
            "model_id": model_id,
            "voice_settings": {
                "stability": stability,
                "similarity_boost": similarity_boost
            },
            "output_format": output_format
        });

        let resp = client
            .post(&url)
            .header("xi-api-key", self.api_key())
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("ElevenLabs TTS API error {status}: {body}")),
            });
        }

        let content_length = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let output = format!(
            "TTS audio generated successfully.\n\
             Voice: {voice_id}\n\
             Model: {model_id}\n\
             Characters: {char_count}\n\
             Estimated credits: {estimated_cost:.2}\n\
             Platform billing: {}\n\
             Output format: {output_format}\n\
             Content-Length: {content_length}",
            self.is_platform_billing()
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
        assert!(props.get("resolution").is_some());
        assert!(props.get("engine").is_some());
        assert!(props.get("realism").is_some());
        assert!(props.get("creative_detailing").is_some());
        assert!(props.get("aspect_ratio").is_some());
    }

    #[test]
    fn image_upscale_tool_spec() {
        let tool = ImageUpscaleTool::new("test-key".into());
        let spec = tool.spec();
        assert_eq!(spec.name, "image_upscale");
        assert!(spec.description.contains("Upscale"));

        let params = spec.parameters;
        let props = params.get("properties").unwrap();
        assert!(props.get("image_url").is_some());
        assert!(props.get("scale_factor").is_some());
        assert!(props.get("optimized_for").is_some());
        assert!(props.get("creativity").is_some());
    }

    #[test]
    fn freepik_image_to_video_tool_spec() {
        let tool = FreepikImageToVideoTool::new("test-key".into());
        let spec = tool.spec();
        assert_eq!(spec.name, "freepik_image_to_video");
        assert!(spec.description.contains("image"));

        let params = spec.parameters;
        let props = params.get("properties").unwrap();
        assert!(props.get("image_url").is_some());
        assert!(props.get("motion_prompt").is_some());
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
        assert!(props.get("custom_lyrics").is_some());
        assert!(props.get("instrumental").is_some());
    }

    #[test]
    fn elevenlabs_tts_tool_spec() {
        let tool = ElevenLabsTtsTool::new_user_key("test-key".into());
        let spec = tool.spec();
        assert_eq!(spec.name, "elevenlabs_tts");
        assert!(spec.description.contains("text-to-speech"));

        let params = spec.parameters;
        let props = params.get("properties").unwrap();
        assert!(props.get("text").is_some());
        assert!(props.get("voice_id").is_some());
        assert!(props.get("model_id").is_some());
        assert!(props.get("stability").is_some());
        assert!(props.get("similarity_boost").is_some());
        assert!(props.get("output_format").is_some());
    }

    #[test]
    fn elevenlabs_user_key_no_billing() {
        let tool = ElevenLabsTtsTool::new_user_key("user-key-123".into());
        assert!(!tool.is_platform_billing());
        assert!((tool.credit_multiplier() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn elevenlabs_platform_key_billing() {
        let tool = ElevenLabsTtsTool::new_platform_key("platform-key-456".into(), 2.5);
        assert!(tool.is_platform_billing());
        assert!((tool.credit_multiplier() - 2.5).abs() < f64::EPSILON);
        // Estimate: 500 chars => 5.0 base credits * 2.5 multiplier = 12.5
        let cost = tool.estimate_credit_cost(500);
        assert!((cost - 12.5).abs() < f64::EPSILON);
    }

    #[test]
    fn image_gen_custom_url() {
        let tool = ImageGenTool::with_api_url("key".into(), "https://custom.api/mystic".into());
        assert_eq!(tool.name(), "image_generate");
        assert_eq!(tool.api_url, "https://custom.api/mystic");
    }

    #[test]
    fn image_upscale_custom_url() {
        let tool =
            ImageUpscaleTool::with_api_url("key".into(), "https://custom.api/magnific".into());
        assert_eq!(tool.name(), "image_upscale");
        assert_eq!(tool.api_url, "https://custom.api/magnific");
    }

    #[test]
    fn freepik_i2v_custom_url() {
        let tool =
            FreepikImageToVideoTool::with_api_url("key".into(), "https://custom.api/i2v".into());
        assert_eq!(tool.name(), "freepik_image_to_video");
        assert_eq!(tool.api_url, "https://custom.api/i2v");
    }

    #[test]
    fn video_gen_custom_config() {
        let tool = VideoGenTool::with_config(
            "key".into(),
            "https://custom.api/video".into(),
            "gen4_custom".into(),
        );
        assert_eq!(tool.name(), "video_generate");
        assert_eq!(tool.model, "gen4_custom");
        assert_eq!(tool.api_url, "https://custom.api/video");
    }

    #[test]
    fn music_gen_custom_url() {
        let tool = MusicGenTool::with_api_url("key".into(), "https://custom.api/music".into());
        assert_eq!(tool.name(), "music_generate");
        assert_eq!(tool.api_url, "https://custom.api/music");
    }

    #[tokio::test]
    async fn image_gen_missing_prompt_fails() {
        let tool = ImageGenTool::new("test-key".into());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn image_upscale_missing_image_url_fails() {
        let tool = ImageUpscaleTool::new("test-key".into());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn freepik_i2v_missing_image_url_fails() {
        let tool = FreepikImageToVideoTool::new("test-key".into());
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

    #[tokio::test]
    async fn elevenlabs_tts_missing_text_fails() {
        let tool = ElevenLabsTtsTool::new_user_key("test-key".into());
        let result = tool.execute(json!({"voice_id": "abc"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn elevenlabs_tts_missing_voice_id_fails() {
        let tool = ElevenLabsTtsTool::new_user_key("test-key".into());
        let result = tool.execute(json!({"text": "hello"})).await;
        assert!(result.is_err());
    }
}
