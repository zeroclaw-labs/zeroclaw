//! Multi-provider Text-to-Speech (TTS) subsystem.
//!
//! Supports OpenAI, ElevenLabs, Google Cloud TTS, Edge TTS (free, subprocess-based),
//! and Piper TTS (local GPU-accelerated, OpenAI-compatible endpoint).
//!
//! per-instance configs live under `[providers.tts.<type>.<alias>]`; agents
//! pick which instance to use via the `tts_provider` dotted alias reference.
//! Global runtime knobs (default_voice, max_text_length, etc.) live on `[tts]`.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use zeroclaw_config::schema::{Config, TtsProviderConfig};

/// Maximum text length before synthesis is rejected (default: 4096 chars).
const DEFAULT_MAX_TEXT_LENGTH: usize = 4096;

/// Default HTTP request timeout for TTS API calls.
const TTS_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

// ── TtsProvider trait ────────────────────────────────────────────

/// Trait for pluggable TTS backends.
#[async_trait::async_trait]
pub trait TtsProvider: Send + Sync {
    /// ModelProvider identifier (e.g. `"openai"`, `"elevenlabs"`).
    fn name(&self) -> &str;

    /// Synthesize `text` using the given `voice`, returning raw audio bytes.
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>>;

    /// Voices supported by this model_provider.
    fn supported_voices(&self) -> Vec<String>;

    /// Audio output formats supported by this model_provider.
    fn supported_formats(&self) -> Vec<String>;
}

// ── OpenAI TTS ───────────────────────────────────────────────────

/// OpenAI TTS model_provider (`POST /v1/audio/speech`).
pub struct OpenAiTtsProvider {
    api_key: String,
    model: String,
    speed: f64,
    client: reqwest::Client,
}

impl OpenAiTtsProvider {
    /// Create a new OpenAI TTS model_provider from config, resolving the API key
    /// from config or `OPENAI_API_KEY` env var.
    pub fn new(config: &TtsProviderConfig) -> Result<Self> {
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
            .context(
                "Missing OpenAI TTS API key: set [providers.tts.openai.<alias>].api_key or OPENAI_API_KEY",
            )?;

        Ok(Self {
            api_key,
            model: config
                .model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "tts-1".to_string()),
            speed: config.speed.unwrap_or(1.0),
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
            "response_format": "opus",
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

/// ElevenLabs TTS model_provider (`POST /v1/text-to-speech/{voice_id}`).
pub struct ElevenLabsTtsProvider {
    api_key: String,
    model_id: String,
    stability: f64,
    similarity_boost: f64,
    client: reqwest::Client,
}

impl ElevenLabsTtsProvider {
    /// Create a new ElevenLabs TTS model_provider from config, resolving the API key
    /// from config or `ELEVENLABS_API_KEY` env var.
    pub fn new(config: &TtsProviderConfig) -> Result<Self> {
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
                "Missing ElevenLabs API key: set [providers.tts.elevenlabs.<alias>].api_key or ELEVENLABS_API_KEY",
            )?;

        Ok(Self {
            api_key,
            model_id: config
                .model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "eleven_monolingual_v1".to_string()),
            stability: config.stability.unwrap_or(0.5),
            similarity_boost: config.similarity_boost.unwrap_or(0.5),
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
        if !voice
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("ElevenLabs voice ID contains invalid characters: {voice}");
        }
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

/// Google Cloud TTS model_provider (`POST /v1/text:synthesize`).
pub struct GoogleTtsProvider {
    api_key: String,
    language_code: String,
    client: reqwest::Client,
}

impl GoogleTtsProvider {
    /// Create a new Google Cloud TTS model_provider from config, resolving the API key
    /// from config or `GOOGLE_TTS_API_KEY` env var.
    pub fn new(config: &TtsProviderConfig) -> Result<Self> {
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
                "Missing Google TTS API key: set [providers.tts.google.<alias>].api_key or GOOGLE_TTS_API_KEY",
            )?;

        Ok(Self {
            api_key,
            language_code: config
                .language_code
                .clone()
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| "en-US".to_string()),
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
        let url = "https://texttospeech.googleapis.com/v1/text:synthesize";
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
            .post(url)
            .header("x-goog-api-key", &self.api_key)
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

/// Edge TTS model_provider — free, uses the `edge-tts` CLI subprocess.
pub struct EdgeTtsProvider {
    binary_path: String,
}

impl EdgeTtsProvider {
    /// Allowed basenames for the Edge TTS binary.
    const ALLOWED_BINARIES: &[&str] = &["edge-tts", "edge-playback"];

    /// Create a new Edge TTS model_provider from config.
    ///
    /// `binary_path` must be a bare command name (no path separators) matching
    /// one of `ALLOWED_BINARIES`. This prevents arbitrary executable
    /// paths like `/tmp/malicious/edge-tts` from passing the basename check.
    pub fn new(config: &TtsProviderConfig) -> Result<Self> {
        let raw_path = config
            .binary_path
            .clone()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| "edge-tts".to_string());
        if raw_path.contains('/') || raw_path.contains('\\') {
            bail!(
                "Edge TTS binary_path must be a bare command name without path separators, got: {raw_path}"
            );
        }
        if !Self::ALLOWED_BINARIES.contains(&raw_path.as_str()) {
            bail!(
                "Edge TTS binary_path must be one of {:?}, got: {raw_path}",
                Self::ALLOWED_BINARIES,
            );
        }
        Ok(Self {
            binary_path: raw_path,
        })
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

        let output = tokio::time::timeout(
            TTS_HTTP_TIMEOUT,
            tokio::process::Command::new(&self.binary_path)
                .arg("--text")
                .arg(text)
                .arg("--voice")
                .arg(voice)
                .arg("--write-media")
                .arg(output_path)
                .output(),
        )
        .await
        .context("Edge TTS subprocess timed out")?
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

// ── Piper TTS (local, OpenAI-compatible) ─────────────────────────

/// Piper TTS model_provider — local GPU-accelerated server with an OpenAI-compatible endpoint.
pub struct PiperTtsProvider {
    client: reqwest::Client,
    api_url: String,
}

impl PiperTtsProvider {
    /// Create a new Piper TTS model_provider from config. Falls back to
    /// `http://127.0.0.1:5000/v1/audio/speech` when no `api_url` is supplied.
    pub fn new(config: &TtsProviderConfig) -> Self {
        let api_url = config
            .uri
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:5000/v1/audio/speech".to_string());
        Self {
            client: reqwest::Client::builder()
                .timeout(TTS_HTTP_TIMEOUT)
                .build()
                .expect("Failed to build HTTP client for Piper TTS"),
            api_url,
        }
    }
}

#[async_trait::async_trait]
impl TtsProvider for PiperTtsProvider {
    fn name(&self) -> &str {
        "piper"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let body = serde_json::json!({
            "model": "tts-1",
            "input": text,
            "voice": voice,
        });

        let resp = self
            .client
            .post(&self.api_url)
            .json(&body)
            .send()
            .await
            .context("Failed to send Piper TTS request")?;

        let status = resp.status();
        if !status.is_success() {
            let error_body: serde_json::Value = resp
                .json()
                .await
                .unwrap_or_else(|_| serde_json::json!({"error": "unknown"}));
            let msg = error_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("Piper TTS API error ({}): {}", status, msg);
        }

        let bytes = resp
            .bytes()
            .await
            .context("Failed to read Piper TTS response body")?;
        Ok(bytes.to_vec())
    }

    fn supported_voices(&self) -> Vec<String> {
        // Piper voices depend on installed models; return empty (dynamic).
        Vec::new()
    }

    fn supported_formats(&self) -> Vec<String> {
        ["mp3", "wav", "opus"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }
}

// ── TtsManager ───────────────────────────────────────────────────

/// Central manager for per-agent TTS synthesis.
///
/// `tts_providers` are keyed by their dotted alias (`<type>.<alias>`).
/// Per-instance voice overrides come from the `voice` field on each
/// `TtsProviderConfig`. The `agent_tts_provider` field carries the
/// resolved alias for the agent that owns this manager instance — empty
/// means the agent doesn't want TTS, and `synthesize_for_agent` fails
/// loud rather than silently pick a default.
pub struct TtsManager {
    tts_providers: HashMap<String, Box<dyn TtsProvider>>,
    voice_by_alias: HashMap<String, String>,
    /// Resolved alias for the agent that owns this manager. Empty when
    /// the agent has no TTS preference (opt-out).
    agent_tts_provider: String,
    default_voice: String,
    max_text_length: usize,
}

impl TtsManager {
    /// Build a `TtsManager` from `[providers.tts.<type>.<alias>]` instances
    /// in `Config`. Each instance is registered under its dotted alias key
    /// (`<type>.<alias>`). Failures to construct a particular instance are
    /// logged at warn but do not abort the manager.
    /// Build a `TtsManager` from `[providers.tts.<type>.<alias>]` instances.
    /// The manager's resolved alias comes from the runtime-active agent's
    /// `tts_provider` field — there is no global default-provider concept,
    /// so when no agent-bound resolution is available the manager refuses
    /// to silently pick a provider (`synthesize` fails loud).
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut tts_providers: HashMap<String, Box<dyn TtsProvider>> = HashMap::new();
        let mut voice_by_alias: HashMap<String, String> = HashMap::new();

        // Typed dispatch over the TtsProviders container's named slots. The
        // unknown-type warn-and-skip arm is gone — the typed container can't
        // hold an unrecognized family.
        for (family, alias, instance) in config.providers.tts.iter_entries() {
            let dotted = format!("{family}.{alias}");
            let result: Result<Box<dyn TtsProvider>> = match family {
                "openai" => OpenAiTtsProvider::new(instance).map(|p| Box::new(p) as _),
                "elevenlabs" => ElevenLabsTtsProvider::new(instance).map(|p| Box::new(p) as _),
                "google" => GoogleTtsProvider::new(instance).map(|p| Box::new(p) as _),
                "edge" => EdgeTtsProvider::new(instance).map(|p| Box::new(p) as _),
                "piper" => Ok(Box::new(PiperTtsProvider::new(instance)) as _),
                _ => unreachable!("TtsProviders typed slots cover all 5 families"),
            };
            match result {
                Ok(p) => {
                    tts_providers.insert(dotted.clone(), p);
                    if let Some(voice) = instance
                        .voice
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                    {
                        voice_by_alias.insert(dotted, voice.to_string());
                    }
                }
                Err(e) => {
                    tracing::warn!("Skipping TTS provider {dotted}: {e}");
                }
            }
        }

        let max_text_length = if config.tts.max_text_length == 0 {
            DEFAULT_MAX_TEXT_LENGTH
        } else {
            config.tts.max_text_length
        };

        // Per-agent join: the runtime-active agent's `tts_provider` is the
        // resolved alias for this manager instance. Empty (or no resolved
        // agent) = no TTS; `synthesize` fails loud rather than silently
        // pick a provider.
        let agent_tts_provider = config
            .resolved_runtime_agent_alias()
            .and_then(|alias| config.agents.get(alias))
            .map(|a| a.tts_provider.as_str().to_string())
            .unwrap_or_default();

        Ok(Self {
            tts_providers,
            voice_by_alias,
            agent_tts_provider,
            default_voice: config.tts.default_voice.clone(),
            max_text_length,
        })
    }

    /// Synthesize text using the runtime-active agent's resolved
    /// `tts_provider` reference and the per-instance voice override (or
    /// `default_voice` as the per-instance fallback). Fails loud when the
    /// agent has no `tts_provider` configured — there is no global
    /// default-provider concept and this manager refuses to silently pick
    /// one.
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        let provider_alias = self.agent_tts_provider.as_str();
        if provider_alias.is_empty() {
            bail!(
                "Agent has no tts_provider configured. Set \
                 `agent.<alias>.tts_provider = \"<type>.<alias>\"` referencing a \
                 [providers.tts.<type>.<alias>] entry."
            );
        }
        let voice = self
            .voice_by_alias
            .get(provider_alias)
            .map_or(self.default_voice.as_str(), String::as_str);
        self.synthesize_with_provider(text, provider_alias, voice)
            .await
    }

    /// Synthesize text using a specific dotted-alias model_provider and voice.
    pub async fn synthesize_with_provider(
        &self,
        text: &str,
        provider_alias: &str,
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

        let tts = self.tts_providers.get(provider_alias).ok_or_else(|| {
            anyhow::anyhow!(
                "TTS model_provider '{}' not configured (available: {})",
                provider_alias,
                self.available_providers().join(", ")
            )
        })?;

        tts.synthesize(text, voice).await
    }

    /// List dotted aliases of all initialized tts_providers.
    pub fn available_providers(&self) -> Vec<String> {
        let mut names: Vec<_> = self.tts_providers.keys().cloned().collect();
        names.sort();
        names
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_edge_alias() -> Config {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::DelegateAgentConfig {
                tts_provider: "edge.default".into(),
                ..Default::default()
            },
        );
        cfg.providers.tts.edge.insert(
            "default".to_string(),
            zeroclaw_config::schema::EdgeTtsProviderConfig {
                base: TtsProviderConfig {
                    binary_path: Some("edge-tts".to_string()),
                    ..TtsProviderConfig::default()
                },
            },
        );
        cfg
    }

    fn config_with_piper_alias() -> Config {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::DelegateAgentConfig {
                tts_provider: "piper.default".into(),
                ..Default::default()
            },
        );
        cfg.providers.tts.piper.insert(
            "default".to_string(),
            zeroclaw_config::schema::PiperTtsProviderConfig {
                base: TtsProviderConfig {
                    uri: Some("http://127.0.0.1:5000/v1/audio/speech".to_string()),
                    ..TtsProviderConfig::default()
                },
            },
        );
        cfg
    }

    #[test]
    fn tts_manager_creation_with_defaults() {
        let config = Config::default();
        let manager = TtsManager::from_config(&config).unwrap();
        assert!(manager.available_providers().is_empty());
    }

    #[test]
    fn tts_manager_registers_alias_keyed_provider() {
        let cfg = config_with_edge_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        assert_eq!(manager.available_providers(), vec!["edge.default"]);
    }

    #[tokio::test]
    async fn tts_rejects_empty_text() {
        let cfg = config_with_edge_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        let err = manager
            .synthesize_with_provider("", "edge.default", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "expected empty-text error, got: {err}"
        );
    }

    #[tokio::test]
    async fn tts_rejects_text_exceeding_max_length() {
        let mut cfg = config_with_edge_alias();
        cfg.tts.max_text_length = 10;
        let manager = TtsManager::from_config(&cfg).unwrap();
        let long_text = "a".repeat(11);
        let err = manager
            .synthesize_with_provider(&long_text, "edge.default", "en-US-AriaNeural")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("too long"),
            "expected too-long error, got: {err}"
        );
    }

    #[tokio::test]
    async fn tts_rejects_unknown_provider() {
        let cfg = Config::default();
        let manager = TtsManager::from_config(&cfg).unwrap();
        let err = manager
            .synthesize_with_provider("hello", "nonexistent.alias", "voice")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not configured"),
            "expected not-configured error, got: {err}"
        );
    }

    #[test]
    fn piper_provider_creation_uses_default_url_when_unset() {
        let model_provider = PiperTtsProvider::new(&TtsProviderConfig::default());
        assert_eq!(model_provider.name(), "piper");
        assert_eq!(
            model_provider.api_url,
            "http://127.0.0.1:5000/v1/audio/speech"
        );
        assert_eq!(
            model_provider.supported_formats(),
            vec!["mp3", "wav", "opus"]
        );
        assert!(model_provider.supported_voices().is_empty());
    }

    #[test]
    fn tts_manager_with_piper_alias() {
        let cfg = config_with_piper_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        assert_eq!(manager.available_providers(), vec!["piper.default"]);
    }

    #[tokio::test]
    async fn tts_rejects_empty_text_for_piper() {
        let cfg = config_with_piper_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        let err = manager
            .synthesize_with_provider("", "piper.default", "default")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "expected empty-text error, got: {err}"
        );
    }

    #[test]
    fn tts_config_defaults() {
        let config = zeroclaw_config::schema::TtsConfig::default();
        assert!(!config.enabled);
        // TtsConfig has no global default-provider field; per-agent
        // `tts_provider` is the only selector.
        assert_eq!(config.default_voice, "alloy");
        assert_eq!(config.default_format, "mp3");
        assert_eq!(config.max_text_length, DEFAULT_MAX_TEXT_LENGTH);
    }

    #[test]
    fn tts_manager_max_text_length_zero_uses_default() {
        let mut cfg = Config::default();
        cfg.tts.max_text_length = 0;
        let manager = TtsManager::from_config(&cfg).unwrap();
        assert_eq!(manager.max_text_length, DEFAULT_MAX_TEXT_LENGTH);
    }
}
