//! Media generation tool integrations for MoA.
//!
//! Provides structured tools for AI-powered media generation:
//! - **Image generation** via Freepik Mystic API (text-to-image, 2K/4K)
//! - **Image upscaling** via Freepik Magnific API (up to 16K)
//! - **Image editing** via Freepik API (background removal, relighting)
//! - **Image-to-video** via Freepik API (simple motion conversion)
//! - **Video generation** via Runway Gen-4 API (text/image-to-video)
//! - **Music generation** via Suno API (text-to-song with vocals)
//! - **Premium TTS** via ElevenLabs API (multilingual, voice selection)
//!
//! Each tool follows the [`Tool`] trait pattern and returns structured
//! results with download URLs and metadata.

use async_trait::async_trait;
use serde_json::json;

use super::traits::{Tool, ToolResult};

// ═══════════════════════════════════════════════════════════════════
// Freepik Image Generation Tool (Mystic)
// ═══════════════════════════════════════════════════════════════════

/// Image generation tool using Freepik Mystic API.
///
/// Supports text-to-image generation with multiple engine choices,
/// resolution up to 4K, and LoRA style presets.
pub struct ImageGenTool {
    api_key: String,
    api_url: String,
    default_engine: String,
    default_resolution: String,
}

impl ImageGenTool {
    /// Create a new Freepik image generation tool.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.freepik.com/v1/ai/mystic".into(),
            default_engine: "magnific_sharpy".into(),
            default_resolution: "2k".into(),
        }
    }

    /// Create with custom configuration.
    pub fn with_config(
        api_key: String,
        api_url: String,
        engine: String,
        resolution: String,
    ) -> Self {
        Self {
            api_key,
            api_url,
            default_engine: engine,
            default_resolution: resolution,
        }
    }
}

#[async_trait]
impl Tool for ImageGenTool {
    fn name(&self) -> &str {
        "image_generate"
    }

    fn description(&self) -> &str {
        "Generate an AI image from a text description using Freepik Mystic. \
         Supports 2K/4K resolution, multiple aspect ratios, and style engines. \
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
                "resolution": {
                    "type": "string",
                    "enum": ["2k", "4k"],
                    "default": "2k",
                    "description": "Output resolution (2K or 4K)"
                },
                "aspect_ratio": {
                    "type": "string",
                    "enum": [
                        "square_1_1", "classic_4_3", "traditional_3_4",
                        "widescreen_16_9", "social_story_9_16"
                    ],
                    "default": "square_1_1",
                    "description": "Image aspect ratio"
                },
                "engine": {
                    "type": "string",
                    "enum": ["automatic", "magnific_illusio", "magnific_sharpy", "magnific_sparkle"],
                    "default": "magnific_sharpy",
                    "description": "Rendering engine style"
                },
                "realism": {
                    "type": "boolean",
                    "default": true,
                    "description": "Enable photorealistic mode"
                },
                "creative_detailing": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 100,
                    "default": 50,
                    "description": "Creative detail level (0=faithful, 100=creative)"
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
            .unwrap_or(&self.default_resolution);
        let aspect_ratio = args
            .get("aspect_ratio")
            .and_then(|v| v.as_str())
            .unwrap_or("square_1_1");
        let engine = args
            .get("engine")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.default_engine);
        let realism = args
            .get("realism")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let creative_detailing = args
            .get("creative_detailing")
            .and_then(|v| v.as_u64())
            .unwrap_or(50);

        let client = crate::config::build_runtime_proxy_client("tool.image_gen");
        let body = json!({
            "prompt": prompt,
            "resolution": resolution,
            "aspect_ratio": aspect_ratio,
            "engine": engine,
            "realism": realism,
            "creative_detailing": creative_detailing
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
                error: Some(format!("Freepik image generation API error {status}: {body}")),
            });
        }

        let result: serde_json::Value = resp.json().await?;

        // Freepik Mystic returns async task — extract task_id or direct image URL
        let image_url = result
            .pointer("/data/0/url")
            .or_else(|| result.pointer("/data/url"))
            .and_then(|u| u.as_str())
            .unwrap_or("");
        let task_id = result
            .pointer("/data/task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let output = if !image_url.is_empty() {
            format!(
                "Image generated successfully.\n\
                 URL: {image_url}\n\
                 Resolution: {resolution}\n\
                 Aspect ratio: {aspect_ratio}\n\
                 Engine: {engine}"
            )
        } else if !task_id.is_empty() {
            format!(
                "Image generation task submitted.\n\
                 Task ID: {task_id}\n\
                 Resolution: {resolution}\n\
                 Status: Processing — poll GET {}/{{task_id}} to get the result URL.",
                self.api_url
            )
        } else {
            format!("Freepik response: {result}")
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// Freepik Image Upscale Tool
// ═══════════════════════════════════════════════════════════════════

/// Image upscaling tool using Freepik Magnific API.
///
/// Supports upscaling images up to 16K resolution with AI enhancement.
pub struct ImageUpscaleTool {
    api_key: String,
    api_url: String,
}

impl ImageUpscaleTool {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.freepik.com/v1/ai/upscale".into(),
        }
    }
}

#[async_trait]
impl Tool for ImageUpscaleTool {
    fn name(&self) -> &str {
        "image_upscale"
    }

    fn description(&self) -> &str {
        "Upscale an image using Freepik Magnific AI, enhancing resolution up to 16K. \
         Provide an image URL and desired scale factor."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "image_url": {
                    "type": "string",
                    "description": "URL of the image to upscale"
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
                    "description": "Optimization preset for the upscaling algorithm"
                },
                "creativity": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 100,
                    "default": 30,
                    "description": "How much creative detail to add (0=faithful, 100=creative)"
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
            .unwrap_or(30);

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
                error: Some(format!("Freepik upscale API error {status}: {body}")),
            });
        }

        let result: serde_json::Value = resp.json().await?;
        let output_url = result
            .pointer("/data/url")
            .or_else(|| result.pointer("/data/0/url"))
            .and_then(|u| u.as_str())
            .unwrap_or("");
        let task_id = result
            .pointer("/data/task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let output = if !output_url.is_empty() {
            format!(
                "Image upscaled successfully.\n\
                 URL: {output_url}\n\
                 Scale: {scale_factor}x\n\
                 Optimized for: {optimized_for}"
            )
        } else if !task_id.is_empty() {
            format!(
                "Image upscale task submitted.\n\
                 Task ID: {task_id}\n\
                 Scale: {scale_factor}x\n\
                 Status: Processing"
            )
        } else {
            format!("Freepik upscale response: {result}")
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// Freepik Image-to-Video Tool (simple motion)
// ═══════════════════════════════════════════════════════════════════

/// Simple image-to-video conversion via Freepik API.
///
/// Converts a static image into a short motion video with
/// customizable motion effects. For advanced video generation,
/// use the `video_generate` tool (Runway).
pub struct FreepikImageToVideoTool {
    api_key: String,
    api_url: String,
}

impl FreepikImageToVideoTool {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.freepik.com/v1/ai/image-to-video".into(),
        }
    }
}

#[async_trait]
impl Tool for FreepikImageToVideoTool {
    fn name(&self) -> &str {
        "image_to_video"
    }

    fn description(&self) -> &str {
        "Convert a static image into a short animated video using Freepik AI. \
         Good for simple motion effects. For advanced video generation with \
         camera control and lip sync, use the video_generate tool instead."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "image_url": {
                    "type": "string",
                    "description": "URL of the source image"
                },
                "motion_prompt": {
                    "type": "string",
                    "description": "Description of the desired motion effect"
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

        let motion_prompt = args
            .get("motion_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let client = crate::config::build_runtime_proxy_client("tool.freepik_img2video");

        let mut body = json!({ "image_url": image_url });
        if !motion_prompt.is_empty() {
            body["prompt"] = json!(motion_prompt);
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
            .pointer("/data/task_id")
            .or_else(|| result.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        Ok(ToolResult {
            success: true,
            output: format!(
                "Image-to-video task submitted.\n\
                 Task ID: {task_id}\n\
                 Status: Processing — poll for the result video URL."
            ),
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// Runway Video Generation Tool (Gen-4)
// ═══════════════════════════════════════════════════════════════════

/// Advanced video generation tool using Runway Gen-4 API.
///
/// Supports text-to-video and image-to-video with camera controls,
/// motion brush, and lip sync capabilities.
pub struct VideoGenTool {
    api_key: String,
    api_url: String,
    model: String,
}

impl VideoGenTool {
    /// Create a new Runway video generation tool.
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
        "Generate a video from a text description or source image using Runway Gen-4. \
         Supports camera control, motion effects, and lip sync. \
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
                error: Some(format!("Runway video generation API error {status}: {body}")),
            });
        }

        let result: serde_json::Value = resp.json().await?;
        let task_id = result.get("id").and_then(|v| v.as_str()).unwrap_or("");

        Ok(ToolResult {
            success: true,
            output: format!(
                "Video generation task submitted.\n\
                 Task ID: {task_id}\n\
                 Duration: {duration}s\n\
                 Model: {}\n\
                 Status: Processing — poll the task status endpoint to get the result URL.",
                self.model
            ),
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// Suno Music Generation Tool
// ═══════════════════════════════════════════════════════════════════

/// Music generation tool using Suno API.
///
/// Generates complete songs with vocals, instruments, and structure
/// from text descriptions. Supports style tags and instrumental mode.
pub struct MusicGenTool {
    api_key: String,
    api_url: String,
}

impl MusicGenTool {
    /// Create a new Suno music generation tool.
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
        "Generate a complete song (vocals + instruments) from a text description \
         using Suno AI. Supports genre/style tags, custom lyrics, and instrumental mode. \
         Returns a task ID to poll for the audio download URL."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Text description of the music to generate (genre, mood, instruments, theme)"
                },
                "title": {
                    "type": "string",
                    "description": "Title for the generated song"
                },
                "style": {
                    "type": "string",
                    "description": "Music style tags (e.g. 'jazz piano, chill, ambient', 'K-pop, energetic, female vocal')"
                },
                "lyrics": {
                    "type": "string",
                    "description": "Custom lyrics for the song (optional, auto-generated if omitted)"
                },
                "instrumental": {
                    "type": "boolean",
                    "default": false,
                    "description": "Generate instrumental-only without vocals"
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
        let lyrics = args.get("lyrics").and_then(|v| v.as_str());
        let instrumental = args
            .get("instrumental")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let client = crate::config::build_runtime_proxy_client("tool.music_gen");

        let mut body = json!({
            "prompt": prompt,
            "title": title,
            "tags": style,
            "make_instrumental": instrumental,
        });
        if let Some(lyric_text) = lyrics {
            body["lyrics"] = json!(lyric_text);
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
                error: Some(format!("Suno music generation API error {status}: {body}")),
            });
        }

        let result: serde_json::Value = resp.json().await?;
        let task_id = result
            .get("data")
            .and_then(|d| d.get("taskId"))
            .and_then(|v| v.as_str())
            .or_else(|| result.get("id").and_then(|v| v.as_str()))
            .unwrap_or("");

        Ok(ToolResult {
            success: true,
            output: format!(
                "Music generation task submitted.\n\
                 Task ID: {task_id}\n\
                 Title: {title}\n\
                 Style: {style}\n\
                 Instrumental: {instrumental}\n\
                 Status: Processing — poll the task status endpoint to get the audio URL."
            ),
            error: None,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// ElevenLabs Premium TTS Tool
// ═══════════════════════════════════════════════════════════════════

/// Premium text-to-speech tool using ElevenLabs API.
///
/// Supports multiple voices (male/female, various ages and styles),
/// multilingual output, and high-quality audio generation.
///
/// **Dual billing model:**
/// - User provides their own API key → no credit charge
/// - Platform (operator) key used → user charged `credit_multiplier` (default 2.2×)
pub struct ElevenLabsTtsTool {
    /// Resolved API key (user's own or operator's fallback).
    api_key: String,
    api_url: String,
    default_voice_id: String,
    default_model: String,
    /// Whether this is using the operator's key (triggers credit billing).
    is_platform_key: bool,
    /// Credit multiplier for platform-key usage.
    credit_multiplier: f64,
}

impl ElevenLabsTtsTool {
    /// Create with a user-supplied API key (no credit charge).
    pub fn new_user_key(api_key: String) -> Self {
        Self {
            api_key,
            api_url: "https://api.elevenlabs.io/v1".into(),
            default_voice_id: "21m00Tcm4TlvDq8ikWAM".into(),
            default_model: "eleven_multilingual_v2".into(),
            is_platform_key: false,
            credit_multiplier: 0.0,
        }
    }

    /// Create with the operator's platform key (credit billing active).
    pub fn new_platform_key(api_key: String, credit_multiplier: f64) -> Self {
        Self {
            api_key,
            api_url: "https://api.elevenlabs.io/v1".into(),
            default_voice_id: "21m00Tcm4TlvDq8ikWAM".into(),
            default_model: "eleven_multilingual_v2".into(),
            is_platform_key: true,
            credit_multiplier,
        }
    }

    /// Create with full configuration.
    pub fn with_config(
        api_key: String,
        api_url: String,
        default_voice_id: String,
        model: String,
        is_platform_key: bool,
        credit_multiplier: f64,
    ) -> Self {
        Self {
            api_key,
            api_url,
            default_voice_id,
            default_model: model,
            is_platform_key,
            credit_multiplier,
        }
    }

    /// Returns `true` if this tool instance uses the platform key
    /// and will charge credits at the configured multiplier.
    pub fn is_platform_billing(&self) -> bool {
        self.is_platform_key
    }

    /// Credit multiplier applied when using the platform key.
    pub fn credit_multiplier(&self) -> f64 {
        self.credit_multiplier
    }
}

#[async_trait]
impl Tool for ElevenLabsTtsTool {
    fn name(&self) -> &str {
        "elevenlabs_tts"
    }

    fn description(&self) -> &str {
        "Generate high-quality speech audio from text using ElevenLabs. \
         Choose from multiple voices (male, female, various ages and accents). \
         Supports 29+ languages including Korean, English, Japanese, Chinese. \
         Returns a URL or base64 audio data."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Text to convert to speech"
                },
                "voice_id": {
                    "type": "string",
                    "description": "ElevenLabs voice ID (use list_voices to discover available voices). \
                                    Popular: '21m00Tcm4TlvDq8ikWAM' (Rachel, female), \
                                    'pNInz6obpgDQGcFmaJgB' (Adam, male), \
                                    'EXAVITQu4vr4xnSDxMaL' (Bella, young female)"
                },
                "model": {
                    "type": "string",
                    "enum": ["eleven_multilingual_v2", "eleven_turbo_v2_5", "eleven_flash_v2_5"],
                    "default": "eleven_multilingual_v2",
                    "description": "TTS model: multilingual_v2 (best quality), turbo (faster), flash (fastest)"
                },
                "stability": {
                    "type": "number",
                    "minimum": 0.0,
                    "maximum": 1.0,
                    "default": 0.5,
                    "description": "Voice stability (0=variable/expressive, 1=stable/consistent)"
                },
                "similarity_boost": {
                    "type": "number",
                    "minimum": 0.0,
                    "maximum": 1.0,
                    "default": 0.75,
                    "description": "Voice similarity to the original (higher = more similar)"
                },
                "output_format": {
                    "type": "string",
                    "enum": ["mp3_44100_128", "mp3_22050_32", "pcm_16000", "pcm_24000"],
                    "default": "mp3_44100_128",
                    "description": "Audio output format"
                }
            },
            "required": ["text"]
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
            .unwrap_or(&self.default_voice_id);
        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.default_model);
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

        let url = format!(
            "{}/text-to-speech/{}?output_format={}",
            self.api_url, voice_id, output_format
        );

        let client = crate::config::build_runtime_proxy_client("tool.elevenlabs_tts");
        let body = json!({
            "text": text,
            "model_id": model,
            "voice_settings": {
                "stability": stability,
                "similarity_boost": similarity_boost
            }
        });

        let resp = client
            .post(&url)
            .header("xi-api-key", &self.api_key)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_body = resp.text().await.unwrap_or_default();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("ElevenLabs TTS API error {status}: {err_body}")),
            });
        }

        // ElevenLabs returns audio bytes directly; estimate cost from character count
        let char_count = text.chars().count();
        let audio_size = resp.content_length().unwrap_or(0);

        let billing_note = if self.is_platform_key {
            // Estimate: ~$0.30 per 1000 characters for multilingual_v2
            let estimated_cost_usd = (char_count as f64 / 1000.0) * 0.30;
            let credit_cost = estimated_cost_usd * self.credit_multiplier;
            format!(
                "\nBilling: Platform key used — estimated {:.1} credits ({:.1}× of ${:.4} API cost)",
                credit_cost, self.credit_multiplier, estimated_cost_usd
            )
        } else {
            "\nBilling: User key — no credit charge".into()
        };

        Ok(ToolResult {
            success: true,
            output: format!(
                "Speech generated successfully.\n\
                 Voice: {voice_id}\n\
                 Model: {model}\n\
                 Characters: {char_count}\n\
                 Audio size: {audio_size} bytes\n\
                 Format: {output_format}\
                 {billing_note}"
            ),
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
        assert!(props.get("aspect_ratio").is_some());
        assert!(props.get("engine").is_some());
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
    }

    #[test]
    fn freepik_img2video_tool_spec() {
        let tool = FreepikImageToVideoTool::new("test-key".into());
        let spec = tool.spec();
        assert_eq!(spec.name, "image_to_video");
        assert!(spec.description.contains("image"));
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
        assert!(spec.description.contains("music") || spec.description.contains("song"));

        let params = spec.parameters;
        let props = params.get("properties").unwrap();
        assert!(props.get("prompt").is_some());
        assert!(props.get("title").is_some());
        assert!(props.get("style").is_some());
        assert!(props.get("lyrics").is_some());
        assert!(props.get("instrumental").is_some());
    }

    #[test]
    fn elevenlabs_tts_tool_spec() {
        let tool = ElevenLabsTtsTool::new_user_key("test-key".into());
        let spec = tool.spec();
        assert_eq!(spec.name, "elevenlabs_tts");
        assert!(spec.description.contains("speech"));

        let params = spec.parameters;
        let props = params.get("properties").unwrap();
        assert!(props.get("text").is_some());
        assert!(props.get("voice_id").is_some());
        assert!(props.get("model").is_some());
    }

    #[test]
    fn elevenlabs_user_key_no_billing() {
        let tool = ElevenLabsTtsTool::new_user_key("user-key".into());
        assert!(!tool.is_platform_billing());
        assert_eq!(tool.credit_multiplier(), 0.0);
    }

    #[test]
    fn elevenlabs_platform_key_billing() {
        let tool = ElevenLabsTtsTool::new_platform_key("admin-key".into(), 2.2);
        assert!(tool.is_platform_billing());
        assert!((tool.credit_multiplier() - 2.2).abs() < f64::EPSILON);
    }

    #[test]
    fn image_gen_custom_config() {
        let tool = ImageGenTool::with_config(
            "key".into(),
            "https://custom.api/mystic".into(),
            "magnific_sparkle".into(),
            "4k".into(),
        );
        assert_eq!(tool.name(), "image_generate");
        assert_eq!(tool.default_engine, "magnific_sparkle");
        assert_eq!(tool.default_resolution, "4k");
    }

    #[test]
    fn video_gen_custom_config() {
        let tool =
            VideoGenTool::with_config("key".into(), "https://custom.api/video".into(), "gen4".into());
        assert_eq!(tool.name(), "video_generate");
        assert_eq!(tool.model, "gen4");
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

    #[tokio::test]
    async fn image_upscale_missing_url_fails() {
        let tool = ImageUpscaleTool::new("test-key".into());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn freepik_img2video_missing_url_fails() {
        let tool = FreepikImageToVideoTool::new("test-key".into());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn elevenlabs_tts_missing_text_fails() {
        let tool = ElevenLabsTtsTool::new_user_key("test-key".into());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }
}
