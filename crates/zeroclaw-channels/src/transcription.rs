use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use reqwest::multipart::{Form, Part};

use zeroclaw_config::schema::TranscriptionConfig;

/// Maximum upload size accepted by most Whisper-compatible APIs (25 MB).
const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;

/// Request timeout for transcription API calls (seconds).
const TRANSCRIPTION_TIMEOUT_SECS: u64 = 120;

// ── Audio utilities ─────────────────────────────────────────────

/// Map file extension to MIME type for Whisper-compatible transcription APIs.
fn mime_for_audio(extension: &str) -> Option<&'static str> {
    match extension.to_ascii_lowercase().as_str() {
        "flac" => Some("audio/flac"),
        "mp3" | "mpeg" | "mpga" => Some("audio/mpeg"),
        "mp4" | "m4a" => Some("audio/mp4"),
        "ogg" | "oga" => Some("audio/ogg"),
        "opus" => Some("audio/opus"),
        "wav" => Some("audio/wav"),
        "webm" => Some("audio/webm"),
        _ => None,
    }
}

/// Normalize audio filename for Whisper-compatible APIs.
///
/// Groq validates the filename extension — `.oga` (Opus-in-Ogg) is not in
/// its accepted list, so we rewrite it to `.ogg`.
fn normalize_audio_filename(file_name: &str) -> String {
    match file_name.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("oga") => format!("{stem}.ogg"),
        _ => file_name.to_string(),
    }
}

/// Resolve MIME type and normalize filename from extension.
///
/// No size check — callers enforce their own limits.
fn resolve_audio_format(file_name: &str) -> Result<(String, &'static str)> {
    let normalized_name = normalize_audio_filename(file_name);
    let extension = normalized_name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .unwrap_or("");
    let mime = mime_for_audio(extension).ok_or_else(|| {
        anyhow::anyhow!(
            "Unsupported audio format '.{extension}' — \
             accepted: flac, mp3, mp4, mpeg, mpga, m4a, ogg, opus, wav, webm"
        )
    })?;
    Ok((normalized_name, mime))
}

/// Validate audio data and resolve MIME type from file name.
///
/// Enforces the 25 MB cloud API cap. Returns `(normalized_filename, mime_type)` on success.
fn validate_audio(audio_data: &[u8], file_name: &str) -> Result<(String, &'static str)> {
    if audio_data.len() > MAX_AUDIO_BYTES {
        bail!(
            "Audio file too large ({} bytes, max {MAX_AUDIO_BYTES})",
            audio_data.len()
        );
    }
    resolve_audio_format(file_name)
}

// ── TranscriptionProvider trait ─────────────────────────────────

/// Trait for speech-to-text provider implementations.
#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    /// Human-readable provider name (e.g. "groq", "openai").
    fn name(&self) -> &str;

    /// Transcribe raw audio bytes. `file_name` includes the extension for
    /// format detection (e.g. "voice.ogg").
    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String>;

    /// List of supported audio file extensions.
    fn supported_formats(&self) -> Vec<String> {
        vec![
            "flac", "mp3", "mpeg", "mpga", "mp4", "m4a", "ogg", "oga", "opus", "wav", "webm",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }
}

// ── GroqProvider ────────────────────────────────────────────────

/// Groq Whisper API provider (default, backward-compatible with existing config).
pub struct GroqProvider {
    api_url: String,
    model: String,
    api_key: String,
    language: Option<String>,
}

impl GroqProvider {
    /// Build from the existing `TranscriptionConfig` fields.
    ///
    /// Credential resolution order:
    /// 1. `config.api_key`
    /// 2. `GROQ_API_KEY` environment variable (backward compatibility)
    pub fn from_config(config: &TranscriptionConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                std::env::var("GROQ_API_KEY")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .context(
                "Missing transcription API key: set [transcription].api_key or GROQ_API_KEY environment variable",
            )?;

        Ok(Self {
            api_url: config.api_url.clone(),
            model: config.model.clone(),
            api_key,
            language: config.language.clone(),
        })
    }
}

#[async_trait]
impl TranscriptionProvider for GroqProvider {
    fn name(&self) -> &str {
        "groq"
    }

    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        let (normalized_name, mime) = validate_audio(audio_data, file_name)?;

        let client = zeroclaw_config::schema::build_runtime_proxy_client("transcription.groq");

        let file_part = Part::bytes(audio_data.to_vec())
            .file_name(normalized_name)
            .mime_str(mime)?;

        let mut form = Form::new()
            .part("file", file_part)
            .text("model", self.model.clone())
            .text("response_format", "json");

        if let Some(ref lang) = self.language {
            form = form.text("language", lang.clone());
        }

        let resp = client
            .post(&self.api_url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .timeout(std::time::Duration::from_secs(TRANSCRIPTION_TIMEOUT_SECS))
            .send()
            .await
            .context("Failed to send transcription request to Groq")?;

        parse_whisper_response(resp).await
    }
}

// ── OpenAiWhisperProvider ───────────────────────────────────────

/// OpenAI Whisper API provider.
pub struct OpenAiWhisperProvider {
    api_key: String,
    model: String,
}

impl OpenAiWhisperProvider {
    pub fn from_config(config: &zeroclaw_config::schema::OpenAiSttConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .context("Missing OpenAI STT API key: set [transcription.openai].api_key")?;

        Ok(Self {
            api_key,
            model: config.model.clone(),
        })
    }
}

#[async_trait]
impl TranscriptionProvider for OpenAiWhisperProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        let (normalized_name, mime) = validate_audio(audio_data, file_name)?;

        let client = zeroclaw_config::schema::build_runtime_proxy_client("transcription.openai");

        let file_part = Part::bytes(audio_data.to_vec())
            .file_name(normalized_name)
            .mime_str(mime)?;

        let form = Form::new()
            .part("file", file_part)
            .text("model", self.model.clone())
            .text("response_format", "json");

        let resp = client
            .post("https://api.openai.com/v1/audio/transcriptions")
            .bearer_auth(&self.api_key)
            .multipart(form)
            .timeout(std::time::Duration::from_secs(TRANSCRIPTION_TIMEOUT_SECS))
            .send()
            .await
            .context("Failed to send transcription request to OpenAI")?;

        parse_whisper_response(resp).await
    }
}

// ── DeepgramProvider ────────────────────────────────────────────

/// Deepgram STT API provider.
pub struct DeepgramProvider {
    api_key: String,
    model: String,
}

impl DeepgramProvider {
    pub fn from_config(config: &zeroclaw_config::schema::DeepgramSttConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .context("Missing Deepgram API key: set [transcription.deepgram].api_key")?;

        Ok(Self {
            api_key,
            model: config.model.clone(),
        })
    }
}

#[async_trait]
impl TranscriptionProvider for DeepgramProvider {
    fn name(&self) -> &str {
        "deepgram"
    }

    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        let (_, mime) = validate_audio(audio_data, file_name)?;

        let client = zeroclaw_config::schema::build_runtime_proxy_client("transcription.deepgram");

        let url = format!(
            "https://api.deepgram.com/v1/listen?model={}&punctuate=true",
            self.model
        );

        let resp = client
            .post(&url)
            .header("Authorization", format!("Token {}", self.api_key))
            .header("Content-Type", mime)
            .body(audio_data.to_vec())
            .timeout(std::time::Duration::from_secs(TRANSCRIPTION_TIMEOUT_SECS))
            .send()
            .await
            .context("Failed to send transcription request to Deepgram")?;

        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse Deepgram response")?;

        if !status.is_success() {
            let error_msg = body["err_msg"]
                .as_str()
                .or_else(|| body["error"].as_str())
                .unwrap_or("unknown error");
            bail!("Deepgram API error ({}): {}", status, error_msg);
        }

        let text = body["results"]["channels"][0]["alternatives"][0]["transcript"]
            .as_str()
            .context("Deepgram response missing transcript field")?
            .to_string();

        Ok(text)
    }
}

// ── AssemblyAiProvider ──────────────────────────────────────────

/// AssemblyAI STT API provider.
pub struct AssemblyAiProvider {
    api_key: String,
}

impl AssemblyAiProvider {
    pub fn from_config(config: &zeroclaw_config::schema::AssemblyAiSttConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .context("Missing AssemblyAI API key: set [transcription.assemblyai].api_key")?;

        Ok(Self { api_key })
    }
}

#[async_trait]
impl TranscriptionProvider for AssemblyAiProvider {
    fn name(&self) -> &str {
        "assemblyai"
    }

    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        let (_, _) = validate_audio(audio_data, file_name)?;

        let client =
            zeroclaw_config::schema::build_runtime_proxy_client("transcription.assemblyai");

        // Step 1: Upload the audio file.
        let upload_resp = client
            .post("https://api.assemblyai.com/v2/upload")
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/octet-stream")
            .body(audio_data.to_vec())
            .timeout(std::time::Duration::from_secs(TRANSCRIPTION_TIMEOUT_SECS))
            .send()
            .await
            .context("Failed to upload audio to AssemblyAI")?;

        let upload_status = upload_resp.status();
        let upload_body: serde_json::Value = upload_resp
            .json()
            .await
            .context("Failed to parse AssemblyAI upload response")?;

        if !upload_status.is_success() {
            let error_msg = upload_body["error"].as_str().unwrap_or("unknown error");
            bail!("AssemblyAI upload error ({}): {}", upload_status, error_msg);
        }

        let upload_url = upload_body["upload_url"]
            .as_str()
            .context("AssemblyAI upload response missing 'upload_url'")?;

        // Step 2: Create transcription job.
        let transcript_req = serde_json::json!({
            "audio_url": upload_url,
        });

        let create_resp = client
            .post("https://api.assemblyai.com/v2/transcript")
            .header("Authorization", &self.api_key)
            .json(&transcript_req)
            .timeout(std::time::Duration::from_secs(TRANSCRIPTION_TIMEOUT_SECS))
            .send()
            .await
            .context("Failed to create AssemblyAI transcription")?;

        let create_status = create_resp.status();
        let create_body: serde_json::Value = create_resp
            .json()
            .await
            .context("Failed to parse AssemblyAI create response")?;

        if !create_status.is_success() {
            let error_msg = create_body["error"].as_str().unwrap_or("unknown error");
            bail!(
                "AssemblyAI transcription error ({}): {}",
                create_status,
                error_msg
            );
        }

        let transcript_id = create_body["id"]
            .as_str()
            .context("AssemblyAI response missing 'id'")?;

        // Step 3: Poll for completion.
        let poll_url = format!("https://api.assemblyai.com/v2/transcript/{transcript_id}");
        let poll_interval = std::time::Duration::from_secs(3);
        let poll_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);

        while tokio::time::Instant::now() < poll_deadline {
            tokio::time::sleep(poll_interval).await;

            let poll_resp = client
                .get(&poll_url)
                .header("Authorization", &self.api_key)
                .timeout(std::time::Duration::from_secs(30))
                .send()
                .await
                .context("Failed to poll AssemblyAI transcription")?;

            let poll_status = poll_resp.status();
            let poll_body: serde_json::Value = poll_resp
                .json()
                .await
                .context("Failed to parse AssemblyAI poll response")?;

            if !poll_status.is_success() {
                let error_msg = poll_body["error"].as_str().unwrap_or("unknown poll error");
                bail!("AssemblyAI poll error ({}): {}", poll_status, error_msg);
            }

            let status_str = poll_body["status"].as_str().unwrap_or("unknown");

            match status_str {
                "completed" => {
                    let text = poll_body["text"]
                        .as_str()
                        .context("AssemblyAI response missing 'text'")?
                        .to_string();
                    return Ok(text);
                }
                "error" => {
                    let error_msg = poll_body["error"]
                        .as_str()
                        .unwrap_or("unknown transcription error");
                    bail!("AssemblyAI transcription failed: {}", error_msg);
                }
                _ => {}
            }
        }

        bail!("AssemblyAI transcription timed out after 180s")
    }
}

// ── GoogleSttProvider ───────────────────────────────────────────

/// Google Cloud Speech-to-Text API provider.
pub struct GoogleSttProvider {
    api_key: String,
    language_code: String,
}

impl GoogleSttProvider {
    pub fn from_config(config: &zeroclaw_config::schema::GoogleSttConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .context("Missing Google STT API key: set [transcription.google].api_key")?;

        Ok(Self {
            api_key,
            language_code: config.language_code.clone(),
        })
    }
}

#[async_trait]
impl TranscriptionProvider for GoogleSttProvider {
    fn name(&self) -> &str {
        "google"
    }

    fn supported_formats(&self) -> Vec<String> {
        // Google Cloud STT supports a subset of formats.
        vec!["flac", "wav", "ogg", "opus", "mp3", "webm"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        let (normalized_name, _) = validate_audio(audio_data, file_name)?;

        let client = zeroclaw_config::schema::build_runtime_proxy_client("transcription.google");

        let encoding = match normalized_name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("flac") => "FLAC",
            Some("wav") => "LINEAR16",
            Some("ogg" | "opus") => "OGG_OPUS",
            Some("mp3") => "MP3",
            Some("webm") => "WEBM_OPUS",
            Some(ext) => bail!("Google STT does not support '.{ext}' input"),
            None => bail!("Google STT requires a file extension"),
        };

        let audio_content =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, audio_data);

        let request_body = serde_json::json!({
            "config": {
                "encoding": encoding,
                "languageCode": &self.language_code,
                "enableAutomaticPunctuation": true,
            },
            "audio": {
                "content": audio_content,
            }
        });

        let url = format!(
            "https://speech.googleapis.com/v1/speech:recognize?key={}",
            self.api_key
        );

        let resp = client
            .post(&url)
            .json(&request_body)
            .timeout(std::time::Duration::from_secs(TRANSCRIPTION_TIMEOUT_SECS))
            .send()
            .await
            .context("Failed to send transcription request to Google STT")?;

        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse Google STT response")?;

        if !status.is_success() {
            let error_msg = body["error"]["message"].as_str().unwrap_or("unknown error");
            bail!("Google STT API error ({}): {}", status, error_msg);
        }

        let text = body["results"][0]["alternatives"][0]["transcript"]
            .as_str()
            .unwrap_or("")
            .to_string();

        Ok(text)
    }
}

// ── ElevenLabsProvider ──────────────────────────────────────────

/// ElevenLabs Scribe STT API provider.
pub struct ElevenLabsProvider {
    api_key: String,
    model_id: String,
    language_code: Option<String>,
}

impl ElevenLabsProvider {
    pub fn from_config(config: &zeroclaw_config::schema::ElevenLabsSttConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                std::env::var("ELEVENLABS_API_KEY")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .context(
                "Missing ElevenLabs STT API key: set [transcription.elevenlabs].api_key or ELEVENLABS_API_KEY env",
            )?;

        Ok(Self {
            api_key,
            model_id: config.model_id.clone(),
            language_code: config
                .language_code
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
        })
    }
}

#[async_trait]
impl TranscriptionProvider for ElevenLabsProvider {
    fn name(&self) -> &str {
        "elevenlabs"
    }

    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        let (normalized_name, mime) = validate_audio(audio_data, file_name)?;

        let client =
            zeroclaw_config::schema::build_runtime_proxy_client("transcription.elevenlabs");

        let file_part = Part::bytes(audio_data.to_vec())
            .file_name(normalized_name)
            .mime_str(mime)?;

        let mut form = Form::new()
            .part("file", file_part)
            .text("model_id", self.model_id.clone());
        if let Some(ref lang) = self.language_code {
            form = form.text("language_code", lang.clone());
        }

        let resp = client
            .post("https://api.elevenlabs.io/v1/speech-to-text")
            .header("xi-api-key", &self.api_key)
            .multipart(form)
            .timeout(std::time::Duration::from_secs(TRANSCRIPTION_TIMEOUT_SECS))
            .send()
            .await
            .context("Failed to send transcription request to ElevenLabs")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("ElevenLabs STT API error ({status}): {}", body.trim());
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse ElevenLabs STT response")?;

        let text = body["text"]
            .as_str()
            .context("ElevenLabs STT response missing 'text' field")?
            .to_string();
        Ok(text)
    }
}

// ── LocalWhisperProvider ────────────────────────────────────────

/// Self-hosted faster-whisper-compatible STT provider.
///
/// POSTs audio as `multipart/form-data` (field name `file`) to a configurable
/// HTTP endpoint (e.g. `http://localhost:8000` or a private network host). The endpoint
/// must return `{"text": "..."}`. No cloud API key required. Size limit is
/// configurable — not constrained by the 25 MB cloud API cap.
pub struct LocalWhisperProvider {
    url: String,
    bearer_token: String,
    max_audio_bytes: usize,
    timeout_secs: u64,
}

impl LocalWhisperProvider {
    /// Build from config. Fails if `url` or `bearer_token` is empty, if `url`
    /// is not a valid HTTP/HTTPS URL (scheme must be `http` or `https`), if
    /// `max_audio_bytes` is zero, or if `timeout_secs` is zero.
    pub fn from_config(config: &zeroclaw_config::schema::LocalWhisperConfig) -> Result<Self> {
        let url = config.url.trim().to_string();
        anyhow::ensure!(!url.is_empty(), "local_whisper: `url` must not be empty");
        let parsed = url
            .parse::<reqwest::Url>()
            .with_context(|| format!("local_whisper: invalid `url`: {url:?}"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https"),
            "local_whisper: `url` must use http or https scheme, got {:?}",
            parsed.scheme()
        );

        let bearer_token = match config.bearer_token.as_deref().map(str::trim) {
            None => anyhow::bail!("local_whisper: `bearer_token` must be set"),
            Some("") => anyhow::bail!("local_whisper: `bearer_token` must not be empty"),
            Some(t) => t.to_string(),
        };

        anyhow::ensure!(
            config.max_audio_bytes > 0,
            "local_whisper: `max_audio_bytes` must be greater than zero"
        );

        anyhow::ensure!(
            config.timeout_secs > 0,
            "local_whisper: `timeout_secs` must be greater than zero"
        );

        Ok(Self {
            url,
            bearer_token,
            max_audio_bytes: config.max_audio_bytes,
            timeout_secs: config.timeout_secs,
        })
    }
}

#[async_trait]
impl TranscriptionProvider for LocalWhisperProvider {
    fn name(&self) -> &str {
        "local_whisper"
    }

    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        if audio_data.len() > self.max_audio_bytes {
            bail!(
                "Audio file too large ({} bytes, local_whisper max {})",
                audio_data.len(),
                self.max_audio_bytes
            );
        }

        let (normalized_name, mime) = resolve_audio_format(file_name)?;

        let client =
            zeroclaw_config::schema::build_runtime_proxy_client("transcription.local_whisper");

        // to_vec() clones the buffer for the multipart payload; peak memory per
        // call is ~2× max_audio_bytes. TODO: replace with streaming upload once
        // reqwest supports body streaming in multipart parts.
        let file_part = Part::bytes(audio_data.to_vec())
            .file_name(normalized_name)
            .mime_str(mime)?;

        let resp = client
            .post(&self.url)
            .bearer_auth(&self.bearer_token)
            .multipart(Form::new().part("file", file_part))
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send()
            .await
            .context("Failed to send audio to local Whisper endpoint")?;

        parse_whisper_response(resp).await
    }
}

// ── Shared response parsing ─────────────────────────────────────

/// Parse a faster-whisper-compatible JSON response (`{ "text": "..." }`).
///
/// Checks HTTP status before attempting JSON parsing so that non-JSON error
/// bodies (plain text, HTML, empty 5xx) produce a readable status error
/// rather than a confusing "Failed to parse transcription response".
async fn parse_whisper_response(resp: reqwest::Response) -> Result<String> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("Transcription API error ({}): {}", status, body.trim());
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .context("Failed to parse transcription response")?;

    let text = body["text"]
        .as_str()
        .context("Transcription response missing 'text' field")?
        .to_string();

    Ok(text)
}

// ── TranscriptionManager ────────────────────────────────────────

/// Manages multiple STT providers and routes transcription requests.
pub struct TranscriptionManager {
    providers: HashMap<String, Box<dyn TranscriptionProvider>>,
    default_provider: String,
}

impl TranscriptionManager {
    /// Build a `TranscriptionManager` from config.
    ///
    /// Always attempts to register the Groq provider from existing config fields.
    /// Additional providers are registered when their config sections are present.
    ///
    /// Provider keys with missing API keys are silently skipped — the error
    /// surfaces at transcribe-time so callers that target a different default
    /// provider are not blocked.
    pub fn new(config: &TranscriptionConfig) -> Result<Self> {
        let mut providers: HashMap<String, Box<dyn TranscriptionProvider>> = HashMap::new();

        if let Ok(groq) = GroqProvider::from_config(config) {
            providers.insert("groq".to_string(), Box::new(groq));
        }

        if let Some(ref openai_cfg) = config.openai
            && let Ok(p) = OpenAiWhisperProvider::from_config(openai_cfg)
        {
            providers.insert("openai".to_string(), Box::new(p));
        }

        if let Some(ref deepgram_cfg) = config.deepgram
            && let Ok(p) = DeepgramProvider::from_config(deepgram_cfg)
        {
            providers.insert("deepgram".to_string(), Box::new(p));
        }

        if let Some(ref assemblyai_cfg) = config.assemblyai
            && let Ok(p) = AssemblyAiProvider::from_config(assemblyai_cfg)
        {
            providers.insert("assemblyai".to_string(), Box::new(p));
        }

        if let Some(ref google_cfg) = config.google
            && let Ok(p) = GoogleSttProvider::from_config(google_cfg)
        {
            providers.insert("google".to_string(), Box::new(p));
        }

        if let Some(ref el_cfg) = config.elevenlabs
            && let Ok(p) = ElevenLabsProvider::from_config(el_cfg)
        {
            providers.insert("elevenlabs".to_string(), Box::new(p));
        }

        if let Some(ref local_cfg) = config.local_whisper {
            match LocalWhisperProvider::from_config(local_cfg) {
                Ok(p) => {
                    providers.insert("local_whisper".to_string(), Box::new(p));
                }
                Err(e) => {
                    tracing::warn!("local_whisper config invalid, provider skipped: {e}");
                }
            }
        }

        let default_provider = config.default_provider.clone();

        if config.enabled && !providers.contains_key(&default_provider) {
            let available: Vec<&str> = providers.keys().map(|k| k.as_str()).collect();
            bail!(
                "Default transcription provider '{}' is not configured. Available: {available:?}",
                default_provider
            );
        }

        Ok(Self {
            providers,
            default_provider,
        })
    }

    /// Transcribe audio using the default provider.
    pub async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        self.transcribe_with_provider(audio_data, file_name, &self.default_provider)
            .await
    }

    /// Transcribe audio using a specific named provider.
    pub async fn transcribe_with_provider(
        &self,
        audio_data: &[u8],
        file_name: &str,
        provider: &str,
    ) -> Result<String> {
        let p = self.providers.get(provider).ok_or_else(|| {
            let available: Vec<&str> = self.providers.keys().map(|k| k.as_str()).collect();
            anyhow::anyhow!(
                "Transcription provider '{provider}' not configured. Available: {available:?}"
            )
        })?;

        p.transcribe(audio_data, file_name).await
    }

    /// List registered provider names.
    pub fn available_providers(&self) -> Vec<&str> {
        self.providers.keys().map(|k| k.as_str()).collect()
    }
}

// ── Backward-compatible convenience function ────────────────────

/// Transcribe audio bytes via a Whisper-compatible transcription API.
///
/// Returns the transcribed text on success.
///
/// This is the backward-compatible entry point that preserves the original
/// function signature. It uses the Groq provider directly, matching the
/// original single-provider behavior.
///
/// Credential resolution order:
/// 1. `config.transcription.api_key`
/// 2. `GROQ_API_KEY` environment variable (backward compatibility)
///
/// The caller is responsible for enforcing duration limits *before* downloading
/// the file; this function enforces the byte-size cap.
pub async fn transcribe_audio(
    audio_data: Vec<u8>,
    file_name: &str,
    config: &TranscriptionConfig,
) -> Result<String> {
    // Validate audio before resolving credentials so that size/format errors
    // are reported before missing-key errors (preserves original behavior).
    validate_audio(&audio_data, file_name)?;

    match config.default_provider.as_str() {
        "groq" => {
            let groq = GroqProvider::from_config(config)?;
            groq.transcribe(&audio_data, file_name).await
        }
        "openai" => {
            let openai_cfg = config.openai.as_ref().context(
                "Default transcription provider 'openai' is not configured. Add [transcription.openai]",
            )?;
            let openai = OpenAiWhisperProvider::from_config(openai_cfg)?;
            openai.transcribe(&audio_data, file_name).await
        }
        "deepgram" => {
            let deepgram_cfg = config.deepgram.as_ref().context(
                "Default transcription provider 'deepgram' is not configured. Add [transcription.deepgram]",
            )?;
            let deepgram = DeepgramProvider::from_config(deepgram_cfg)?;
            deepgram.transcribe(&audio_data, file_name).await
        }
        "assemblyai" => {
            let assemblyai_cfg = config.assemblyai.as_ref().context(
                "Default transcription provider 'assemblyai' is not configured. Add [transcription.assemblyai]",
            )?;
            let assemblyai = AssemblyAiProvider::from_config(assemblyai_cfg)?;
            assemblyai.transcribe(&audio_data, file_name).await
        }
        "google" => {
            let google_cfg = config.google.as_ref().context(
                "Default transcription provider 'google' is not configured. Add [transcription.google]",
            )?;
            let google = GoogleSttProvider::from_config(google_cfg)?;
            google.transcribe(&audio_data, file_name).await
        }
        "elevenlabs" => {
            let el_cfg = config.elevenlabs.as_ref().context(
                "Default transcription provider 'elevenlabs' is not configured. Add [transcription.elevenlabs]",
            )?;
            let el = ElevenLabsProvider::from_config(el_cfg)?;
            el.transcribe(&audio_data, file_name).await
        }
        other => bail!("Unsupported transcription provider '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_oversized_audio() {
        let big = vec![0u8; MAX_AUDIO_BYTES + 1];
        let config = TranscriptionConfig::default();

        let err = transcribe_audio(big, "test.ogg", &config)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("too large"),
            "expected size error, got: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_missing_api_key() {
        // Ensure all candidate keys are absent for this test.
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("OPENAI_API_KEY") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("TRANSCRIPTION_API_KEY") };

        let data = vec![0u8; 100];
        let config = TranscriptionConfig::default();

        let err = transcribe_audio(data, "test.ogg", &config)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("transcription API key"),
            "expected missing-key error, got: {err}"
        );
    }

    #[tokio::test]
    async fn uses_config_api_key_without_groq_env() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        let data = vec![0u8; 100];
        let config = TranscriptionConfig {
            api_key: Some("transcription-key".to_string()),
            ..TranscriptionConfig::default()
        };

        // Keep invalid extension so we fail before network, but after key resolution.
        let err = transcribe_audio(data, "recording.aac", &config)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Unsupported audio format"),
            "expected unsupported-format error, got: {err}"
        );
    }

    #[tokio::test]
    async fn openai_default_provider_uses_openai_config() {
        let data = vec![0u8; 100];
        let config = TranscriptionConfig {
            default_provider: "openai".to_string(),
            openai: Some(zeroclaw_config::schema::OpenAiSttConfig {
                api_key: None,
                model: "gpt-4o-mini-transcribe".to_string(),
            }),
            ..TranscriptionConfig::default()
        };

        let err = transcribe_audio(data, "test.ogg", &config)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("[transcription.openai].api_key"),
            "expected openai-specific missing-key error, got: {err}"
        );
    }

    #[test]
    fn mime_for_audio_maps_accepted_formats() {
        let cases = [
            ("flac", "audio/flac"),
            ("mp3", "audio/mpeg"),
            ("mpeg", "audio/mpeg"),
            ("mpga", "audio/mpeg"),
            ("mp4", "audio/mp4"),
            ("m4a", "audio/mp4"),
            ("ogg", "audio/ogg"),
            ("oga", "audio/ogg"),
            ("opus", "audio/opus"),
            ("wav", "audio/wav"),
            ("webm", "audio/webm"),
        ];
        for (ext, expected) in cases {
            assert_eq!(
                mime_for_audio(ext),
                Some(expected),
                "failed for extension: {ext}"
            );
        }
    }

    #[test]
    fn mime_for_audio_case_insensitive() {
        assert_eq!(mime_for_audio("OGG"), Some("audio/ogg"));
        assert_eq!(mime_for_audio("MP3"), Some("audio/mpeg"));
        assert_eq!(mime_for_audio("Opus"), Some("audio/opus"));
    }

    #[test]
    fn mime_for_audio_rejects_unknown() {
        assert_eq!(mime_for_audio("txt"), None);
        assert_eq!(mime_for_audio("pdf"), None);
        assert_eq!(mime_for_audio("aac"), None);
        assert_eq!(mime_for_audio(""), None);
    }

    #[test]
    fn normalize_audio_filename_rewrites_oga() {
        assert_eq!(normalize_audio_filename("voice.oga"), "voice.ogg");
        assert_eq!(normalize_audio_filename("file.OGA"), "file.ogg");
    }

    #[test]
    fn normalize_audio_filename_preserves_accepted() {
        assert_eq!(normalize_audio_filename("voice.ogg"), "voice.ogg");
        assert_eq!(normalize_audio_filename("track.mp3"), "track.mp3");
        assert_eq!(normalize_audio_filename("clip.opus"), "clip.opus");
    }

    #[test]
    fn normalize_audio_filename_no_extension() {
        assert_eq!(normalize_audio_filename("voice"), "voice");
    }

    #[tokio::test]
    async fn rejects_unsupported_audio_format() {
        let data = vec![0u8; 100];
        let config = TranscriptionConfig::default();

        let err = transcribe_audio(data, "recording.aac", &config)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Unsupported audio format"),
            "expected unsupported-format error, got: {msg}"
        );
        assert!(
            msg.contains(".aac"),
            "error should mention the rejected extension, got: {msg}"
        );
    }

    // ── TranscriptionManager tests ──────────────────────────────

    #[test]
    fn manager_creation_with_default_config() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        let config = TranscriptionConfig::default();
        let manager = TranscriptionManager::new(&config).unwrap();
        assert_eq!(manager.default_provider, "groq");
        // Groq won't be registered without a key.
        assert!(manager.providers.is_empty());
    }

    #[test]
    fn manager_registers_groq_with_key() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        let config = TranscriptionConfig {
            api_key: Some("test-groq-key".to_string()),
            ..TranscriptionConfig::default()
        };

        let manager = TranscriptionManager::new(&config).unwrap();
        assert!(manager.providers.contains_key("groq"));
        assert_eq!(manager.providers["groq"].name(), "groq");
    }

    #[test]
    fn manager_registers_multiple_providers() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        let config = TranscriptionConfig {
            api_key: Some("test-groq-key".to_string()),
            openai: Some(zeroclaw_config::schema::OpenAiSttConfig {
                api_key: Some("test-openai-key".to_string()),
                model: "whisper-1".to_string(),
            }),
            deepgram: Some(zeroclaw_config::schema::DeepgramSttConfig {
                api_key: Some("test-deepgram-key".to_string()),
                model: "nova-2".to_string(),
            }),
            ..TranscriptionConfig::default()
        };

        let manager = TranscriptionManager::new(&config).unwrap();
        assert!(manager.providers.contains_key("groq"));
        assert!(manager.providers.contains_key("openai"));
        assert!(manager.providers.contains_key("deepgram"));
        assert_eq!(manager.available_providers().len(), 3);
    }

    #[tokio::test]
    async fn manager_rejects_unconfigured_provider() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        let config = TranscriptionConfig {
            api_key: Some("test-groq-key".to_string()),
            ..TranscriptionConfig::default()
        };

        let manager = TranscriptionManager::new(&config).unwrap();
        let err = manager
            .transcribe_with_provider(&[0u8; 100], "test.ogg", "nonexistent")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not configured"),
            "expected not-configured error, got: {err}"
        );
    }

    #[test]
    fn manager_default_provider_from_config() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        let config = TranscriptionConfig {
            default_provider: "openai".to_string(),
            openai: Some(zeroclaw_config::schema::OpenAiSttConfig {
                api_key: Some("test-openai-key".to_string()),
                model: "whisper-1".to_string(),
            }),
            ..TranscriptionConfig::default()
        };

        let manager = TranscriptionManager::new(&config).unwrap();
        assert_eq!(manager.default_provider, "openai");
    }

    #[test]
    fn validate_audio_rejects_oversized() {
        let big = vec![0u8; MAX_AUDIO_BYTES + 1];
        let err = validate_audio(&big, "test.ogg").unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn validate_audio_rejects_unsupported_format() {
        let data = vec![0u8; 100];
        let err = validate_audio(&data, "test.aac").unwrap_err();
        assert!(err.to_string().contains("Unsupported audio format"));
    }

    #[test]
    fn validate_audio_accepts_supported_format() {
        let data = vec![0u8; 100];
        let (name, mime) = validate_audio(&data, "test.ogg").unwrap();
        assert_eq!(name, "test.ogg");
        assert_eq!(mime, "audio/ogg");
    }

    #[test]
    fn validate_audio_normalizes_oga() {
        let data = vec![0u8; 100];
        let (name, mime) = validate_audio(&data, "voice.oga").unwrap();
        assert_eq!(name, "voice.ogg");
        assert_eq!(mime, "audio/ogg");
    }

    #[test]
    fn backward_compat_config_defaults_unchanged() {
        let config = TranscriptionConfig::default();
        assert!(!config.enabled);
        assert!(config.api_key.is_none());
        assert!(config.api_url.contains("groq.com"));
        assert_eq!(config.model, "whisper-large-v3-turbo");
        assert_eq!(config.default_provider, "groq");
        assert!(config.openai.is_none());
        assert!(config.deepgram.is_none());
        assert!(config.assemblyai.is_none());
        assert!(config.google.is_none());
        assert!(config.local_whisper.is_none());
        assert!(!config.transcribe_non_ptt_audio);
    }

    // ── LocalWhisperProvider tests (TDD — added below as red/green cycles) ──

    fn local_whisper_config(url: &str) -> zeroclaw_config::schema::LocalWhisperConfig {
        zeroclaw_config::schema::LocalWhisperConfig {
            url: url.to_string(),
            bearer_token: Some("test-token".to_string()),
            max_audio_bytes: 10 * 1024 * 1024,
            timeout_secs: 30,
        }
    }

    #[test]
    fn local_whisper_rejects_empty_url() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.url = String::new();
        let err = LocalWhisperProvider::from_config(&cfg).err().unwrap();
        assert!(
            err.to_string().contains("`url` must not be empty"),
            "got: {err}"
        );
    }

    #[test]
    fn local_whisper_rejects_invalid_url() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.url = "not-a-url".to_string();
        let err = LocalWhisperProvider::from_config(&cfg).err().unwrap();
        assert!(err.to_string().contains("invalid `url`"), "got: {err}");
    }

    #[test]
    fn local_whisper_rejects_non_http_url() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.url = "ftp://10.10.0.1:8001/v1/transcribe".to_string();
        let err = LocalWhisperProvider::from_config(&cfg).err().unwrap();
        assert!(err.to_string().contains("http or https"), "got: {err}");
    }

    #[test]
    fn local_whisper_rejects_empty_bearer_token() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.bearer_token = Some(String::new());
        let err = LocalWhisperProvider::from_config(&cfg).err().unwrap();
        assert!(
            err.to_string().contains("`bearer_token` must not be empty"),
            "got: {err}"
        );
    }

    #[test]
    fn local_whisper_rejects_missing_bearer_token() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.bearer_token = None;
        let err = LocalWhisperProvider::from_config(&cfg).err().unwrap();
        assert!(
            err.to_string().contains("`bearer_token` must be set"),
            "got: {err}"
        );
    }

    #[test]
    fn local_whisper_rejects_zero_max_audio_bytes() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.max_audio_bytes = 0;
        let err = LocalWhisperProvider::from_config(&cfg).err().unwrap();
        assert!(
            err.to_string()
                .contains("`max_audio_bytes` must be greater than zero"),
            "got: {err}"
        );
    }

    #[test]
    fn local_whisper_rejects_zero_timeout() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.timeout_secs = 0;
        let err = LocalWhisperProvider::from_config(&cfg).err().unwrap();
        assert!(
            err.to_string()
                .contains("`timeout_secs` must be greater than zero"),
            "got: {err}"
        );
    }

    #[test]
    fn local_whisper_registered_when_config_present() {
        let config = TranscriptionConfig {
            local_whisper: Some(local_whisper_config("http://127.0.0.1:9999/v1/transcribe")),
            default_provider: "local_whisper".to_string(),
            ..TranscriptionConfig::default()
        };

        let manager = TranscriptionManager::new(&config).unwrap();
        assert!(
            manager.available_providers().contains(&"local_whisper"),
            "expected local_whisper in {:?}",
            manager.available_providers()
        );
    }

    #[test]
    fn local_whisper_misconfigured_section_fails_manager_construction() {
        // A misconfigured local_whisper section logs a warning and skips
        // registration. When local_whisper is also the default_provider and
        // transcription is enabled, the safety net in TranscriptionManager
        // surfaces the error: "not configured".
        let mut bad_cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        bad_cfg.bearer_token = Some(String::new());
        let config = TranscriptionConfig {
            local_whisper: Some(bad_cfg),
            enabled: true,
            default_provider: "local_whisper".to_string(),
            ..TranscriptionConfig::default()
        };

        let err = TranscriptionManager::new(&config).err().unwrap();
        assert!(
            err.to_string().contains("not configured"),
            "expected 'not configured' from manager safety net, got: {err}"
        );
    }

    #[test]
    fn validate_audio_still_enforces_25mb_cap() {
        // Regression: extracting resolve_audio_format() must not weaken validate_audio().
        let at_limit = vec![0u8; MAX_AUDIO_BYTES];
        assert!(validate_audio(&at_limit, "test.ogg").is_ok());
        let over_limit = vec![0u8; MAX_AUDIO_BYTES + 1];
        let err = validate_audio(&over_limit, "test.ogg").unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[tokio::test]
    async fn local_whisper_rejects_oversized_audio() {
        let cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        let provider = LocalWhisperProvider::from_config(&cfg).unwrap();
        let big = vec![0u8; cfg.max_audio_bytes + 1];
        let err = provider.transcribe(&big, "voice.ogg").await.unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[tokio::test]
    async fn local_whisper_rejects_unsupported_format() {
        let cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        let provider = LocalWhisperProvider::from_config(&cfg).unwrap();
        let data = vec![0u8; 100];
        let err = provider.transcribe(&data, "voice.aiff").await.unwrap_err();
        assert!(
            err.to_string().contains("Unsupported audio format"),
            "got: {err}"
        );
    }

    // ── LocalWhisperProvider HTTP mock tests ────────────────────

    #[tokio::test]
    async fn local_whisper_returns_text_from_response() {
        use wiremock::matchers::{header_exists, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .and(header_exists("authorization"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"text": "hello world"})),
            )
            .mount(&server)
            .await;

        let cfg = local_whisper_config(&format!("{}/v1/transcribe", server.uri()));
        let provider = LocalWhisperProvider::from_config(&cfg).unwrap();

        let result = provider
            .transcribe(b"fake-audio", "voice.ogg")
            .await
            .unwrap();
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn local_whisper_sends_bearer_auth_header() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "auth ok"})),
            )
            .mount(&server)
            .await;

        let cfg = local_whisper_config(&format!("{}/v1/transcribe", server.uri()));
        let provider = LocalWhisperProvider::from_config(&cfg).unwrap();

        let result = provider
            .transcribe(b"fake-audio", "voice.ogg")
            .await
            .unwrap();
        assert_eq!(result, "auth ok");
    }

    #[tokio::test]
    async fn local_whisper_propagates_http_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .respond_with(
                ResponseTemplate::new(503).set_body_json(
                    serde_json::json!({"error": {"message": "service unavailable"}}),
                ),
            )
            .mount(&server)
            .await;

        let cfg = local_whisper_config(&format!("{}/v1/transcribe", server.uri()));
        let provider = LocalWhisperProvider::from_config(&cfg).unwrap();

        let err = provider
            .transcribe(b"fake-audio", "voice.ogg")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("503") || err.to_string().contains("service unavailable"),
            "expected HTTP error, got: {err}"
        );
    }

    #[tokio::test]
    async fn local_whisper_propagates_non_json_http_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .respond_with(
                ResponseTemplate::new(502)
                    .set_body_string("Bad Gateway")
                    .insert_header("content-type", "text/plain"),
            )
            .mount(&server)
            .await;

        let cfg = local_whisper_config(&format!("{}/v1/transcribe", server.uri()));
        let provider = LocalWhisperProvider::from_config(&cfg).unwrap();

        let err = provider
            .transcribe(b"fake-audio", "voice.ogg")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("502"), "got: {err}");
        assert!(
            err.to_string().contains("Bad Gateway"),
            "expected plain-text body in error, got: {err}"
        );
    }
}
