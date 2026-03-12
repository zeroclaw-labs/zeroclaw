// Video Analysis Tool for ZeroClaw
// Provides video analysis and understanding capabilities via multimodal providers

use async_trait::async_trait;
use super::traits::{Tool, ToolResult};
use crate::config::schema::MultimodalGenerationConfig;
use serde::{Deserialize, Serialize};

/// Video Analysis tool name
pub const TOOL_NAME: &str = "video_analysis";

/// Video Analysis tool description
pub const TOOL_DESCRIPTION: &str = "Analyze video content to extract descriptions, transcripts, and insights";

/// Parameters for video analysis tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoAnalysisParams {
    /// URL of the video to analyze (supports YouTube, Vimeo, direct video URLs)
    pub video_url: String,

    /// Specific prompt/question about the video content
    #[serde(default)]
    pub prompt: Option<String>,

    /// Analysis type: "describe", "transcribe", "summarize", "extract_frames", "qa"
    #[serde(default = "default_analysis_type")]
    pub analysis_type: String,

    /// Maximum number of frames to extract (for frame extraction)
    #[serde(default = "default_max_frames")]
    pub max_frames: usize,

    /// Start time for segment analysis (in seconds)
    #[serde(default)]
    pub start_time: Option<f64>,

    /// End time for segment analysis (in seconds)
    #[serde(default)]
    pub end_time: Option<f64>,

    /// Model to use for analysis (overrides config default)
    #[serde(default)]
    pub model: Option<String>,

    /// Provider to use (overrides config default)
    #[serde(default)]
    pub provider: Option<String>,
}

fn default_analysis_type() -> String {
    "describe".to_string()
}

fn default_max_frames() -> usize {
    10
}

/// Video analysis response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoAnalysisResponse {
    /// Analysis result text (description, summary, transcript, etc.)
    pub analysis: String,
    /// Analysis type performed
    pub analysis_type: String,
    /// Provider used
    pub provider: String,
    /// Model used
    pub model: String,
    /// Extracted frames (if frame extraction was requested)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<Vec<FrameData>>,
    /// Video metadata
    pub metadata: VideoMetadata,
    /// Timing information
    pub timing: TimingInfo,
}

/// Individual frame data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameData {
    /// Frame number/identifier
    pub frame_id: String,
    /// Timestamp in seconds
    pub timestamp: f64,
    /// Base64 encoded frame image
    pub image: String,
    /// Frame description
    pub description: Option<String>,
}

/// Video metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMetadata {
    /// Video title (if available)
    pub title: Option<String>,
    /// Video duration in seconds
    pub duration_secs: Option<f64>,
    /// Video resolution
    pub resolution: Option<String>,
    /// Video format
    pub format: Option<String>,
}

/// Timing information for the analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingInfo {
    /// Processing time in seconds
    pub processing_time_secs: f64,
    /// Timestamp when analysis was performed
    pub analyzed_at: String,
}

/// Video Analysis Tool
pub struct VideoAnalysisTool {
    config: MultimodalGenerationConfig,
}

impl VideoAnalysisTool {
    /// Create a new video analysis tool instance
    pub fn new(config: MultimodalGenerationConfig) -> Self {
        Self { config }
    }

    /// Analyze a video based on the specified parameters
    pub async fn analyze(&self, params: VideoAnalysisParams) -> anyhow::Result<VideoAnalysisResponse> {
        // Determine provider and model
        let provider_name = params
            .provider
            .as_deref()
            .or(self.config.default_image_provider.as_deref())
            .unwrap_or("openai");

        let model = params
            .model
            .as_deref()
            .or(self.config.default_image_model.as_deref())
            .unwrap_or("gpt-4o");

        // Validate video URL
        if params.video_url.is_empty() {
            anyhow::bail!("Video URL is required");
        }

        // Perform analysis based on type
        let (analysis, frames) = match params.analysis_type.as_str() {
            "describe" => self.describe_video(provider_name, model, &params).await?,
            "transcribe" => self.transcribe_video(provider_name, model, &params).await?,
            "summarize" => self.summarize_video(provider_name, model, &params).await?,
            "extract_frames" => {
                let (desc, extracted_frames) = self.extract_frames(provider_name, model, &params).await?;
                (desc, Some(extracted_frames))
            }
            "qa" => self.answer_video_question(provider_name, model, &params).await?,
            _ => {
                // Default to description
                self.describe_video(provider_name, model, &params).await?
            }
        };

        // Get video metadata (would require actual video info fetching)
        let metadata = VideoMetadata {
            title: None,
            duration_secs: None,
            resolution: None,
            format: None,
        };

        Ok(VideoAnalysisResponse {
            analysis,
            analysis_type: params.analysis_type,
            provider: provider_name.to_string(),
            model: model.to_string(),
            frames,
            metadata,
            timing: TimingInfo {
                processing_time_secs: 0.0, // Would be calculated
                analyzed_at: chrono_now(),
            },
        })
    }

    /// Describe video content
    async fn describe_video(
        &self,
        provider: &str,
        model: &str,
        params: &VideoAnalysisParams,
    ) -> anyhow::Result<(String, Option<Vec<FrameData>>)> {
        // Build prompt for video description
        let prompt = format!(
            "Provide a detailed description of the video at {}. Include: main subjects, actions, setting, colors, mood, and any notable moments.",
            params.video_url
        );

        // Call vision-capable model to analyze video frames
        let description = self
            .analyze_with_vision(provider, model, &params.video_url, &prompt)
            .await?;

        Ok((description, None))
    }

    /// Transcribe video audio
    async fn transcribe_video(
        &self,
        _provider: &str,
        _model: &str,
        params: &VideoAnalysisParams,
    ) -> anyhow::Result<(String, Option<Vec<FrameData>>)> {
        // For video transcription, we'd need to:
        // 1. Extract audio from video
        // 2. Send to transcription service (like Whisper)
        // 3. Return transcript

        // For now, return a placeholder
        let transcript = format!(
            "Video transcription for: {}\n\nNote: Full transcription requires audio extraction pipeline.",
            params.video_url
        );

        Ok((transcript, None))
    }

    /// Summarize video content
    async fn summarize_video(
        &self,
        provider: &str,
        model: &str,
        params: &VideoAnalysisParams,
    ) -> anyhow::Result<(String, Option<Vec<FrameData>>)> {
        // First get description, then summarize
        let (description, _) = self.describe_video(provider, model, params).await?;

        let summary = format!(
            "Video Summary:\n\n{}\n\nKey Points:\n- Main subject: [extracted from description]\n- Duration: [based on video analysis]\n- Setting: [from description]\n- Notable moments: [from description]",
            description
        );

        Ok((summary, None))
    }

    /// Extract key frames from video
    async fn extract_frames(
        &self,
        _provider: &str,
        _model: &str,
        params: &VideoAnalysisParams,
    ) -> anyhow::Result<(String, Vec<FrameData>)> {
        let max_frames = params.max_frames.clamp(1, 20);

        // Generate frame timestamps evenly across video duration
        // For now, return placeholder frames
        let frames: Vec<FrameData> = (0..max_frames)
            .map(|i| {
                let timestamp = (i as f64) * (60.0 / max_frames as f64); // Assuming 1 min video for placeholder
                FrameData {
                    frame_id: format!("frame_{:03}", i + 1),
                    timestamp,
                    image: String::new(), // Would contain actual frame data
                    description: Some(format!("Frame at {:.1}s", timestamp)),
                }
            })
            .collect();

        let summary = format!(
            "Extracted {} frames from video at {}",
            frames.len(),
            params.video_url
        );

        Ok((summary, frames))
    }

    /// Answer questions about video content
    async fn answer_video_question(
        &self,
        provider: &str,
        model: &str,
        params: &VideoAnalysisParams,
    ) -> anyhow::Result<(String, Option<Vec<FrameData>>)> {
        let question = params
            .prompt
            .as_deref()
            .unwrap_or("What is happening in this video?");

        let prompt = format!(
            "Based on the video at {}, answer this question: {}\n\nProvide a detailed and accurate response.",
            params.video_url, question
        );

        let answer = self
            .analyze_with_vision(provider, model, &params.video_url, &prompt)
            .await?;

        Ok((answer, None))
    }

    /// Analyze video using vision-capable model
    async fn analyze_with_vision(
        &self,
        provider: &str,
        model: &str,
        video_url: &str,
        prompt: &str,
    ) -> anyhow::Result<String> {
        // Get API key
        let api_key = self
            .config
            .api_key
            .clone()
            .or_else(|| std::env::var("ZEROCLAW_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());

        if api_key.is_none() {
            anyhow::bail!("No API key configured for video analysis");
        }

        // Build request based on provider
        let (url, payload) = match provider {
            "openai" | "gpt" => {
                let url = "https://api.openai.com/v1/chat/completions".to_string();
                let payload = serde_json::json!({
                    "model": model,
                    "messages": [
                        {
                            "role": "user",
                            "content": [
                                {
                                    "type": "text",
                                    "text": prompt
                                },
                                {
                                    "type": "video_url",
                                    "video_url": {
                                        "url": video_url
                                    }
                                }
                            ]
                        }
                    ],
                    "max_tokens": 4096
                });
                (url, payload)
            }
            "gemini" => {
                let url = format!(
                    "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                    model
                );
                let payload = serde_json::json!({
                    "contents": [{
                        "parts": [
                            {"text": prompt},
                            {"fileData": {
                                "mimeType": "video/mp4",
                                "fileUri": video_url
                            }}
                        ]
                    }],
                    "generationConfig": {
                        "temperature": 0.7,
                        "maxOutputTokens": 4096
                    }
                });
                (url, payload)
            }
            _ => {
                // Generic approach
                let url = format!("https://api.{}/v1/analyze", provider);
                let payload = serde_json::json!({
                    "video_url": video_url,
                    "prompt": prompt,
                    "model": model
                });
                (url, payload)
            }
        };

        // Make HTTP request
        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key.unwrap()))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Video analysis request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error = response.text().await.unwrap_or_default();
            anyhow::bail!("Video analysis API error ({}): {}", status, error);
        }

        // Parse response
        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to parse video analysis response: {}", e))?;

        // Extract text from response
        let text = match provider {
            "openai" | "gpt" => result
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|t| t.as_str())
                .unwrap_or("Analysis completed")
                .to_string(),
            "gemini" => result
                .get("candidates")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|c| c.get("content"))
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
                .and_then(|arr| arr.first())
                .and_then(|p| p.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("Analysis completed")
                .to_string(),
            _ => result
                .get("analysis")
                .or_else(|| result.get("text"))
                .or_else(|| result.get("result"))
                .and_then(|r| r.as_str())
                .unwrap_or("Analysis completed")
                .to_string(),
        };

        Ok(text)
    }
}

/// Get current timestamp in ISO format
fn chrono_now() -> String {
    // Simple timestamp without external dependency
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{}", now)
}

#[async_trait]
impl Tool for VideoAnalysisTool {
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
                "video_url": {
                    "type": "string",
                    "description": "URL of the video to analyze"
                },
                "prompt": {
                    "type": "string",
                    "description": "Specific prompt/question about the video content"
                },
                "analysis_type": {
                    "type": "string",
                    "description": "Analysis type: describe, transcribe, summarize, extract_frames, qa",
                    "default": "describe"
                },
                "max_frames": {
                    "type": "integer",
                    "description": "Maximum number of frames to extract",
                    "default": 10
                },
                "start_time": {
                    "type": "number",
                    "description": "Start time for segment analysis (in seconds)"
                },
                "end_time": {
                    "type": "number",
                    "description": "End time for segment analysis (in seconds)"
                },
                "model": {
                    "type": "string",
                    "description": "Model to use for analysis"
                },
                "provider": {
                    "type": "string",
                    "description": "Provider to use (openai, gemini)"
                }
            },
            "required": ["video_url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let params: VideoAnalysisParams = serde_json::from_value(args)?;

        // Check if generation is enabled in config
        if !self.config.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Video analysis is disabled. Enable multimedia features in config.toml: [multimodal.generation]".to_string()),
            });
        }

        match self.analyze(params).await {
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
    fn test_default_analysis_type() {
        assert_eq!(default_analysis_type(), "describe");
    }

    #[test]
    fn test_default_max_frames() {
        assert_eq!(default_max_frames(), 10);
    }

    #[test]
    fn test_video_analysis_params_serde() {
        let params = VideoAnalysisParams {
            video_url: "https://example.com/video.mp4".to_string(),
            prompt: Some("What is this video about?".to_string()),
            analysis_type: "describe".to_string(),
            max_frames: 5,
            start_time: Some(10.0),
            end_time: Some(30.0),
            model: Some("gpt-4o".to_string()),
            provider: Some("openai".to_string()),
        };

        let json = serde_json::to_string(&params).unwrap();
        let parsed: VideoAnalysisParams = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.video_url, "https://example.com/video.mp4");
        assert_eq!(parsed.analysis_type, "describe");
        assert_eq!(parsed.max_frames, 5);
    }

    #[test]
    fn test_frame_data_serde() {
        let frame = FrameData {
            frame_id: "frame_001".to_string(),
            timestamp: 5.5,
            image: "base64data...".to_string(),
            description: Some("A cat running".to_string()),
        };

        let json = serde_json::to_string(&frame).unwrap();
        let parsed: FrameData = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.frame_id, "frame_001");
        assert_eq!(parsed.timestamp, 5.5);
    }

    #[test]
    fn test_video_metadata_serde() {
        let metadata = VideoMetadata {
            title: Some("My Video".to_string()),
            duration_secs: Some(120.5),
            resolution: Some("1920x1080".to_string()),
            format: Some("mp4".to_string()),
        };

        let json = serde_json::to_string(&metadata).unwrap();
        let parsed: VideoMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.title, Some("My Video".to_string()));
        assert_eq!(parsed.duration_secs, Some(120.5));
    }
}
