// Speech Synthesis (TTS) Tool for ZeroClaw
// Provides text-to-speech capabilities via configured providers

use async_trait::async_trait;
use super::traits::{Tool, ToolResult};
use crate::config::schema::MultimodalGenerationConfig;
use serde::{Deserialize, Serialize};

/// TTS tool name
pub const TOOL_NAME: &str = "speech_synthesis";

/// TTS tool description
pub const TOOL_DESCRIPTION: &str = "Convert text to speech using AI-powered text-to-speech providers";

/// Parameters for TTS tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechSynthesisParams {
    /// The text to convert to speech
    pub text: String,

    /// Voice ID to use (e.g., "alloy", "echo", "fable", "onyx", "nova", "shimmer" for OpenAI)
    #[serde(default)]
    pub voice: Option<String>,

    /// Model to use for TTS (overrides config default)
    #[serde(default)]
    pub model: Option<String>,

    /// Provider to use (overrides config default)
    #[serde(default)]
    pub provider: Option<String>,

    /// Output format ("mp3", "opus", "aac", "flac", "wav")
    #[serde(default)]
    pub format: Option<String>,

    /// Speech speed (0.25 to 4.0)
    #[serde(default = "default_speed")]
    pub speed: f64,

    /// Optional filename for saving the audio
    #[serde(default)]
    pub output_file: Option<String>,
}

fn default_speed() -> f64 {
    1.0
}

/// TTS response containing audio data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechSynthesisResponse {
    /// Base64 encoded audio data
    pub audio: String,
    /// Audio format (mp3, opus, etc.)
    pub format: String,
    /// Provider used
    pub provider: String,
    /// Model used
    pub model: String,
    /// Voice used
    pub voice: String,
    /// Duration in seconds
    pub duration_secs: Option<f64>,
}

/// Speech Synthesis Tool
pub struct SpeechSynthesisTool {
    config: MultimodalGenerationConfig,
}

impl SpeechSynthesisTool {
    /// Create a new TTS tool instance
    pub fn new(config: MultimodalGenerationConfig) -> Self {
        Self { config }
    }

    /// Synthesize speech from text
    pub async fn synthesize(&self, params: SpeechSynthesisParams) -> anyhow::Result<SpeechSynthesisResponse> {
        // Determine provider and model to use
        let provider_name = params
            .provider
            .as_deref()
            .or(self.config.default_tts_provider.as_deref())
            .unwrap_or("openai");

        let model = params
            .model
            .as_deref()
            .or(self.config.default_tts_model.as_deref())
            .unwrap_or("tts-1");

        let voice = params
            .voice
            .as_deref()
            .or(self.config.default_voice.as_deref())
            .unwrap_or("alloy");

        let format = params
            .format
            .as_deref()
            .unwrap_or("mp3");

        let speed = params.speed.clamp(0.25, 4.0);

        // Build provider-specific payload
        let payload = self.build_provider_payload(provider_name, model, voice, format, speed, &params.text)?;

        // Call the provider
        let audio_data = self
            .call_provider(provider_name, model, voice, format, payload)
            .await?;

        Ok(SpeechSynthesisResponse {
            audio: audio_data,
            format: format.to_string(),
            provider: provider_name.to_string(),
            model: model.to_string(),
            voice: voice.to_string(),
            duration_secs: None, // Could be calculated from audio length
        })
    }

    /// Build provider-specific request payload
    fn build_provider_payload(
        &self,
        provider: &str,
        model: &str,
        voice: &str,
        format: &str,
        speed: f64,
        text: &str,
    ) -> anyhow::Result<serde_json::Value> {
        match provider {
            "openai" => Ok(serde_json::json!({
                "model": model,
                "input": text,
                "voice": voice,
                "response_format": format,
                "speed": speed
            })),
            "elevenlabs" => Ok(serde_json::json!({
                "text": text,
                "voice_id": voice,
                "model_id": model,
                "output_format": self.format_to_elevenlabs(format)
            })),
            "coqui" => Ok(serde_json::json!({
                "text": text,
                "voice_id": voice,
                "speed": speed,
                "format": "wav"
            })),
            "gemini" => Ok(serde_json::json!({
                "model": model,
                "input": {
                    "text": text
                },
                "voice_config": {
                    "prebuilt_voice_config": {
                        "voice_name": voice
                    }
                },
                "audio_config": {
                    "audio_encoding": self.format_to_google(format)
                }
            })),
            _ => Ok(serde_json::json!({
                "model": model,
                "text": text,
                "voice": voice,
                "format": format,
                "speed": speed
            })),
        }
    }

    /// Convert format to ElevenLabs format
    fn format_to_elevenlabs(&self, format: &str) -> String {
        match format {
            "mp3" => "mp3_44100_128".to_string(),
            "opus" => "opus_48000".to_string(),
            "aac" => "aac_44100".to_string(),
            "flac" => "flac_44100".to_string(),
            _ => "mp3_44100_128".to_string(),
        }
    }

    /// Convert format to Google (Gemini) audio encoding
    fn format_to_google(&self, format: &str) -> String {
        match format {
            "mp3" => "MP3".to_string(),
            "wav" => "LINEAR16".to_string(),
            "aac" => "AAC".to_string(),
            "ogg" => "OGG_OPUS".to_string(),
            _ => "MP3".to_string(),
        }
    }

    /// Call the TTS provider API
    async fn call_provider(
        &self,
        provider: &str,
        model: &str,
        voice: &str,
        _format: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<String> {
        // Get API key from config or environment
        let api_key = self
            .config
            .api_key
            .clone()
            .or_else(|| std::env::var("ZEROCLAW_API_KEY").ok())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok());

        if api_key.is_none() {
            anyhow::bail!("No API key configured for TTS");
        }

        // Build request URL based on provider
        let (url, extra_headers) = match provider {
            "openai" => (
                "https://api.openai.com/v1/audio/speech".to_string(),
                vec![("OpenAI-Beta", "assistants=v2")],
            ),
            "elevenlabs" => (
                format!(
                    "https://api.elevenlabs.io/v1/text-to-speech/{}",
                    voice
                ),
                vec![],
            ),
            "coqui" => (
                "https://api.coqui.ai/v2/tts".to_string(),
                vec![],
            ),
            "gemini" => (
                format!(
                    "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                    model
                ),
                vec![],
            ),
            _ => return Err(anyhow::anyhow!("Unsupported TTS provider: {}", provider)),
        };

        // Make the HTTP request
        let client = reqwest::Client::new();
        let mut request = client.post(&url);

        // Add authentication
        request = request.header("Authorization", format!("Bearer {}", api_key.unwrap()));

        // Add provider-specific headers
        for (key, value) in extra_headers {
            request = request.header(key, value);
        }

        // Provider-specific request building
        match provider {
            "openai" | "elevenlabs" | "gemini" | "_" => {
                request = request.header("Content-Type", "application/json");
            }
            _ => {
                request = request.header("Content-Type", "application/json");
            }
        }

        // Send request
        let response = request
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to call TTS API: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("TTS API error ({}): {}", status, error_body);
        }

        // Get audio data
        let bytes = response
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read TTS audio data: {}", e))?;

        // Encode to base64
        let audio_base64 = base64_encode(&bytes);

        Ok(audio_base64)
    }
}

/// Simple base64 encoder (no external dependency)
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
        
        result.push(CHARS[b0 >> 2] as char);
        result.push(CHARS[((b0 & 0x03) << 4) | (b1 >> 4)] as char);
        
        if chunk.len() > 1 {
            result.push(CHARS[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
        } else {
            result.push('=');
        }
        
        if chunk.len() > 2 {
            result.push(CHARS[b2 & 0x3f] as char);
        } else {
            result.push('=');
        }
    }
    
    result
}

#[async_trait]
impl Tool for SpeechSynthesisTool {
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
                "text": {
                    "type": "string",
                    "description": "The text to convert to speech"
                },
                "voice": {
                    "type": "string",
                    "description": "Voice ID (alloy, echo, fable, onyx, nova, shimmer)"
                },
                "model": {
                    "type": "string",
                    "description": "Model to use for TTS"
                },
                "provider": {
                    "type": "string",
                    "description": "Provider to use (openai, elevenlabs, coqui, gemini)"
                },
                "format": {
                    "type": "string",
                    "description": "Output format (mp3, opus, aac, flac, wav)",
                    "default": "mp3"
                },
                "speed": {
                    "type": "number",
                    "description": "Speech speed (0.25 to 4.0)",
                    "default": 1.0
                },
                "output_file": {
                    "type": "string",
                    "description": "Optional filename for saving the audio"
                }
            },
            "required": ["text"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let params: SpeechSynthesisParams = serde_json::from_value(args)?;

        // Check if generation is enabled in config
        if !self.config.enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Speech synthesis is disabled. Enable it in config.toml: [multimodal.generation]".to_string()),
            });
        }

        match self.synthesize(params).await {
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
    fn test_default_speed() {
        assert_eq!(default_speed(), 1.0);
    }

    #[test]
    fn test_format_to_elevenlabs() {
        let tool = SpeechSynthesisTool::new(MultimodalGenerationConfig::default());
        
        assert_eq!(tool.format_to_elevenlabs("mp3"), "mp3_44100_128");
        assert_eq!(tool.format_to_elevenlabs("opus"), "opus_48000");
        assert_eq!(tool.format_to_elevenlabs("aac"), "aac_44100");
        assert_eq!(tool.format_to_elevenlabs("flac"), "flac_44100");
    }

    #[test]
    fn test_format_to_google() {
        let tool = SpeechSynthesisTool::new(MultimodalGenerationConfig::default());
        
        assert_eq!(tool.format_to_google("mp3"), "MP3");
        assert_eq!(tool.format_to_google("wav"), "LINEAR16");
        assert_eq!(tool.format_to_google("aac"), "AAC");
        assert_eq!(tool.format_to_google("ogg"), "OGG_OPUS");
    }

    #[test]
    fn test_speech_synthesis_params_serde() {
        let params = SpeechSynthesisParams {
            text: "Hello, world!".to_string(),
            voice: Some("alloy".to_string()),
            model: Some("tts-1".to_string()),
            provider: Some("openai".to_string()),
            format: Some("mp3".to_string()),
            speed: 1.0,
            output_file: Some("hello.mp3".to_string()),
        };

        let json = serde_json::to_string(&params).unwrap();
        let parsed: SpeechSynthesisParams = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.text, "Hello, world!");
        assert_eq!(parsed.voice, Some("alloy".to_string()));
        assert_eq!(parsed.speed, 1.0);
    }

    #[test]
    fn test_base64_encode() {
        // Test vector from RFC 4648
        assert_eq!(base64_encode(b"".as_slice()), "");
        assert_eq!(base64_encode(b"f".as_slice()), "Zg==");
        assert_eq!(base64_encode(b"fo".as_slice()), "Zm8=");
        assert_eq!(base64_encode(b"foo".as_slice()), "Zm9v");
        assert_eq!(base64_encode(b"foob".as_slice()), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba".as_slice()), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar".as_slice()), "Zm9vYmFy");
    }
}
