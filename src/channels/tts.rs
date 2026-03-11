//! Multi-provider Text-to-Speech (TTS) subsystem.
//!
//! Supports OpenAI, ElevenLabs, Google Cloud TTS, and Edge TTS (free, subprocess-based).
//! Provider selection is driven by [`TtsConfig`] in `config.toml`.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::config::TtsConfig;

/// Maximum text length before synthesis is rejected (default: 4096 chars).
const DEFAULT_MAX_TEXT_LENGTH: usize = 4096;

/// Default HTTP request timeout for TTS API calls.
const TTS_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

// ── TtsProvider trait ────────────────────────────────────────────

/// Trait for pluggable TTS backends.
#[async_trait::async_trait]
pub trait TtsProvider: Send + Sync {
    /// Provider identifier (e.g. `"openai"`, `"elevenlabs"`).
    fn name(&self) -> &str;

    /// Synthesize `text` using the given `voice`, returning raw audio bytes.
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>>;

    /// Voices supported by this provider.
    fn supported_voices(&self) -> Vec<String>;

    /// Audio output formats supported by this provider.
    fn supported_formats(&self) -> Vec<String>;
}

// ── OpenAI TTS ───────────────────────────────────────────────────

/// OpenAI TTS provider (`POST /v1/audio/speech`).
pub struct OpenAiTtsProvider {
    api_key: String,
    model: String,
    speed: f64,
    client: reqwest::Client,
}

impl OpenAiTtsProvider {
    /// Create a new OpenAI TTS provider from config, resolving the API key
    /// from config or `OPENAI_API_KEY` env var.
    pub fn new(config: &crate::config::OpenAiTtsConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                std::env::var("OPENAI_API_KEY")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .context("Missing OpenAI TTS API key: set [tts.openai].api_key or OPENAI_API_KEY")?;

        Ok(Self {
            api_key,
            model: config.model.clone(),
            speed: config.speed,
            client: reqwest::Client::builder()
                .timeout(TTS_HTTP_TIMEOUT)
                .build()
                .context("Failed to build HTTP client for OpenAI TTS")?,
        })
    }
}

#[async_trait::async_trait]
impl TtsProvider for OpenAiTtsProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
            "voice": voice,
            "speed": self.speed,
        });

        let resp = self
            .client
            .post("https://api.openai.com/v1/audio/speech")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to send OpenAI TTS request")?;

        let status = resp.status();
        if !status.is_success() {
            let error_body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or_else(|_| serde_json::json!({"error": "unknown"}));
            let msg = error_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("OpenAI TTS API error ({}): {}", status, msg);
        }

        let bytes = resp
            .bytes()
            .await
            .context("Failed to read OpenAI TTS response body")?;
        Ok(bytes.to_vec())
    }

    fn supported_voices(&self) -> Vec<String> {
        ["alloy", "echo", "fable", "onyx", "nova", "shimmer"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    fn supported_formats(&self) -> Vec<String> {
        ["mp3", "opus", "aac", "flac", "wav", "pcm"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
}

// ── ElevenLabs TTS ───────────────────────────────────────────────

/// ElevenLabs TTS provider (`POST /v1/text-to-speech/{voice_id}`).
pub struct ElevenLabsTtsProvider {
    api_key: String,
    model_id: String,
    stability: f64,
    similarity_boost: f64,
    client: reqwest::Client,
}

impl ElevenLabsTtsProvider {
    /// Create a new ElevenLabs TTS provider from config, resolving the API key
    /// from config or `ELEVENLABS_API_KEY` env var.
    pub fn new(config: &crate::config::ElevenLabsTtsConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                std::env::var("ELEVENLABS_API_KEY")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .context(
                "Missing ElevenLabs API key: set [tts.elevenlabs].api_key or ELEVENLABS_API_KEY",
            )?;

        Ok(Self {
            api_key,
            model_id: config.model_id.clone(),
            stability: config.stability,
            similarity_boost: config.similarity_boost,
            client: reqwest::Client::builder()
                .timeout(TTS_HTTP_TIMEOUT)
                .build()
                .context("Failed to build HTTP client for ElevenLabs TTS")?,
        })
    }
}

#[async_trait::async_trait]
impl TtsProvider for ElevenLabsTtsProvider {
    fn name(&self) -> &str {
        "elevenlabs"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}");
        let body = serde_json::json!({
            "text": text,
            "model_id": self.model_id,
            "voice_settings": {
                "stability": self.stability,
                "similarity_boost": self.similarity_boost,
            },
        });

        let resp = self
            .client
            .post(&url)
            .header("xi-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("Failed to send ElevenLabs TTS request")?;

        let status = resp.status();
        if !status.is_success() {
            let error_body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or_else(|_| serde_json::json!({"error": "unknown"}));
            let msg = error_body["detail"]["message"]
                .as_str()
                .or_else(|| error_body["detail"].as_str())
                .unwrap_or("unknown error");
            bail!("ElevenLabs TTS API error ({}): {}", status, msg);
        }

        let bytes = resp
            .bytes()
            .await
            .context("Failed to read ElevenLabs TTS response body")?;
        Ok(bytes.to_vec())
    }

    fn supported_voices(&self) -> Vec<String> {
        // ElevenLabs voices are user-specific; return empty (dynamic lookup).
        Vec::new()
    }

    fn supported_formats(&self) -> Vec<String> {
        ["mp3", "pcm", "ulaw"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
}

// ── Google Cloud TTS ─────────────────────────────────────────────

/// Google Cloud TTS provider (`POST /v1/text:synthesize`).
pub struct GoogleTtsProvider {
    api_key: String,
    language_code: String,
    client: reqwest::Client,
}

impl GoogleTtsProvider {
    /// Create a new Google Cloud TTS provider from config, resolving the API key
    /// from config or `GOOGLE_TTS_API_KEY` env var.
    pub fn new(config: &crate::config::GoogleTtsConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                std::env::var("GOOGLE_TTS_API_KEY")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .context(
                "Missing Google TTS API key: set [tts.google].api_key or GOOGLE_TTS_API_KEY",
            )?;

        Ok(Self {
            api_key,
            language_code: config.language_code.clone(),
            client: reqwest::Client::builder()
                .timeout(TTS_HTTP_TIMEOUT)
                .build()
                .context("Failed to build HTTP client for Google TTS")?,
        })
    }
}

#[async_trait::async_trait]
impl TtsProvider for GoogleTtsProvider {
    fn name(&self) -> &str {
        "google"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let url = format!(
            "https://texttospeech.googleapis.com/v1/text:synthesize?key={}",
            self.api_key
        );
        let body = serde_json::json!({
            "input": { "text": text },
            "voice": {
                "languageCode": self.language_code,
                "name": voice,
            },
            "audioConfig": {
                "audioEncoding": "MP3",
            },
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Failed to send Google TTS request")?;

        let status = resp.status();
        let resp_body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse Google TTS response")?;

        if !status.is_success() {
            let msg = resp_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("Google TTS API error ({}): {}", status, msg);
        }

        let audio_b64 = resp_body["audioContent"]
            .as_str()
            .context("Google TTS response missing 'audioContent' field")?;

        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(audio_b64)
            .context("Failed to decode Google TTS base64 audio")?;
        Ok(bytes)
    }

    fn supported_voices(&self) -> Vec<String> {
        // Google voices vary by language; return common English defaults.
        [
            "en-US-Standard-A",
            "en-US-Standard-B",
            "en-US-Standard-C",
            "en-US-Standard-D",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
    }

    fn supported_formats(&self) -> Vec<String> {
        ["mp3", "wav", "ogg"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
}

// ── Edge TTS (subprocess) ────────────────────────────────────────

/// Edge TTS provider — free, uses the `edge-tts` CLI subprocess.
pub struct EdgeTtsProvider {
    binary_path: String,
}

impl EdgeTtsProvider {
    /// Create a new Edge TTS provider from config.
    pub fn new(config: &crate::config::EdgeTtsConfig) -> Self {
        Self {
            binary_path: config.binary_path.clone(),
        }
    }
}

#[async_trait::async_trait]
impl TtsProvider for EdgeTtsProvider {
    fn name(&self) -> &str {
        "edge"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let temp_dir = std::env::temp_dir();
        let output_file = temp_dir.join(format!("zeroclaw_tts_{}.mp3", uuid::Uuid::new_v4()));
        let output_path = output_file
            .to_str()
            .context("Failed to build temp file path for Edge TTS")?;

        let output = tokio::process::Command::new(&self.binary_path)
            .arg("--text")
            .arg(text)
            .arg("--voice")
            .arg(voice)
            .arg("--write-media")
            .arg(output_path)
            .output()
            .await
            .context("Failed to spawn edge-tts subprocess")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Clean up temp file on failure.
            let _ = tokio::fs::remove_file(&output_file).await;
            bail!("edge-tts failed (exit {}): {}", output.status, stderr);
        }

        let bytes = tokio::fs::read(&output_file)
            .await
            .context("Failed to read edge-tts output file")?;

        // Clean up temp file.
        let _ = tokio::fs::remove_file(&output_file).await;

        Ok(bytes)
    }

    fn supported_voices(&self) -> Vec<String> {
        // Edge TTS has many voices; return common defaults.
        [
            "en-US-AriaNeural",
            "en-US-GuyNeural",
            "en-US-JennyNeural",
            "en-GB-SoniaNeural",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
    }

    fn supported_formats(&self) -> Vec<String> {
        vec!["mp3".to_string()]
    }
}

// ── TtsManager ───────────────────────────────────────────────────

/// Central manager for multi-provider TTS synthesis.
pub struct TtsManager {
    providers: HashMap<String, Box<dyn TtsProvider>>,
    default_provider: String,
    default_voice: String,
    max_text_length: usize,
}

impl TtsManager {
    /// Build a `TtsManager` from config, initializing all configured providers.
    pub fn new(config: &TtsConfig) -> Result<Self> {
        let mut providers: HashMap<String, Box<dyn TtsProvider>> = HashMap::new();

        if let Some(ref openai_cfg) = config.openai {
            match OpenAiTtsProvider::new(openai_cfg) {
                Ok(p) => {
                    providers.insert("openai".to_string(), Box::new(p));
                }
                Err(e) => {
                    tracing::warn!("Skipping OpenAI TTS provider: {e}");
                }
            }
        }

        if let Some(ref elevenlabs_cfg) = config.elevenlabs {
            match ElevenLabsTtsProvider::new(elevenlabs_cfg) {
                Ok(p) => {
                    providers.insert("elevenlabs".to_string(), Box::new(p));
                }
                Err(e) => {
                    tracing::warn!("Skipping ElevenLabs TTS provider: {e}");
                }
            }
        }

        if let Some(ref google_cfg) = config.google {
            match GoogleTtsProvider::new(google_cfg) {
                Ok(p) => {
                    providers.insert("google".to_string(), Box::new(p));
                }
                Err(e) => {
                    tracing::warn!("Skipping Google TTS provider: {e}");
                }
            }
        }

        if let Some(ref edge_cfg) = config.edge {
            let p = EdgeTtsProvider::new(edge_cfg);
            providers.insert("edge".to_string(), Box::new(p));
        }

        let max_text_length = if config.max_text_length == 0 {
            DEFAULT_MAX_TEXT_LENGTH
        } else {
            config.max_text_length
        };

        Ok(Self {
            providers,
            default_provider: config.default_provider.clone(),
            default_voice: config.default_voice.clone(),
            max_text_length,
        })
    }

    /// Synthesize text using the default provider and voice.
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        self.synthesize_with_provider(text, &self.default_provider, &self.default_voice)
            .await
    }

    /// Synthesize text using a specific provider and voice.
    pub async fn synthesize_with_provider(
        &self,
        text: &str,
        provider: &str,
        voice: &str,
    ) -> Result<Vec<u8>> {
        if text.is_empty() {
            bail!("TTS text must not be empty");
        }
        let char_count = text.chars().count();
        if char_count > self.max_text_length {
            bail!(
                "TTS text too long ({} chars, max {})",
                char_count,
                self.max_text_length
            );
        }

        let tts = self.providers.get(provider).ok_or_else(|| {
            anyhow::anyhow!(
                "TTS provider '{}' not configured (available: {})",
                provider,
                self.available_providers().join(", ")
            )
        })?;

        tts.synthesize(text, voice).await
    }

    /// List names of all initialized providers.
    pub fn available_providers(&self) -> Vec<String> {
        let mut names: Vec<_> = self.providers.keys().cloned().collect();
        names.sort();
        names
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_tts_config() -> TtsConfig {
        TtsConfig::default()
    }

    #[test]
    fn tts_manager_creation_with_defaults() {
        let config = default_tts_config();
        let manager = TtsManager::new(&config).unwrap();
        // No providers configured by default, so list is empty.
        assert!(manager.available_providers().is_empty());
    }

    #[test]
    fn tts_manager_with_edge_provider() {
        let mut config = default_tts_config();
        config.default_provider = "edge".to_string();
        config.edge = Some(crate::config::EdgeTtsConfig {
            binary_path: "edge-tts".to_string(),
        });

        let manager = TtsManager::new(&config).unwrap();
        assert_eq!(manager.available_providers(), vec!["edge"]);
    }

    #[tokio::test]
    async fn tts_rejects_empty_text() {
        let mut config = default_tts_config();
        config.default_provider = "edge".to_string();
        config.edge = Some(crate::config::EdgeTtsConfig {
            binary_path: "edge-tts".to_string(),
        });

        let manager = TtsManager::new(&config).unwrap();
        let err = manager
            .synthesize_with_provider("", "edge", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "expected empty-text error, got: {err}"
        );
    }

    #[tokio::test]
    async fn tts_rejects_text_exceeding_max_length() {
        let mut config = default_tts_config();
        config.default_provider = "edge".to_string();
        config.max_text_length = 10;
        config.edge = Some(crate::config::EdgeTtsConfig {
            binary_path: "edge-tts".to_string(),
        });

        let manager = TtsManager::new(&config).unwrap();
        let long_text = "a".repeat(11);
        let err = manager
            .synthesize_with_provider(&long_text, "edge", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("too long"),
            "expected too-long error, got: {err}"
        );
    }

    #[tokio::test]
    async fn tts_rejects_unknown_provider() {
        let config = default_tts_config();
        let manager = TtsManager::new(&config).unwrap();
        let err = manager
            .synthesize_with_provider("hello", "nonexistent", "voice")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not configured"),
            "expected not-configured error, got: {err}"
        );
    }

    #[test]
    fn tts_config_defaults() {
        let config = TtsConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.default_provider, "openai");
        assert_eq!(config.default_voice, "alloy");
        assert_eq!(config.default_format, "mp3");
        assert_eq!(config.max_text_length, DEFAULT_MAX_TEXT_LENGTH);
        assert!(config.openai.is_none());
        assert!(config.elevenlabs.is_none());
        assert!(config.google.is_none());
        assert!(config.edge.is_none());
    }

    #[test]
    fn tts_manager_max_text_length_zero_uses_default() {
        let mut config = default_tts_config();
        config.max_text_length = 0;
        let manager = TtsManager::new(&config).unwrap();
        assert_eq!(manager.max_text_length, DEFAULT_MAX_TEXT_LENGTH);
    }
}
