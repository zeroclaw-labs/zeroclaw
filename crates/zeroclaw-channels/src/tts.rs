//! Multi-provider Text-to-Speech (TTS) subsystem.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use zeroclaw_config::schema::{Config, TtsProviderConfig};

/// Maximum text length before synthesis is rejected (default: 4096 chars).
const DEFAULT_MAX_TEXT_LENGTH: usize = 4096;

/// Default HTTP request timeout for TTS API calls.
const TTS_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Maximum time allowed for a local ffmpeg transcode.
const FFMPEG_TRANSCODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

// ── TtsProvider trait ────────────────────────────────────────────

/// Trait for pluggable TTS backends.
#[async_trait::async_trait]
pub trait TtsProvider: Send + Sync + ::zeroclaw_api::attribution::Attributable {
    /// ModelProvider identifier (e.g. `"openai"`, `"elevenlabs"`).
    fn name(&self) -> &str;

    /// Synthesize `text` using the given `voice`, returning raw audio bytes.
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>>;

    /// The audio container/format of the bytes returned by
    /// [`synthesize`](Self::synthesize) (e.g. `"opus"`, `"wav"`, `"mp3"`).
    /// Channels use this to pick the correct upload MIME type and Telegram
    /// send method — only `opus`/`ogg` is a true voice note.
    fn output_format(&self) -> &str;

    /// Voices supported by this model_provider.
    fn supported_voices(&self) -> Vec<String>;

    /// Audio output formats supported by this model_provider.
    fn supported_formats(&self) -> Vec<String>;
}

// ── OpenAI TTS ───────────────────────────────────────────────────

/// OpenAI TTS model_provider (`POST /v1/audio/speech`).
pub struct OpenAiTtsProvider {
    alias: String,
    api_key: String,
    model: String,
    speed: f64,
    /// Full endpoint URL. Defaults to the OpenAI production endpoint; can be
    /// overridden via `[providers.tts.openai.<alias>].uri` to point at any
    /// OpenAI-compatible TTS backend (Groq, Azure, self-hosted proxies).
    base_url: String,
    /// Audio response format. Defaults to `"opus"`; override to `"wav"` for
    /// Orpheus-class models or `"mp3"` for broader compatibility.
    response_format: String,
    client: reqwest::Client,
}

impl OpenAiTtsProvider {
    /// Create a new OpenAI TTS model_provider from config. Reads
    /// `[tts_providers.openai.<alias>].api_key` (or via the schema-mirror
    /// env grammar). Legacy `OPENAI_API_KEY` env-var fallback eradicated
    /// in V0.8.0.
    pub fn new(alias: &str, config: &TtsProviderConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToOwned::to_owned)
            .context(
                "Missing OpenAI TTS API key: set `[tts_providers.openai.<alias>].api_key` (or via \
                 `ZEROCLAW_providers__tts__openai__<alias>__api_key=...`).",
            )?;

        Ok(Self {
            alias: alias.to_string(),
            api_key,
            model: config
                .model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "tts-1".to_string()),
            speed: config.speed.unwrap_or(1.0),
            base_url: config
                .uri
                .clone()
                .filter(|u| !u.trim().is_empty())
                .unwrap_or_else(|| "https://api.openai.com/v1/audio/speech".to_string()),
            response_format: config
                .response_format
                .clone()
                .filter(|f| !f.trim().is_empty())
                .unwrap_or_else(|| "opus".to_string()),
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

    fn output_format(&self) -> &str {
        &self.response_format
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
            "voice": voice,
            "speed": self.speed,
            "response_format": self.response_format,
        });

        let resp = self
            .client
            .post(&self.base_url)
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
    alias: String,
    api_key: String,
    model_id: String,
    stability: f64,
    similarity_boost: f64,
    /// Optional `optimize_streaming_latency` query level (0-4). Higher
    /// values trade audio quality for lower time-to-first-audio.
    optimize_streaming_latency: Option<u32>,
    client: reqwest::Client,
}

impl ElevenLabsTtsProvider {
    /// Create a new ElevenLabs TTS model_provider from config. Reads
    /// `[tts_providers.elevenlabs.<alias>].api_key`. Legacy
    /// `ELEVENLABS_API_KEY` env-var fallback eradicated in V0.8.0.
    pub fn new(alias: &str, config: &TtsProviderConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToOwned::to_owned)
            .context(
                "Missing ElevenLabs API key: set `[tts_providers.elevenlabs.<alias>].api_key` (or \
                 via `ZEROCLAW_providers__tts__elevenlabs__<alias>__api_key=...`).",
            )?;

        Ok(Self {
            alias: alias.to_string(),
            api_key,
            // Default model when the config leaves `model` unset: the
            // low-latency flash tier. Explicitly configured models are
            // always honored unchanged.
            model_id: config
                .model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| "eleven_flash_v2_5".to_string()),
            stability: config.stability.unwrap_or(0.5),
            similarity_boost: config.similarity_boost.unwrap_or(0.5),
            optimize_streaming_latency: config.optimize_streaming_latency,
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

    fn output_format(&self) -> &str {
        // ElevenLabs default output is MP3 (mp3_44100_128).
        "mp3"
    }

    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        if !voice
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("ElevenLabs voice ID contains invalid characters: {voice}");
        }
        let mut url = format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}");
        if let Some(level) = self.optimize_streaming_latency {
            url.push_str(&format!("?optimize_streaming_latency={level}"));
        }
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
    alias: String,
    api_key: String,
    language_code: String,
    client: reqwest::Client,
}

impl GoogleTtsProvider {
    /// Create a new Google Cloud TTS model_provider from config, resolving the API key
    /// from `[tts_providers.google.<alias>].api_key`. Legacy
    /// `GOOGLE_TTS_API_KEY` env-var fallback eradicated in V0.8.0.
    pub fn new(alias: &str, config: &TtsProviderConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(ToOwned::to_owned)
            .context(
                "Missing Google TTS API key: set `[tts_providers.google.<alias>].api_key` (or via \
                 `ZEROCLAW_providers__tts__google__<alias>__api_key=...`).",
            )?;

        Ok(Self {
            alias: alias.to_string(),
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

    fn output_format(&self) -> &str {
        // audioConfig.audioEncoding is hard-coded to MP3 below.
        "mp3"
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
    alias: String,
    binary_path: String,
}

impl EdgeTtsProvider {
    /// Allowed basenames for the Edge TTS binary.
    const ALLOWED_BINARIES: &[&str] = &["edge-tts", "edge-playback"];

    pub fn new(alias: &str, config: &TtsProviderConfig) -> Result<Self> {
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
            alias: alias.to_string(),
            binary_path: raw_path,
        })
    }
}

#[async_trait::async_trait]
impl TtsProvider for EdgeTtsProvider {
    fn name(&self) -> &str {
        "edge"
    }

    fn output_format(&self) -> &str {
        // edge-tts writes an MP3 temp file (see `--write-media …mp3`).
        "mp3"
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
    alias: String,
    client: reqwest::Client,
    api_url: String,
}

impl PiperTtsProvider {
    /// Create a new Piper TTS model_provider from config. Falls back to
    /// `http://127.0.0.1:5000/v1/audio/speech` when no `api_url` is supplied.
    pub fn new(alias: &str, config: &TtsProviderConfig) -> Self {
        let api_url = config
            .uri
            .clone()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:5000/v1/audio/speech".to_string());
        Self {
            alias: alias.to_string(),
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

    fn output_format(&self) -> &str {
        // Piper's OpenAI-compatible server returns WAV when no response_format
        // is requested (the body below omits it).
        "wav"
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

async fn write_audio_and_wait_with_output(
    mut child: tokio::process::Child,
    audio: Vec<u8>,
    timeout: std::time::Duration,
) -> Result<std::process::Output> {
    use tokio::io::AsyncWriteExt;

    let mut stdin = child.stdin.take().context("ffmpeg stdin was not piped")?;

    tokio::time::timeout(timeout, async move {
        // Drive stdin and wait concurrently: if the child fills its stdout pipe
        // before stdin is complete, sequential operation would deadlock.
        let (write_result, output) = tokio::join!(
            async move {
                stdin.write_all(&audio).await?;
                stdin.shutdown().await
            },
            child.wait_with_output()
        );

        write_result.context("failed to write audio to ffmpeg stdin")?;
        output.context("ffmpeg process error")
    })
    .await
    .with_context(|| format!("ffmpeg transcode timed out after {timeout:?}"))?
}

async fn transcode_to_opus(audio: Vec<u8>) -> Result<Vec<u8>> {
    use std::process::Stdio;

    let child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-f",
            "ogg",
            "-acodec",
            "libopus",
            "-b:a",
            "32k",
            "-vbr",
            "on",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context(
            "failed to spawn ffmpeg — ensure ffmpeg with libopus support is installed \
             (e.g. `sudo dnf install ffmpeg` / `sudo apt install ffmpeg`)",
        )?;

    let output = write_audio_and_wait_with_output(child, audio, FFMPEG_TRANSCODE_TIMEOUT).await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffmpeg transcode to opus failed: {stderr}");
    }

    anyhow::ensure!(
        !output.stdout.is_empty(),
        "ffmpeg produced empty output — check that libopus is available"
    );

    Ok(output.stdout)
}

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
    pub fn from_config(config: &Config) -> Result<Self> {
        Self::from_config_for_agent(config, None)
    }

    pub fn from_config_for_agent(config: &Config, agent_alias: Option<&str>) -> Result<Self> {
        let mut tts_providers: HashMap<String, Box<dyn TtsProvider>> = HashMap::new();
        let mut voice_by_alias: HashMap<String, String> = HashMap::new();

        // Typed dispatch over the TtsProviders container's named slots. The
        // unknown-type warn-and-skip arm is gone — the typed container can't
        // hold an unrecognized family.
        for (family, alias, instance) in config.providers.tts.iter_entries() {
            let dotted = format!("{family}.{alias}");
            let result: Result<Box<dyn TtsProvider>> = match family {
                "openai" => OpenAiTtsProvider::new(alias, instance).map(|p| Box::new(p) as _),
                "elevenlabs" => {
                    ElevenLabsTtsProvider::new(alias, instance).map(|p| Box::new(p) as _)
                }
                "google" => GoogleTtsProvider::new(alias, instance).map(|p| Box::new(p) as _),
                "edge" => EdgeTtsProvider::new(alias, instance).map(|p| Box::new(p) as _),
                "piper" => Ok(Box::new(PiperTtsProvider::new(alias, instance)) as _),
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
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": format!("{}", e), "dotted": dotted})
                            ),
                        "Skipping TTS provider"
                    );
                }
            }
        }

        let max_text_length = if config.tts.max_text_length == 0 {
            DEFAULT_MAX_TEXT_LENGTH
        } else {
            config.tts.max_text_length
        };

        // Per-agent join: bind to the channel-owning agent's `tts_provider`
        // when known, else the runtime-active agent. Empty (or no resolved
        // agent) = no TTS; `synthesize` fails loud rather than silently
        // pick a provider.
        let agent_tts_provider = agent_alias
            .or_else(|| config.resolved_runtime_agent_alias())
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

    pub async fn synthesize_opus(&self, text: &str) -> Result<Vec<u8>> {
        let audio = self.synthesize(text).await?;
        let provider_alias = self.agent_tts_provider.as_str();
        let format = self
            .tts_providers
            .get(provider_alias)
            .map(|p| p.output_format())
            .unwrap_or("unknown");
        if format == "opus" {
            return Ok(audio);
        }
        transcode_to_opus(audio).await
    }

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

    /// Synthesize text using the runtime-active agent's resolved
    /// `tts_provider` reference and an explicit voice.
    pub async fn synthesize_with_voice(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        let provider_alias = self.agent_tts_provider.as_str();
        if provider_alias.is_empty() {
            bail!(
                "Agent has no tts_provider configured. Set \
                 `agent.<alias>.tts_provider = \"<type>.<alias>\"` referencing a \
                 [providers.tts.<type>.<alias>] entry."
            );
        }
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
            let available = self.available_providers().join(", ");
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "tts_provider": provider_alias,
                        "available": available,
                    })),
                "tts: provider not configured"
            );
            anyhow::Error::msg(format!(
                "TTS model_provider '{}' not configured (available: {})",
                provider_alias, available
            ))
        })?;

        use ::zeroclaw_log::Instrument;
        let span = ::zeroclaw_log::attribution_span!(tts.as_ref());
        ::zeroclaw_log::scope!(voice: voice, => tts.synthesize(text, voice))
            .instrument(span)
            .await
    }

    /// List dotted aliases of all initialized tts_providers.
    pub fn available_providers(&self) -> Vec<String> {
        let mut names: Vec<_> = self.tts_providers.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn agent_output_format(&self) -> Option<&str> {
        let alias = self.agent_tts_provider.as_str();
        if alias.is_empty() {
            return None;
        }
        self.tts_providers.get(alias).map(|p| p.output_format())
    }

    /// Resolve the dotted provider alias a voice turn should synthesize
    /// with: the owning agent's `tts_provider` when it references a
    /// configured provider, else the deterministically-first configured
    /// provider as an install-wide fallback. `None` when no TTS provider
    /// is configured at all.
    pub fn resolve_voice_provider(&self) -> Option<String> {
        let bound = self.agent_tts_provider.as_str();
        if !bound.is_empty() && self.tts_providers.contains_key(bound) {
            return Some(bound.to_string());
        }
        self.available_providers().into_iter().next()
    }

    /// The voice configured for a dotted provider alias
    /// (`[providers.tts.<type>.<alias>].voice`), falling back to
    /// `[tts].default_voice`.
    pub fn voice_for_provider(&self, provider_alias: &str) -> &str {
        self.voice_by_alias
            .get(provider_alias)
            .map_or(self.default_voice.as_str(), String::as_str)
    }

    /// Audio container/format produced by a dotted provider alias.
    pub fn output_format_for_provider(&self, provider_alias: &str) -> Option<&str> {
        self.tts_providers
            .get(provider_alias)
            .map(|p| p.output_format())
    }
}

// ── ElevenLabs streaming (multi-stream-input) ────────────────────
//
// A persistent WebSocket session against
// `wss://api.elevenlabs.io/v1/text-to-speech/{voice}/multi-stream-input`.
// One socket is kept alive across turns of a single WS chat session; per
// turn a context is opened, sentence units are flushed as they stream in,
// and the context is closed at turn end. The socket idles out server-side
// after ~20s, so connection is established lazily and idempotently.

/// Immutable connection parameters for a streaming ElevenLabs session.
#[derive(Debug, Clone)]
pub struct ElevenLabsStreamConfig {
    pub api_key: String,
    pub voice_id: String,
    pub model_id: String,
    pub stability: f64,
    pub similarity_boost: f64,
    /// Base URL override replacing `wss://api.elevenlabs.io/v1/text-to-speech`.
    /// Populated from the provider's `uri` config when it starts with
    /// `ws://`/`wss://` — self-hosted proxies and test harnesses.
    pub ws_base: Option<String>,
}

impl ElevenLabsStreamConfig {
    /// The multi-stream-input WebSocket URL for this config. Output is raw
    /// little-endian mono 16-bit PCM at 16 kHz (`pcm_16000`), `auto_mode`
    /// on so each `flush` yields audio without an explicit alignment pass.
    #[must_use]
    pub fn ws_url(&self) -> String {
        let base = self
            .ws_base
            .as_deref()
            .map(|b| b.trim_end_matches('/'))
            .unwrap_or("wss://api.elevenlabs.io/v1/text-to-speech");
        format!(
            "{base}/{}/multi-stream-input?model_id={}&output_format=pcm_16000&auto_mode=true",
            self.voice_id, self.model_id
        )
    }

    /// Resolve streaming config for `agent_alias`'s TTS provider when it is
    /// an ElevenLabs-family provider with an API key; `None` otherwise (the
    /// caller then keeps the per-sentence HTTP synthesis path).
    pub fn from_config_for_agent(config: &Config, agent_alias: &str) -> Option<Self> {
        let dotted = config
            .agent(agent_alias)
            .map(|a| a.tts_provider.as_str().to_string())?;
        if !dotted.starts_with("elevenlabs.") {
            return None;
        }
        let (_, _, base) = config
            .providers
            .tts
            .iter_entries()
            .find(|(family, alias, _)| *family == "elevenlabs" && format!("{family}.{alias}") == dotted)?;
        let api_key = base
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())?
            .to_string();
        let voice_id = base
            .voice
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| config.tts.default_voice.clone());
        let model_id = base
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "eleven_flash_v2_5".to_string());
        let ws_base = base
            .uri
            .as_deref()
            .map(str::trim)
            .filter(|u| u.starts_with("ws://") || u.starts_with("wss://"))
            .map(ToOwned::to_owned);
        Some(Self {
            api_key,
            voice_id,
            model_id,
            stability: base.stability.unwrap_or(0.5),
            similarity_boost: base.similarity_boost.unwrap_or(0.5),
            ws_base,
        })
    }
}

/// A decoded audio payload from an ElevenLabs multi-stream frame. `audio`
/// is raw PCM bytes (already base64-decoded); it may be empty on a
/// standalone final marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAudioChunk {
    pub audio: Vec<u8>,
    pub context_id: Option<String>,
    pub is_final: bool,
}

/// Outgoing message opening a context: a single space primes the buffer and
/// carries the per-turn voice settings.
#[must_use]
pub(crate) fn stream_init_message(
    context_id: &str,
    stability: f64,
    similarity_boost: f64,
) -> serde_json::Value {
    serde_json::json!({
        "text": " ",
        "context_id": context_id,
        "voice_settings": {
            "stability": stability,
            "similarity_boost": similarity_boost,
        },
    })
}

/// Outgoing message flushing one sentence unit. A trailing space follows the
/// unit so ElevenLabs treats it as a complete token boundary.
#[must_use]
pub(crate) fn stream_flush_message(context_id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "text": format!("{text} "),
        "context_id": context_id,
        "flush": true,
    })
}

/// Outgoing message closing a context at turn end (or on barge-in).
#[must_use]
pub(crate) fn stream_close_message(context_id: &str) -> serde_json::Value {
    serde_json::json!({
        "context_id": context_id,
        "close_context": true,
    })
}

/// Parse an inbound ElevenLabs frame into a [`StreamAudioChunk`]. Returns
/// `None` for frames that carry neither audio nor a final marker (metadata,
/// alignment-only frames). Note the inbound field is `contextId`
/// (camelCase) whereas outbound uses `context_id`.
#[must_use]
pub(crate) fn parse_stream_audio(text: &str) -> Option<StreamAudioChunk> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let is_final = v.get("isFinal").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let context_id = v
        .get("contextId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let audio = match v.get("audio").and_then(serde_json::Value::as_str) {
        Some(b64) if !b64.is_empty() => {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.decode(b64).ok()?
        }
        _ => {
            if is_final {
                Vec::new()
            } else {
                return None;
            }
        }
    };
    Some(StreamAudioChunk {
        audio,
        context_id,
        is_final,
    })
}

/// Writer half of a streaming connection.
#[async_trait::async_trait]
pub trait StreamSocketWriter: Send {
    async fn send_json(&mut self, value: serde_json::Value) -> Result<()>;
}

/// Reader half of a streaming connection. Yields decoded audio chunks and
/// `None` once the socket closes.
#[async_trait::async_trait]
pub trait StreamSocketReader: Send {
    async fn next_audio(&mut self) -> Result<Option<StreamAudioChunk>>;
}

/// An established streaming connection, split so a turn can feed text and
/// drain audio concurrently.
pub struct StreamConnection {
    pub writer: Box<dyn StreamSocketWriter>,
    pub reader: Box<dyn StreamSocketReader>,
}

/// Establishes a fresh [`StreamConnection`]. Abstracted behind a trait so
/// the reconnect-idempotency guard can be exercised with a mock in tests.
#[async_trait::async_trait]
pub trait StreamConnector: Send + Sync {
    async fn connect(&self, cfg: &ElevenLabsStreamConfig) -> Result<StreamConnection>;
}

// ── Real tokio-tungstenite connector ─────────────────────────────

type ElWsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct WsWriterImpl {
    sink: futures_util::stream::SplitSink<ElWsStream, tokio_tungstenite::tungstenite::Message>,
}

#[async_trait::async_trait]
impl StreamSocketWriter for WsWriterImpl {
    async fn send_json(&mut self, value: serde_json::Value) -> Result<()> {
        use futures_util::SinkExt as _;
        self.sink
            .send(tokio_tungstenite::tungstenite::Message::Text(
                value.to_string().into(),
            ))
            .await
            .context("ElevenLabs stream send failed")
    }
}

struct WsReaderImpl {
    stream: futures_util::stream::SplitStream<ElWsStream>,
}

#[async_trait::async_trait]
impl StreamSocketReader for WsReaderImpl {
    async fn next_audio(&mut self) -> Result<Option<StreamAudioChunk>> {
        use futures_util::StreamExt as _;
        use tokio_tungstenite::tungstenite::Message;
        while let Some(msg) = self.stream.next().await {
            match msg.context("ElevenLabs stream recv failed")? {
                Message::Text(t) => {
                    if let Some(chunk) = parse_stream_audio(&t) {
                        return Ok(Some(chunk));
                    }
                }
                Message::Close(_) => return Ok(None),
                // Binary / Ping / Pong / Frame: ignore, keep reading.
                _ => {}
            }
        }
        Ok(None)
    }
}

/// The production connector. Establishes TCP+TLS (rustls/ring, webpki
/// roots) then performs the WebSocket upgrade with the `xi-api-key` header,
/// mirroring the proven manual handshake used elsewhere for channel
/// sockets (avoids relying on a process-default rustls crypto provider).
pub struct ElevenLabsWsConnector;

#[async_trait::async_trait]
impl StreamConnector for ElevenLabsWsConnector {
    async fn connect(&self, cfg: &ElevenLabsStreamConfig) -> Result<StreamConnection> {
        if !cfg
            .voice_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!(
                "ElevenLabs voice ID contains invalid characters: {}",
                cfg.voice_id
            );
        }
        let url = cfg.ws_url();
        let target = url::Url::parse(&url).context("invalid ElevenLabs stream URL")?;
        let host = target
            .host_str()
            .context("ElevenLabs stream URL missing host")?
            .to_string();
        let secure = target.scheme() == "wss";
        let port = target
            .port_or_known_default()
            .unwrap_or(if secure { 443 } else { 80 });

        let tcp = tokio::net::TcpStream::connect(format!("{host}:{port}"))
            .await
            .with_context(|| format!("TCP connect to {host}:{port}"))?;

        // Plain-TCP path exists for ws:// overrides (self-hosted proxies,
        // test harnesses); the real ElevenLabs endpoint is always wss.
        let stream = if secure {
            let mut root_store = rustls::RootCertStore::empty();
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let tls_config = std::sync::Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(root_store)
                    .with_no_client_auth(),
            );
            let connector = tokio_rustls::TlsConnector::from(tls_config);
            let server_name = rustls_pki_types::ServerName::try_from(host.clone())
                .with_context(|| format!("invalid TLS server name: {host}"))?;
            let tls_stream = connector
                .connect(server_name, tcp)
                .await
                .with_context(|| format!("TLS handshake with {host}"))?;
            tokio_tungstenite::MaybeTlsStream::Rustls(tls_stream)
        } else {
            tokio_tungstenite::MaybeTlsStream::Plain(tcp)
        };

        let host_header = if (secure && port == 443) || (!secure && port == 80) {
            host.clone()
        } else {
            format!("{host}:{port}")
        };
        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri(url.as_str())
            .header("Host", host_header)
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("xi-api-key", cfg.api_key.as_str())
            .body(())
            .context("failed to build ElevenLabs WebSocket upgrade request")?;

        let (ws, _resp) = tokio_tungstenite::client_async(request, stream)
            .await
            .with_context(|| format!("ElevenLabs WebSocket handshake failed for {url}"))?;
        let (sink, stream) = futures_util::StreamExt::split(ws);
        Ok(StreamConnection {
            writer: Box::new(WsWriterImpl { sink }),
            reader: Box::new(WsReaderImpl { stream }),
        })
    }
}

// ── Session (persistent, idempotent reconnect) ───────────────────

enum StreamState {
    /// No live socket. `run_turn` / `ensure_connected` will connect.
    Idle,
    /// A live, unused socket, reusable by the next turn.
    Ready(StreamConnection),
}

/// A persistent ElevenLabs streaming session, cheap to clone (all state is
/// shared). Cloning yields another handle onto the *same* socket so a
/// prewarm task and the turn worker cooperate on one connection.
#[derive(Clone)]
pub struct ElevenLabsStreamSession {
    cfg: ElevenLabsStreamConfig,
    connector: std::sync::Arc<dyn StreamConnector>,
    state: std::sync::Arc<tokio::sync::Mutex<StreamState>>,
    /// Cleared on a connect/stream failure so the next turn's up-front
    /// health check falls back to HTTP synthesis until a reconnect succeeds.
    healthy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ElevenLabsStreamSession {
    pub fn new(cfg: ElevenLabsStreamConfig, connector: std::sync::Arc<dyn StreamConnector>) -> Self {
        Self {
            cfg,
            connector,
            state: std::sync::Arc::new(tokio::sync::Mutex::new(StreamState::Idle)),
            healthy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    /// Build a session backed by the production tokio-tungstenite connector.
    #[must_use]
    pub fn with_default_connector(cfg: ElevenLabsStreamConfig) -> Self {
        Self::new(cfg, std::sync::Arc::new(ElevenLabsWsConnector))
    }

    /// Whether the last connect attempt succeeded (or none has failed yet).
    /// Used by the caller to decide, up front, between the streaming path
    /// and the HTTP fallback.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Idempotent connect. Serialized by the state mutex, so a prewarm
    /// racing turn setup can never open a second socket: the second caller
    /// observes the `Ready` state and returns immediately.
    pub async fn ensure_connected(&self) -> Result<()> {
        let mut guard = self.state.lock().await;
        if matches!(*guard, StreamState::Ready(_)) {
            return Ok(());
        }
        match self.connector.connect(&self.cfg).await {
            Ok(conn) => {
                *guard = StreamState::Ready(conn);
                self.healthy
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            Err(e) => {
                self.healthy
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                Err(e)
            }
        }
    }

    /// Run one turn over the persistent socket.
    ///
    /// Runs one voice turn over the persistent socket, opening ONE
    /// ELEVENLABS CONTEXT PER SENTENCE UNIT (context id
    /// `{turn_context_id}-u{unit_seq}`) so every audio frame is exactly
    /// attributable to the sentence unit it voices — this is what lets the
    /// gateway tag `tts_chunk` frames with `unit_seq` and the client fire
    /// audio-locked mascot cues. Up to [`TTS_STREAM_CONTEXTS_IN_FLIGHT`]
    /// contexts synthesize concurrently; audio is forwarded to `audio_tx`
    /// as `(unit_seq, chunk)` STRICTLY in unit order (later units buffer
    /// until earlier ones finish).
    ///
    /// On `cancel` all open contexts are closed and the socket stays
    /// reusable. Any I/O error drops the socket (next turn reconnects) and
    /// clears the health flag. Holding the state lock for the turn's
    /// duration is safe: turns on a session are serialized, so no second
    /// turn or prewarm contends for it.
    pub async fn run_turn(
        &self,
        context_id: &str,
        mut units: tokio::sync::mpsc::UnboundedReceiver<(u64, String)>,
        audio_tx: tokio::sync::mpsc::Sender<(u64, StreamAudioChunk)>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        use std::collections::{BTreeMap, VecDeque};

        let mut guard = self.state.lock().await;
        if !matches!(*guard, StreamState::Ready(_)) {
            match self.connector.connect(&self.cfg).await {
                Ok(conn) => *guard = StreamState::Ready(conn),
                Err(e) => {
                    self.healthy
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    return Err(e);
                }
            }
        }

        let unit_ctx = |seq: u64| format!("{context_id}-u{seq}");
        let parse_unit = |ctx: &str| -> Option<u64> {
            ctx.strip_prefix(context_id)
                .and_then(|rest| rest.strip_prefix("-u"))
                .and_then(|n| n.parse::<u64>().ok())
        };

        let run: Result<()> = async {
            let StreamState::Ready(conn) = &mut *guard else {
                unreachable!("connection ensured Ready above")
            };
            let StreamConnection { writer, reader } = conn;
            self.healthy
                .store(true, std::sync::atomic::Ordering::Relaxed);

            // Per-unit synthesis state. `next_emit` is the unit whose audio
            // may be forwarded right now; later units buffer.
            let mut pending: VecDeque<(u64, String)> = VecDeque::new();
            let mut buffered: BTreeMap<u64, Vec<StreamAudioChunk>> = BTreeMap::new();
            let mut finished: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
            let mut open: Vec<u64> = Vec::new();
            let mut next_emit: u64 = 0;
            let mut first_unit_seen = false;
            let mut feeding = true;

            loop {
                // Open queued contexts up to the in-flight cap.
                while open.len() < TTS_STREAM_CONTEXTS_IN_FLIGHT {
                    let Some((seq, text)) = pending.pop_front() else {
                        break;
                    };
                    let ctx = unit_ctx(seq);
                    writer
                        .send_json(stream_init_message(
                            &ctx,
                            self.cfg.stability,
                            self.cfg.similarity_boost,
                        ))
                        .await?;
                    writer.send_json(stream_flush_message(&ctx, &text)).await?;
                    writer.send_json(stream_close_message(&ctx)).await?;
                    open.push(seq);
                }
                // Turn complete: no more units coming, none queued or open.
                if !feeding && pending.is_empty() && open.is_empty() {
                    break;
                }

                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        // Barge-in: close every open context and drop the
                        // rest; the socket stays reusable for the next turn.
                        for seq in open.drain(..) {
                            let _ = writer.send_json(stream_close_message(&unit_ctx(seq))).await;
                        }
                        break;
                    }
                    unit = units.recv(), if feeding => match unit {
                        Some((seq, text)) => {
                            if !first_unit_seen {
                                first_unit_seen = true;
                                next_emit = seq;
                            }
                            pending.push_back((seq, text));
                        }
                        None => feeding = false,
                    },
                    audio = reader.next_audio() => match audio? {
                        Some(chunk) => {
                            let Some(unit) = chunk
                                .context_id
                                .as_deref()
                                .and_then(parse_unit)
                            else {
                                continue; // stale frame from a previous turn
                            };
                            let is_final = chunk.is_final;
                            if unit <= next_emit {
                                // Current unit — or a straggler that arrived
                                // after its final marker (misbehaving
                                // provider): emit rather than strand it.
                                if !chunk.audio.is_empty()
                                    && audio_tx.send((unit, chunk)).await.is_err()
                                {
                                    break;
                                }
                            } else if !chunk.audio.is_empty() {
                                buffered.entry(unit).or_default().push(chunk);
                            }
                            if is_final {
                                finished.insert(unit);
                                open.retain(|s| *s != unit);
                                // Advance emission through every finished
                                // unit, draining its buffered audio in order.
                                while finished.remove(&next_emit) {
                                    if let Some(chunks) = buffered.remove(&next_emit) {
                                        for c in chunks {
                                            if audio_tx.send((next_emit, c)).await.is_err() {
                                                break;
                                            }
                                        }
                                    }
                                    next_emit += 1;
                                }
                            }
                        }
                        None => break,
                    },
                }
            }
            Ok(())
        }
        .await;

        if run.is_err() {
            self.healthy
                .store(false, std::sync::atomic::Ordering::Relaxed);
            *guard = StreamState::Idle;
        }
        run
    }
}

/// How many per-unit ElevenLabs contexts may synthesize concurrently.
/// Mirrors the HTTP path's `TTS_MAX_IN_FLIGHT` pipelining depth.
pub const TTS_STREAM_CONTEXTS_IN_FLIGHT: usize = 2;

// ── Tests ────────────────────────────────────────────────────────

impl ::zeroclaw_api::attribution::Attributable for OpenAiTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::OpenAi,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for ElevenLabsTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::ElevenLabs,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for GoogleTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::Google,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for EdgeTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::Edge,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for PiperTtsProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(::zeroclaw_api::attribution::ProviderKind::Tts(
            ::zeroclaw_api::attribution::TtsProviderKind::Piper,
        ))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn piped_shell_child(script: &str) -> tokio::process::Child {
        use std::process::Stdio;

        tokio::process::Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn test child")
    }

    #[cfg(unix)]
    async fn process_exists(pid: u32) -> bool {
        use std::process::Stdio;

        tokio::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcode_process_times_out_stalled_child() {
        let child = piped_shell_child("exec sleep 60");
        let pid = child.id().expect("spawned child has a process ID");
        let started = std::time::Instant::now();
        let error = write_audio_and_wait_with_output(
            child,
            b"audio".to_vec(),
            std::time::Duration::from_millis(20),
        )
        .await
        .expect_err("stalled child must time out");

        assert!(
            error.to_string().contains("timed out"),
            "expected timeout error, got: {error:#}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "stalled child must return promptly"
        );

        let cleanup_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while process_exists(pid).await && std::time::Instant::now() < cleanup_deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!process_exists(pid).await, "timed-out child must be killed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn transcode_process_preserves_healthy_pipe_io() {
        let input = vec![b'a'; 1024 * 1024];
        let output = write_audio_and_wait_with_output(
            piped_shell_child("exec cat"),
            input.clone(),
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("healthy child completes");

        assert!(output.status.success());
        assert_eq!(output.stdout, input);
    }

    fn config_with_edge_alias() -> Config {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
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
            zeroclaw_config::schema::AliasedAgentConfig {
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

    #[test]
    fn tts_manager_binds_owning_agent_provider() {
        // Reuse the edge.default provider registration, but install two agents:
        // `primary` (the channel owner, has the provider) and a
        // lexicographically-earlier `background` agent with no `tts_provider`.
        let mut cfg = config_with_edge_alias();
        cfg.agents.clear();
        cfg.agents.insert(
            "primary".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                tts_provider: "edge.default".into(),
                ..Default::default()
            },
        );
        cfg.agents.insert(
            "background".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                ..Default::default()
            },
        );

        // Owner-bound resolution picks primary's provider...
        let owner_bound = TtsManager::from_config_for_agent(&cfg, Some("primary")).unwrap();
        assert_eq!(
            owner_bound.agent_tts_provider, "edge.default",
            "owner-bound manager must resolve the channel owner's tts_provider"
        );

        // ...while binding to the provider-less first-sorting agent stays empty,
        // proving the binding is per-agent and not a global/first-sorting pick.
        let background_bound = TtsManager::from_config_for_agent(&cfg, Some("background")).unwrap();
        assert!(
            background_bound.agent_tts_provider.is_empty(),
            "an agent with no tts_provider must not inherit another agent's provider"
        );
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
        let model_provider = PiperTtsProvider::new("test", &TtsProviderConfig::default());
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

    fn config_with_openai_wav_alias() -> Config {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                tts_provider: "openai.default".into(),
                ..Default::default()
            },
        );
        cfg.providers.tts.openai.insert(
            "default".to_string(),
            zeroclaw_config::schema::OpenAITtsProviderConfig {
                base: TtsProviderConfig {
                    api_key: Some("sk-test".to_string()),
                    response_format: Some("wav".to_string()),
                    ..TtsProviderConfig::default()
                },
            },
        );
        cfg
    }

    #[test]
    fn openai_provider_reports_configured_output_format() {
        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            response_format: Some("wav".to_string()),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("default", &cfg).unwrap();
        assert_eq!(provider.output_format(), "wav");
    }

    #[test]
    fn openai_provider_defaults_output_format_to_opus() {
        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("default", &cfg).unwrap();
        assert_eq!(provider.output_format(), "opus");
    }

    #[test]
    fn piper_provider_reports_wav_output_format() {
        let provider = PiperTtsProvider::new("default", &TtsProviderConfig::default());
        assert_eq!(provider.output_format(), "wav");
    }

    #[test]
    fn agent_output_format_resolves_active_provider() {
        let cfg = config_with_openai_wav_alias();
        let manager = TtsManager::from_config(&cfg).unwrap();
        assert_eq!(manager.agent_output_format(), Some("wav"));
    }

    #[test]
    fn agent_output_format_none_when_no_provider() {
        let manager = TtsManager::from_config(&Config::default()).unwrap();
        assert_eq!(manager.agent_output_format(), None);
    }

    #[test]
    fn tts_manager_max_text_length_zero_uses_default() {
        let mut cfg = Config::default();
        cfg.tts.max_text_length = 0;
        let manager = TtsManager::from_config(&cfg).unwrap();
        assert_eq!(manager.max_text_length, DEFAULT_MAX_TEXT_LENGTH);
    }

    #[tokio::test]
    async fn synthesize_posts_to_configured_uri_with_response_format() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"FAKE_WAV".to_vec()))
            .mount(&server)
            .await;

        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            uri: Some(format!("{}/v1/audio/speech", server.uri())),
            response_format: Some("wav".to_string()),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("test", &cfg).unwrap();

        let audio = provider.synthesize("hello world", "hannah").await.unwrap();
        assert_eq!(
            audio, b"FAKE_WAV",
            "synthesize should return the bytes served by the configured endpoint"
        );

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(
            reqs.len(),
            1,
            "exactly one POST should reach the configured uri"
        );
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(
            body["response_format"], "wav",
            "configured response_format must reach the outgoing request body"
        );
        assert_eq!(body["input"], "hello world");
        assert_eq!(body["voice"], "hannah");
        assert_eq!(body["model"], "tts-1");
    }

    #[tokio::test]
    async fn synthesize_defaults_response_format_to_opus_when_unset() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"AUDIO".to_vec()))
            .mount(&server)
            .await;

        // uri points at the mock so we can inspect the body; response_format left unset.
        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            uri: Some(format!("{}/v1/audio/speech", server.uri())),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("test", &cfg).unwrap();
        provider.synthesize("hi", "alloy").await.unwrap();

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(
            body["response_format"], "opus",
            "unset response_format must default to opus in the outgoing request"
        );
    }

    #[test]
    fn openai_defaults_to_production_endpoint_when_uri_unset() {
        let cfg = TtsProviderConfig {
            api_key: Some("sk-test".to_string()),
            ..TtsProviderConfig::default()
        };
        let provider = OpenAiTtsProvider::new("test", &cfg).unwrap();
        assert_eq!(provider.base_url, "https://api.openai.com/v1/audio/speech");
        assert_eq!(provider.response_format, "opus");
    }

    // ── ElevenLabs streaming ─────────────────────────────────────

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn stream_cfg() -> ElevenLabsStreamConfig {
        ElevenLabsStreamConfig {
            api_key: "sk-test".to_string(),
            voice_id: "voice123".to_string(),
            model_id: "eleven_flash_v2_5".to_string(),
            stability: 0.4,
            similarity_boost: 0.6,
            ws_base: None,
        }
    }

    #[test]
    fn stream_url_honors_ws_base_override() {
        let mut cfg = stream_cfg();
        cfg.ws_base = Some("ws://127.0.0.1:9100/v1/text-to-speech/".to_string());
        let url = cfg.ws_url();
        assert!(
            url.starts_with("ws://127.0.0.1:9100/v1/text-to-speech/voice123/multi-stream-input?"),
            "override replaces the default base: {url}"
        );
    }

    #[test]
    fn stream_url_carries_pcm_and_automode() {
        let url = stream_cfg().ws_url();
        assert!(url.starts_with(
            "wss://api.elevenlabs.io/v1/text-to-speech/voice123/multi-stream-input"
        ));
        assert!(url.contains("model_id=eleven_flash_v2_5"));
        assert!(url.contains("output_format=pcm_16000"));
        assert!(url.contains("auto_mode=true"));
    }

    #[test]
    fn stream_message_serialization() {
        let init = stream_init_message("turn-1", 0.4, 0.6);
        assert_eq!(init["text"], " ");
        assert_eq!(init["context_id"], "turn-1");
        assert_eq!(init["voice_settings"]["stability"], 0.4);
        assert_eq!(init["voice_settings"]["similarity_boost"], 0.6);
        assert!(init.get("flush").is_none());

        let flush = stream_flush_message("turn-1", "Hello there");
        assert_eq!(flush["text"], "Hello there ", "unit gets a trailing space");
        assert_eq!(flush["context_id"], "turn-1");
        assert_eq!(flush["flush"], true);

        let close = stream_close_message("turn-1");
        assert_eq!(close["context_id"], "turn-1");
        assert_eq!(close["close_context"], true);
        assert!(close.get("text").is_none());
    }

    #[test]
    fn parse_audio_decodes_payload() {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3, 4]);
        let frame = format!(r#"{{"audio":"{b64}","contextId":"turn-1","isFinal":false}}"#);
        let chunk = parse_stream_audio(&frame).expect("audio frame parses");
        assert_eq!(chunk.audio, vec![1, 2, 3, 4]);
        assert_eq!(chunk.context_id.as_deref(), Some("turn-1"));
        assert!(!chunk.is_final);
    }

    #[test]
    fn parse_audio_handles_final_marker_without_audio() {
        let frame = r#"{"audio":null,"contextId":"turn-1","isFinal":true}"#;
        let chunk = parse_stream_audio(frame).expect("final marker parses");
        assert!(chunk.audio.is_empty());
        assert!(chunk.is_final);
    }

    #[test]
    fn parse_audio_ignores_metadata_frames() {
        assert!(parse_stream_audio(r#"{"alignment":{"chars":[]}}"#).is_none());
        assert!(parse_stream_audio("not json").is_none());
    }

    // Mock transport for guard + protocol tests.
    struct CountingConnector {
        count: Arc<AtomicUsize>,
        fail: bool,
        sent: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
        audio: Vec<StreamAudioChunk>,
    }

    #[async_trait::async_trait]
    impl StreamConnector for CountingConnector {
        async fn connect(&self, _cfg: &ElevenLabsStreamConfig) -> Result<StreamConnection> {
            self.count.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                bail!("mock connect failure");
            }
            Ok(StreamConnection {
                writer: Box::new(RecordingWriter {
                    sent: Arc::clone(&self.sent),
                }),
                reader: Box::new(ScriptedReader {
                    queue: self.audio.clone().into(),
                }),
            })
        }
    }

    struct RecordingWriter {
        sent: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    }

    #[async_trait::async_trait]
    impl StreamSocketWriter for RecordingWriter {
        async fn send_json(&mut self, value: serde_json::Value) -> Result<()> {
            self.sent.lock().push(value);
            Ok(())
        }
    }

    struct ScriptedReader {
        queue: std::collections::VecDeque<StreamAudioChunk>,
    }

    #[async_trait::async_trait]
    impl StreamSocketReader for ScriptedReader {
        async fn next_audio(&mut self) -> Result<Option<StreamAudioChunk>> {
            Ok(self.queue.pop_front())
        }
    }

    fn counting_session(fail: bool) -> (ElevenLabsStreamSession, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let connector = CountingConnector {
            count: Arc::clone(&count),
            fail,
            sent: Arc::new(parking_lot::Mutex::new(Vec::new())),
            audio: Vec::new(),
        };
        (
            ElevenLabsStreamSession::new(stream_cfg(), Arc::new(connector)),
            count,
        )
    }

    #[tokio::test]
    async fn ensure_connected_is_idempotent() {
        let (session, count) = counting_session(false);
        session.ensure_connected().await.unwrap();
        // A racing prewarm on a clone sees the Ready state and does not
        // open a second socket.
        session.clone().ensure_connected().await.unwrap();
        session.ensure_connected().await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1, "must connect exactly once");
        assert!(session.is_healthy());
    }

    #[tokio::test]
    async fn ensure_connected_marks_unhealthy_on_failure() {
        let (session, count) = counting_session(true);
        assert!(session.ensure_connected().await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(
            !session.is_healthy(),
            "a failed connect must flip the health flag"
        );
    }

    #[tokio::test]
    async fn run_turn_opens_context_per_unit_and_emits_in_unit_order() {
        let count = Arc::new(AtomicUsize::new(0));
        let sent = Arc::new(parking_lot::Mutex::new(Vec::new()));
        // Unit 1's audio arrives BEFORE unit 0's — run_turn must reorder.
        let audio = vec![
            StreamAudioChunk {
                audio: vec![1, 1],
                context_id: Some("turn-1-u1".to_string()),
                is_final: true,
            },
            StreamAudioChunk {
                audio: vec![0, 0],
                context_id: Some("turn-1-u0".to_string()),
                is_final: true,
            },
        ];
        let connector = CountingConnector {
            count: Arc::clone(&count),
            fail: false,
            sent: Arc::clone(&sent),
            audio,
        };
        let session = ElevenLabsStreamSession::new(stream_cfg(), Arc::new(connector));

        let (unit_tx, unit_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, String)>();
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<(u64, StreamAudioChunk)>(8);
        unit_tx.send((0, "First sentence.".to_string())).unwrap();
        unit_tx.send((1, "Second sentence.".to_string())).unwrap();
        drop(unit_tx); // close feed → turn ends once all contexts finish

        let cancel = tokio_util::sync::CancellationToken::new();
        session
            .run_turn("turn-1", unit_rx, audio_tx, cancel)
            .await
            .unwrap();

        let mut chunks = Vec::new();
        while let Some(chunk) = audio_rx.recv().await {
            chunks.push(chunk);
        }
        assert_eq!(chunks.len(), 2, "both units' audio forwarded");
        assert_eq!(chunks[0].0, 0, "unit 0 first despite arriving second");
        assert_eq!(chunks[0].1.audio, vec![0, 0]);
        assert_eq!(chunks[1].0, 1);
        assert_eq!(chunks[1].1.audio, vec![1, 1]);

        let sent = sent.lock();
        // Per unit: init + flush + close.
        assert_eq!(sent.len(), 6, "2 × (init + flush + close)");
        assert_eq!(sent[0]["text"], " ", "unit 0 context opens first");
        assert_eq!(sent[0]["context_id"], "turn-1-u0");
        assert_eq!(sent[1]["text"], "First sentence. ");
        assert_eq!(sent[1]["flush"], true);
        assert_eq!(sent[2]["close_context"], true);
        assert_eq!(sent[2]["context_id"], "turn-1-u0");
        assert_eq!(sent[3]["context_id"], "turn-1-u1");
        assert_eq!(sent[4]["text"], "Second sentence. ");
        assert_eq!(sent[5]["close_context"], true);
    }

    #[tokio::test]
    async fn run_turn_closes_context_on_cancel() {
        let count = Arc::new(AtomicUsize::new(0));
        let sent = Arc::new(parking_lot::Mutex::new(Vec::new()));
        // Reader parks forever so the loop only exits via the cancel branch.
        struct PendingReader;
        #[async_trait::async_trait]
        impl StreamSocketReader for PendingReader {
            async fn next_audio(&mut self) -> Result<Option<StreamAudioChunk>> {
                std::future::pending().await
            }
        }
        struct CancelConnector {
            sent: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
            count: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl StreamConnector for CancelConnector {
            async fn connect(&self, _cfg: &ElevenLabsStreamConfig) -> Result<StreamConnection> {
                self.count.fetch_add(1, Ordering::SeqCst);
                Ok(StreamConnection {
                    writer: Box::new(RecordingWriter {
                        sent: Arc::clone(&self.sent),
                    }),
                    reader: Box::new(PendingReader),
                })
            }
        }
        let session = ElevenLabsStreamSession::new(
            stream_cfg(),
            Arc::new(CancelConnector {
                sent: Arc::clone(&sent),
                count: Arc::clone(&count),
            }),
        );

        let (unit_tx, unit_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, String)>();
        let (audio_tx, _audio_rx) = tokio::sync::mpsc::channel::<(u64, StreamAudioChunk)>(8);
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel2 = cancel.clone();
        // An open unit context ensures cancel has something to close.
        unit_tx.send((0, "Interrupted sentence.".to_string())).unwrap();
        // Keep the feed open so only the cancel branch can end the turn.
        let _keep = unit_tx;
        // Fire the cancel concurrently with the turn (no spawn — the repo
        // disallows bare tokio::spawn).
        let canceller = async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            cancel2.cancel();
        };
        let (run_result, ()) =
            tokio::join!(session.run_turn("turn-9", unit_rx, audio_tx, cancel), canceller);
        run_result.unwrap();

        let sent = sent.lock();
        assert_eq!(sent[0]["text"], " ");
        assert_eq!(
            sent.last().unwrap()["close_context"],
            true,
            "barge-in must close the active context"
        );
        // Socket stays Ready (reusable): only one connect ever happened.
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
