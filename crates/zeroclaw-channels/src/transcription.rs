use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use reqwest::multipart::{Form, Part};

use zeroclaw_config::providers::{TranscriptionProviderEntry, TranscriptionProviders};
use zeroclaw_config::schema::{Config, TranscriptionConfig};

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
/// Groq validates the filename extension — `.oga` (Opus-in-Ogg) is not in
/// its accepted list, so we rewrite it to `.ogg`.
fn normalize_audio_filename(file_name: &str) -> String {
    match file_name.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("oga") => format!("{stem}.ogg"),
        _ => file_name.to_string(),
    }
}

/// Resolve MIME type and normalize filename from extension.
/// No size check — callers enforce their own limits.
fn resolve_audio_format(file_name: &str) -> Result<(String, &'static str)> {
    let normalized_name = normalize_audio_filename(file_name);
    let extension = normalized_name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .unwrap_or("");
    let mime = mime_for_audio(extension).ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"extension": extension})),
            "transcription: unsupported audio format"
        );
        anyhow::Error::msg(format!(
            "Unsupported audio format '.{extension}'. \
             accepted: flac, mp3, mp4, mpeg, mpga, m4a, ogg, opus, wav, webm"
        ))
    })?;
    Ok((normalized_name, mime))
}

/// Validate audio data and resolve MIME type from file name.
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

/// Trait for speech-to-text transcription_provider implementations.
#[async_trait]
pub trait TranscriptionProvider: Send + Sync + ::zeroclaw_api::attribution::Attributable {
    /// Human-readable transcription_provider name (e.g. "groq", "openai").
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

/// Groq Whisper API transcription_provider (default, backward-compatible with existing config).
pub struct GroqProvider {
    alias: String,
    api_url: String,
    model: String,
    api_key: String,
    language: Option<String>,
}

impl GroqProvider {
    pub fn from_config(alias: &str, config: &TranscriptionConfig) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .context(
                "Missing transcription API key: set `[transcription].api_key` (or via the \
                 schema-mirror grammar `ZEROCLAW_transcription__api_key=...`).",
            )?;

        Ok(Self {
            alias: alias.to_string(),
            api_url: config.api_url.clone(),
            model: config.model.clone(),
            api_key,
            language: config.language.clone(),
        })
    }

    /// Build from a typed `[providers.transcription.groq.<alias>]` entry.
    pub fn from_typed_config(
        alias: &str,
        cfg: &zeroclaw_config::schema::GroqTranscriptionProviderConfig,
    ) -> Result<Self> {
        let api_key = cfg
            .base
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Missing API key for [providers.transcription.groq.{alias}]"
                ))
            })?;
        Ok(Self {
            alias: alias.to_string(),
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: cfg
                .model
                .clone()
                .filter(|model| !model.trim().is_empty())
                .unwrap_or_else(|| "whisper-large-v3-turbo".to_string()),
            api_key,
            language: cfg.base.language.clone(),
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

/// OpenAI Whisper API transcription_provider.
pub struct OpenAiWhisperProvider {
    alias: String,
    api_key: String,
    model: String,
}

impl OpenAiWhisperProvider {
    pub fn from_config(
        alias: &str,
        config: &zeroclaw_config::schema::OpenAiSttConfig,
    ) -> Result<Self> {
        // 1. 显式配置字段优先
        if let Some(api_key) = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Ok(Self {
                alias: alias.to_string(),
                api_key: api_key.to_string(),
                model: config.model.clone(),
            });
        }

        // 2. 环境变量回退: TRANSCRIPTION_API_KEY (专用)
        if let Ok(key) = std::env::var("TRANSCRIPTION_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                return Ok(Self {
                    alias: alias.to_string(),
                    api_key: trimmed.to_string(),
                    model: config.model.clone(),
                });
            }
        }

        // 3. 环境变量回退: OPENAI_API_KEY (通用)
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                return Ok(Self {
                    alias: alias.to_string(),
                    api_key: trimmed.to_string(),
                    model: config.model.clone(),
                });
            }
        }

        // 4. 全部失败
        anyhow::bail!(
            "Missing OpenAI STT API key. Set either:\n\
             • [transcription.openai].api_key in config.toml\n\
             • TRANSCRIPTION_API_KEY environment variable\n\
             • OPENAI_API_KEY environment variable"
        );
    }

    /// Build from a typed `[providers.transcription.openai.<alias>]` entry.
    pub fn from_typed_config(
        alias: &str,
        cfg: &zeroclaw_config::schema::OpenAiTranscriptionProviderConfig,
    ) -> Result<Self> {
        // 1. 显式配置字段优先
        if let Some(api_key) = cfg
            .base
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Missing API key for [providers.transcription.openai.{alias}]"
                ))
            })?;
        Ok(Self {
            alias: alias.to_string(),
            api_key,
            model: cfg
                .model
                .clone()
                .filter(|model| !model.trim().is_empty())
                .unwrap_or_else(|| "whisper-1".to_string()),
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

/// Deepgram STT API transcription_provider.
pub struct DeepgramProvider {
    alias: String,
    api_key: String,
    model: String,
}

impl DeepgramProvider {
    pub fn from_config(
        alias: &str,
        config: &zeroclaw_config::schema::DeepgramSttConfig,
    ) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .context("Missing Deepgram API key: set [transcription.deepgram].api_key")?;

        Ok(Self {
            alias: alias.to_string(),
            api_key,
            model: config.model.clone(),
        })
    }

    /// Build from a typed `[providers.transcription.deepgram.<alias>]` entry.
    pub fn from_typed_config(
        alias: &str,
        cfg: &zeroclaw_config::schema::DeepgramTranscriptionProviderConfig,
    ) -> Result<Self> {
        let api_key = cfg
            .base
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Missing API key for [providers.transcription.deepgram.{alias}]"
                ))
            })?;
        Ok(Self {
            alias: alias.to_string(),
            api_key,
            model: cfg
                .model
                .clone()
                .filter(|model| !model.trim().is_empty())
                .unwrap_or_else(|| "nova-2".to_string()),
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

/// AssemblyAI STT API transcription_provider.
pub struct AssemblyAiProvider {
    alias: String,
    api_key: String,
}

impl AssemblyAiProvider {
    pub fn from_config(
        alias: &str,
        config: &zeroclaw_config::schema::AssemblyAiSttConfig,
    ) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .context("Missing AssemblyAI API key: set [transcription.assemblyai].api_key")?;

        Ok(Self {
            alias: alias.to_string(),
            api_key,
        })
    }

    /// Build from a typed `[providers.transcription.assemblyai.<alias>]` entry.
    pub fn from_typed_config(
        alias: &str,
        cfg: &zeroclaw_config::schema::AssemblyAiTranscriptionProviderConfig,
    ) -> Result<Self> {
        let api_key = cfg
            .base
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Missing API key for [providers.transcription.assemblyai.{alias}]"
                ))
            })?;
        Ok(Self {
            alias: alias.to_string(),
            api_key,
        })
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

/// Google Cloud Speech-to-Text API transcription_provider.
pub struct GoogleSttProvider {
    alias: String,
    api_key: String,
    language_code: String,
}

impl GoogleSttProvider {
    pub fn from_config(
        alias: &str,
        config: &zeroclaw_config::schema::GoogleSttConfig,
    ) -> Result<Self> {
        let api_key = config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .context("Missing Google STT API key: set [transcription.google].api_key")?;

        Ok(Self {
            alias: alias.to_string(),
            api_key,
            language_code: config.language_code.clone(),
        })
    }

    /// Build from a typed `[providers.transcription.google.<alias>]` entry.
    pub fn from_typed_config(
        alias: &str,
        cfg: &zeroclaw_config::schema::GoogleTranscriptionProviderConfig,
    ) -> Result<Self> {
        let api_key = cfg
            .base
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "Missing API key for [providers.transcription.google.{alias}]"
                ))
            })?;
        Ok(Self {
            alias: alias.to_string(),
            api_key,
            language_code: cfg
                .base
                .language
                .clone()
                .filter(|language| !language.trim().is_empty())
                .unwrap_or_else(|| "en-US".to_string()),
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

// ── LocalWhisperProvider ────────────────────────────────────────

pub struct LocalWhisperProvider {
    alias: String,
    url: String,
    bearer_token: String,
    max_audio_bytes: usize,
    timeout_secs: u64,
}

impl LocalWhisperProvider {
    /// Build from config. Fails if `url` or `bearer_token` is empty, if `url`
    /// is not a valid HTTP/HTTPS URL (scheme must be `http` or `https`), if
    /// `max_audio_bytes` is zero, or if `timeout_secs` is zero.
    pub fn from_config(
        alias: &str,
        config: &zeroclaw_config::schema::LocalWhisperConfig,
    ) -> Result<Self> {
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
            alias: alias.to_string(),
            url,
            bearer_token,
            max_audio_bytes: config.max_audio_bytes,
            timeout_secs: config.timeout_secs,
        })
    }

    /// Build from a typed `[providers.transcription.local_whisper.<alias>]` entry.
    /// Delegates validation to `from_config` via a bridge — the typed config
    /// uses `uri` instead of `url` but is otherwise identical.
    pub fn from_typed_config(
        alias: &str,
        cfg: &zeroclaw_config::schema::LocalWhisperTranscriptionProviderConfig,
    ) -> Result<Self> {
        let bridge = zeroclaw_config::schema::LocalWhisperConfig {
            url: cfg.uri.clone(),
            bearer_token: cfg.bearer_token.clone(),
            max_audio_bytes: cfg.max_audio_bytes,
            timeout_secs: cfg.timeout_secs,
        };
        Self::from_config(alias, &bridge)
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

/// Manages multiple transcription / STT providers and routes transcription
/// requests. The manager is implicitly per-agent: the runtime-active
/// agent's `transcription_provider` reference is the resolved alias for
/// `transcribe()` calls. there is no global default-provider concept.
pub struct TranscriptionManager {
    transcription_providers: HashMap<String, Box<dyn TranscriptionProvider>>,
    max_audio_bytes: Option<usize>,
    /// Resolved alias for the agent that owns this manager. Empty when
    /// the agent has no transcription preference (opt-out).
    agent_transcription_provider: String,
}

impl TranscriptionManager {
    /// Empty manager with no providers. Used as a base when only typed
    /// `[providers.transcription.<family>.<alias>]` config is present and
    /// there is no legacy `[transcription]` block to seed from.
    pub fn empty() -> Self {
        Self {
            transcription_providers: HashMap::new(),
            max_audio_bytes: None,
            agent_transcription_provider: String::new(),
        }
    }

    /// Build a `TranscriptionManager` from a `TranscriptionConfig`. The
    /// resolved agent alias starts empty; orchestrators that wire the
    /// manager to a specific agent should call
    /// `with_agent_transcription_provider` to set it.
    pub fn new(config: &TranscriptionConfig) -> Result<Self> {
        if matches!(config.max_audio_bytes, Some(0)) {
            bail!("transcription.max_audio_bytes must be greater than zero");
        }

        let mut transcription_providers: HashMap<String, Box<dyn TranscriptionProvider>> =
            HashMap::new();

        Self::register_legacy_providers(&mut transcription_providers, config);

        if config.enabled && transcription_providers.is_empty() {
            bail!(
                "Transcription is enabled but no transcription provider registered \
                 successfully. Configure at least one of: [transcription] (Groq) \
                 with api_key + api_url; [transcription.openai]; [transcription.deepgram]; \
                 [transcription.assemblyai]; [transcription.google]; [transcription.local_whisper]; \
                 or [providers.transcription.<type>.<alias>]."
            );
        }

        Ok(Self {
            transcription_providers,
            max_audio_bytes: config.max_audio_bytes,
            agent_transcription_provider: String::new(),
        })
    }

    pub fn from_config_for_agent(config: &Config, agent_alias: Option<&str>) -> Result<Self> {
        if matches!(config.transcription.max_audio_bytes, Some(0)) {
            bail!("transcription.max_audio_bytes must be greater than zero");
        }

        let mut transcription_providers: HashMap<String, Box<dyn TranscriptionProvider>> =
            HashMap::new();

        Self::register_legacy_providers(&mut transcription_providers, &config.transcription);
        Self::register_typed_providers(
            &mut transcription_providers,
            &config.providers.transcription,
        );

        if config.transcription.enabled && transcription_providers.is_empty() {
            bail!(
                "Transcription is enabled but no transcription provider registered \
                 successfully. Configure at least one of: [providers.transcription.<type>.<alias>], \
                 [transcription] (Groq) with api_key + api_url, [transcription.openai], \
                 [transcription.deepgram], [transcription.assemblyai], [transcription.google], \
                 or [transcription.local_whisper]."
            );
        }

        let agent_transcription_provider = agent_alias
            .or_else(|| config.resolved_runtime_agent_alias())
            .and_then(|alias| config.agents.get(alias))
            .map(|a| a.transcription_provider.as_str().to_string())
            .unwrap_or_default();

        Ok(Self {
            transcription_providers,
            max_audio_bytes: config.transcription.max_audio_bytes,
            agent_transcription_provider,
        })
    }

    fn register_legacy_providers(
        transcription_providers: &mut HashMap<String, Box<dyn TranscriptionProvider>>,
        config: &TranscriptionConfig,
    ) {
        if let Ok(groq) = GroqProvider::from_config("groq", config) {
            transcription_providers.insert("groq".to_string(), Box::new(groq));
        }

        if let Some(ref openai_cfg) = config.openai
            && let Ok(p) = OpenAiWhisperProvider::from_config("openai", openai_cfg)
        {
            transcription_providers.insert("openai".to_string(), Box::new(p));
        }

        if let Some(ref deepgram_cfg) = config.deepgram
            && let Ok(p) = DeepgramProvider::from_config("deepgram", deepgram_cfg)
        {
            transcription_providers.insert("deepgram".to_string(), Box::new(p));
        }

        if let Some(ref assemblyai_cfg) = config.assemblyai
            && let Ok(p) = AssemblyAiProvider::from_config("assemblyai", assemblyai_cfg)
        {
            transcription_providers.insert("assemblyai".to_string(), Box::new(p));
        }

        if let Some(ref google_cfg) = config.google
            && let Ok(p) = GoogleSttProvider::from_config("google", google_cfg)
        {
            transcription_providers.insert("google".to_string(), Box::new(p));
        }

        if let Some(ref local_cfg) = config.local_whisper {
            match LocalWhisperProvider::from_config("local_whisper", local_cfg) {
                Ok(p) => {
                    transcription_providers.insert("local_whisper".to_string(), Box::new(p));
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "local_whisper config invalid, provider skipped"
                    );
                }
            }
        }
    }

    fn register_typed_providers(
        transcription_providers: &mut HashMap<String, Box<dyn TranscriptionProvider>>,
        typed: &TranscriptionProviders,
    ) {
        for (family, alias, entry) in typed.iter_entries() {
            let dotted = format!("{family}.{alias}");
            if transcription_providers.contains_key(&dotted) {
                continue;
            }
            let result: Result<Box<dyn TranscriptionProvider>> = match entry {
                TranscriptionProviderEntry::Groq(provider_config) => {
                    GroqProvider::from_typed_config(alias, provider_config)
                        .map(|provider| Box::new(provider) as _)
                }
                TranscriptionProviderEntry::OpenAi(provider_config) => {
                    OpenAiWhisperProvider::from_typed_config(alias, provider_config)
                        .map(|provider| Box::new(provider) as _)
                }
                TranscriptionProviderEntry::Deepgram(provider_config) => {
                    DeepgramProvider::from_typed_config(alias, provider_config)
                        .map(|provider| Box::new(provider) as _)
                }
                TranscriptionProviderEntry::AssemblyAi(provider_config) => {
                    AssemblyAiProvider::from_typed_config(alias, provider_config)
                        .map(|provider| Box::new(provider) as _)
                }
                TranscriptionProviderEntry::Google(provider_config) => {
                    GoogleSttProvider::from_typed_config(alias, provider_config)
                        .map(|provider| Box::new(provider) as _)
                }
                TranscriptionProviderEntry::LocalWhisper(provider_config) => {
                    LocalWhisperProvider::from_typed_config(alias, provider_config)
                        .map(|provider| Box::new(provider) as _)
                }
            };

            match result {
                Ok(provider) => {
                    transcription_providers.insert(dotted, provider);
                }
                Err(e) => {
                    let config_path = format!("[providers.transcription.{dotted}]");
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": e.to_string(), "config_path": config_path})
                            ),
                        "typed transcription provider skipped (config error)"
                    );
                }
            }
        }
    }

    #[must_use]
    pub fn with_typed_providers(mut self, typed: &TranscriptionProviders) -> Self {
        Self::register_typed_providers(&mut self.transcription_providers, typed);
        self
    }

    /// Set the resolved agent `transcription_provider` alias. Called by
    /// orchestrators that bind this manager to a specific agent at startup.
    /// Subsequent `transcribe` calls dispatch to this alias.
    #[must_use]
    pub fn with_agent_transcription_provider(mut self, alias: impl Into<String>) -> Self {
        self.agent_transcription_provider = alias.into();
        self
    }

    /// Transcribe audio using the runtime-active agent's resolved
    /// `transcription_provider`. Fails loud when the agent has no
    /// transcription_provider configured — there is no global default.
    pub async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        let provider_alias = self.agent_transcription_provider.as_str();
        if provider_alias.is_empty() {
            bail!(
                "Agent has no transcription_provider configured. Set \
                 `agent.<alias>.transcription_provider = \"<type>.<alias>\"` \
                 referencing a configured transcription provider."
            );
        }
        self.transcribe_with_provider(audio_data, file_name, provider_alias)
            .await
    }

    /// Transcribe audio using a specific named transcription_provider.
    pub async fn transcribe_with_provider(
        &self,
        audio_data: &[u8],
        file_name: &str,
        transcription_provider: &str,
    ) -> Result<String> {
        let p = self.transcription_providers.get(transcription_provider).ok_or_else(|| {
            let available: Vec<&str> = self.transcription_providers.keys().map(|k| k.as_str()).collect();
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "transcription_provider": transcription_provider,
                        "available": available,
                    })),
                "transcription: provider not configured"
            );
            anyhow::Error::msg(format!(
                "Transcription transcription_provider '{transcription_provider}' not configured. Available: {available:?}"
            ))
        })?;

        self.enforce_global_audio_limit(audio_data)?;

        use ::zeroclaw_log::Instrument;
        let span = ::zeroclaw_log::attribution_span!(p.as_ref());
        p.transcribe(audio_data, file_name).instrument(span).await
    }

    fn enforce_global_audio_limit(&self, audio_data: &[u8]) -> Result<()> {
        if let Some(max_audio_bytes) = self.max_audio_bytes
            && audio_data.len() > max_audio_bytes
        {
            bail!(
                "Audio file too large ({} bytes, global max {})",
                audio_data.len(),
                max_audio_bytes
            );
        }
        Ok(())
    }

    /// List registered transcription_provider names.
    pub fn available_providers(&self) -> Vec<&str> {
        self.transcription_providers
            .keys()
            .map(|k| k.as_str())
            .collect()
    }
}

impl ::zeroclaw_api::attribution::Attributable for GroqProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Transcription(
                ::zeroclaw_api::attribution::TranscriptionProviderKind::Groq,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for OpenAiWhisperProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Transcription(
                ::zeroclaw_api::attribution::TranscriptionProviderKind::OpenAi,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for DeepgramProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Transcription(
                ::zeroclaw_api::attribution::TranscriptionProviderKind::Deepgram,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for AssemblyAiProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Transcription(
                ::zeroclaw_api::attribution::TranscriptionProviderKind::AssemblyAi,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for GoogleSttProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Transcription(
                ::zeroclaw_api::attribution::TranscriptionProviderKind::Google,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

impl ::zeroclaw_api::attribution::Attributable for LocalWhisperProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Transcription(
                ::zeroclaw_api::attribution::TranscriptionProviderKind::Whisper,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct StaticTranscriptionProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl TranscriptionProvider for StaticTranscriptionProvider {
        fn name(&self) -> &str {
            "static"
        }

        async fn transcribe(&self, _audio_data: &[u8], _file_name: &str) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("under cap".to_string())
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for StaticTranscriptionProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Transcription(
                    ::zeroclaw_api::attribution::TranscriptionProviderKind::Groq,
                ),
            )
        }

        fn alias(&self) -> &str {
            "static"
        }
    }

    fn manager_with_static_provider(
        max_audio_bytes: Option<usize>,
    ) -> (TranscriptionManager, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut transcription_providers: HashMap<String, Box<dyn TranscriptionProvider>> =
            HashMap::new();
        transcription_providers.insert(
            "static".to_string(),
            Box::new(StaticTranscriptionProvider {
                calls: Arc::clone(&calls),
            }),
        );
        (
            TranscriptionManager {
                transcription_providers,
                max_audio_bytes,
                agent_transcription_provider: String::new(),
            },
            calls,
        )
    }

    // Tests for the deleted `transcribe_audio` free function were removed
    // alongside the function in Equivalent coverage lives on
    // `TranscriptionManager` (`manager_creation_with_default_config`,
    // `manager_registers_groq_with_key`, `manager_rejects_unconfigured_provider`).

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

    #[test]
    fn rejects_unsupported_audio_format() {
        // Without the legacy `transcribe_audio` free function, exercise the
        // format-rejection path directly via `validate_audio`.
        let data = vec![0u8; 100];
        let err = validate_audio(&data, "recording.aac").unwrap_err();
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
        // the manager's agent_transcription_provider starts empty
        // until an orchestrator wires it via `with_agent_transcription_provider`.
        // No global default-provider concept.
        assert!(manager.agent_transcription_provider.is_empty());
        // Groq won't be registered without a key.
        assert!(manager.transcription_providers.is_empty());
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
        assert!(manager.transcription_providers.contains_key("groq"));
        assert_eq!(manager.transcription_providers["groq"].name(), "groq");
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
        assert!(manager.transcription_providers.contains_key("groq"));
        assert!(manager.transcription_providers.contains_key("openai"));
        assert!(manager.transcription_providers.contains_key("deepgram"));
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
    fn manager_agent_transcription_provider_via_setter() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        let config = TranscriptionConfig {
            openai: Some(zeroclaw_config::schema::OpenAiSttConfig {
                api_key: Some("test-openai-key".to_string()),
                model: "whisper-1".to_string(),
            }),
            ..TranscriptionConfig::default()
        };

        let manager = TranscriptionManager::new(&config)
            .unwrap()
            .with_agent_transcription_provider("openai");
        assert_eq!(manager.agent_transcription_provider, "openai");
    }

    #[test]
    fn manager_from_config_for_agent_registers_dotted_provider_refs() {
        let mut config = zeroclaw_config::schema::Config {
            transcription: TranscriptionConfig {
                enabled: true,
                ..TranscriptionConfig::default()
            },
            ..zeroclaw_config::schema::Config::default()
        };
        config.providers.transcription.groq.insert(
            "default".to_string(),
            zeroclaw_config::schema::GroqTranscriptionProviderConfig {
                base: zeroclaw_config::schema::TranscriptionProviderConfig {
                    api_key: Some("test-groq-key".to_string()),
                    ..zeroclaw_config::schema::TranscriptionProviderConfig::default()
                },
                ..zeroclaw_config::schema::GroqTranscriptionProviderConfig::default()
            },
        );
        config.agents.insert(
            "default".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                transcription_provider: "groq.default".into(),
                ..zeroclaw_config::schema::AliasedAgentConfig::default()
            },
        );

        let manager = TranscriptionManager::from_config_for_agent(&config, None).unwrap();

        assert_eq!(manager.agent_transcription_provider, "groq.default");
        assert!(manager.available_providers().contains(&"groq.default"));
    }

    #[test]
    fn typed_registration_logs_all_family_errors_across_entry_points() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let mut typed = TranscriptionProviders::default();
        typed.groq.insert(
            "invalid".to_string(),
            zeroclaw_config::schema::GroqTranscriptionProviderConfig::default(),
        );
        typed.openai.insert(
            "invalid".to_string(),
            zeroclaw_config::schema::OpenAiTranscriptionProviderConfig::default(),
        );
        typed.deepgram.insert(
            "invalid".to_string(),
            zeroclaw_config::schema::DeepgramTranscriptionProviderConfig::default(),
        );
        typed.assemblyai.insert(
            "invalid".to_string(),
            zeroclaw_config::schema::AssemblyAiTranscriptionProviderConfig::default(),
        );
        typed.google.insert(
            "invalid".to_string(),
            zeroclaw_config::schema::GoogleTranscriptionProviderConfig::default(),
        );
        typed.local_whisper.insert(
            "invalid".to_string(),
            zeroclaw_config::schema::LocalWhisperTranscriptionProviderConfig::default(),
        );

        let assert_events = |entry_point: &str, events: Vec<serde_json::Value>| {
            let expected = [
                ("groq.invalid", "[providers.transcription.groq.invalid]"),
                ("openai.invalid", "[providers.transcription.openai.invalid]"),
                (
                    "deepgram.invalid",
                    "[providers.transcription.deepgram.invalid]",
                ),
                (
                    "assemblyai.invalid",
                    "[providers.transcription.assemblyai.invalid]",
                ),
                ("google.invalid", "[providers.transcription.google.invalid]"),
                (
                    "local_whisper.invalid",
                    "local_whisper: `url` must not be empty",
                ),
            ];

            for (provider, error_fragment) in expected {
                let config_path = format!("[providers.transcription.{provider}]");
                let event = events
                    .iter()
                    .find(|value| value["attributes"]["config_path"] == config_path)
                    .unwrap_or_else(|| {
                        panic!("{entry_point} should log a warning for {provider}: {events:?}")
                    });
                assert_eq!(
                    event["message"], "typed transcription provider skipped (config error)",
                    "provider: {provider}"
                );
                assert!(
                    event["attributes"].get("provider").is_none(),
                    "{entry_point} must not emit provider attribution as a call-site attribute: {event}"
                );
                assert!(
                    event["attributes"]["error"]
                        .as_str()
                        .is_some_and(|error| error.contains(error_fragment)),
                    "{entry_point} should include the remediation error for {provider}: {event}"
                );
            }
        };

        let mut config = zeroclaw_config::schema::Config::default();
        config.providers.transcription = typed.clone();
        TranscriptionManager::from_config_for_agent(&config, None)
            .expect("invalid typed providers should be skipped when transcription is disabled");
        assert_events(
            "config manager",
            std::iter::from_fn(|| rx.try_recv().ok()).collect(),
        );

        let _manager = TranscriptionManager::empty().with_typed_providers(&typed);
        assert_events(
            "builder",
            std::iter::from_fn(|| rx.try_recv().ok()).collect(),
        );
    }

    #[test]
    fn typed_registration_defaults_blank_optional_values() {
        let base = zeroclaw_config::schema::TranscriptionProviderConfig {
            api_key: Some("test-key".to_string()),
            ..zeroclaw_config::schema::TranscriptionProviderConfig::default()
        };

        let groq = GroqProvider::from_typed_config(
            "default",
            &zeroclaw_config::schema::GroqTranscriptionProviderConfig {
                base: base.clone(),
                model: Some("   ".to_string()),
            },
        )
        .unwrap();
        assert_eq!(groq.model, "whisper-large-v3-turbo");

        let openai = OpenAiWhisperProvider::from_typed_config(
            "default",
            &zeroclaw_config::schema::OpenAiTranscriptionProviderConfig {
                base: base.clone(),
                model: Some(String::new()),
            },
        )
        .unwrap();
        assert_eq!(openai.model, "whisper-1");

        let deepgram = DeepgramProvider::from_typed_config(
            "default",
            &zeroclaw_config::schema::DeepgramTranscriptionProviderConfig {
                base: base.clone(),
                model: Some("\t".to_string()),
            },
        )
        .unwrap();
        assert_eq!(deepgram.model, "nova-2");

        let google = GoogleSttProvider::from_typed_config(
            "default",
            &zeroclaw_config::schema::GoogleTranscriptionProviderConfig {
                base: zeroclaw_config::schema::TranscriptionProviderConfig {
                    language: Some("  ".to_string()),
                    ..base
                },
            },
        )
        .unwrap();
        assert_eq!(google.language_code, "en-US");
    }

    #[test]
    fn manager_rejects_zero_global_max_audio_bytes() {
        let config = TranscriptionConfig {
            max_audio_bytes: Some(0),
            ..TranscriptionConfig::default()
        };

        let err = match TranscriptionManager::new(&config) {
            Ok(_) => panic!("expected zero max_audio_bytes to fail manager construction"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("transcription.max_audio_bytes must be greater than zero"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn manager_global_max_audio_bytes_rejects_over_limit_before_provider_dispatch() {
        let (manager, calls) = manager_with_static_provider(Some(3));
        let err = manager
            .transcribe_with_provider(&[0u8; 4], "voice.ogg", "static")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Audio file too large"),
            "got: {err}"
        );
        assert!(err.to_string().contains("global max 3"), "got: {err}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn manager_global_max_audio_bytes_allows_exact_limit() {
        let (manager, calls) = manager_with_static_provider(Some(4));
        let result = manager
            .transcribe_with_provider(&[0u8; 4], "voice.ogg", "static")
            .await
            .unwrap();
        assert_eq!(result, "under cap");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn manager_transcribe_enforces_global_max_audio_bytes() {
        let (manager, calls) = manager_with_static_provider(Some(2));
        let manager = manager.with_agent_transcription_provider("static");
        let err = manager
            .transcribe(&[0u8; 3], "voice.ogg")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Audio file too large"),
            "got: {err}"
        );
        assert!(err.to_string().contains("global max 2"), "got: {err}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
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
        // TranscriptionConfig has no global default-provider field;
        // per-agent `transcription_provider` is the only selector.
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
        let err = LocalWhisperProvider::from_config("local_whisper", &cfg)
            .err()
            .unwrap();
        assert!(
            err.to_string().contains("`url` must not be empty"),
            "got: {err}"
        );
    }

    #[test]
    fn local_whisper_rejects_invalid_url() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.url = "not-a-url".to_string();
        let err = LocalWhisperProvider::from_config("local_whisper", &cfg)
            .err()
            .unwrap();
        assert!(err.to_string().contains("invalid `url`"), "got: {err}");
    }

    #[test]
    fn local_whisper_rejects_non_http_url() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.url = "ftp://10.10.0.1:8001/v1/transcribe".to_string();
        let err = LocalWhisperProvider::from_config("local_whisper", &cfg)
            .err()
            .unwrap();
        assert!(err.to_string().contains("http or https"), "got: {err}");
    }

    #[test]
    fn local_whisper_rejects_empty_bearer_token() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.bearer_token = Some(String::new());
        let err = LocalWhisperProvider::from_config("local_whisper", &cfg)
            .err()
            .unwrap();
        assert!(
            err.to_string().contains("`bearer_token` must not be empty"),
            "got: {err}"
        );
    }

    #[test]
    fn local_whisper_rejects_missing_bearer_token() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.bearer_token = None;
        let err = LocalWhisperProvider::from_config("local_whisper", &cfg)
            .err()
            .unwrap();
        assert!(
            err.to_string().contains("`bearer_token` must be set"),
            "got: {err}"
        );
    }

    #[test]
    fn local_whisper_rejects_zero_max_audio_bytes() {
        let mut cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        cfg.max_audio_bytes = 0;
        let err = LocalWhisperProvider::from_config("local_whisper", &cfg)
            .err()
            .unwrap();
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
        let err = LocalWhisperProvider::from_config("local_whisper", &cfg)
            .err()
            .unwrap();
        assert!(
            err.to_string()
                .contains("`timeout_secs` must be greater than zero"),
            "got: {err}"
        );
    }

    // ── LocalWhisper `Default` must use the serde-default values, not
    //    the Rust `usize`/`u64` zeros. `#[serde(default = "...")]` only
    //    fires for deserialization; without a manual `Default` impl that
    //    delegates to the helpers, `Config::init_defaults` materializes
    //    `Some(LocalWhisperConfig { max_audio_bytes: 0, timeout_secs: 0,
    //    .. })`, the parent `[transcription]` block is poisoned at load,
    //    and `transcription.enabled` silently flips to `false`
    //    regardless of operator intent.

    #[test]
    fn local_whisper_default_uses_serde_defaults_not_rust_zero() {
        let cfg = zeroclaw_config::schema::LocalWhisperConfig::default();
        assert_eq!(
            cfg.max_audio_bytes,
            25 * 1024 * 1024,
            "Rust default must reuse the serde-default value (25 MB); got {}",
            cfg.max_audio_bytes
        );
        assert_eq!(
            cfg.timeout_secs, 300,
            "Rust default must reuse the serde-default value (300 s); got {}",
            cfg.timeout_secs
        );
        assert_eq!(
            cfg.bearer_token, None,
            "bearer_token stays None by default (unauthenticated local endpoint)"
        );
        assert!(
            cfg.url.is_empty(),
            "url stays empty (no working endpoint at config-init time)"
        );
    }

    #[test]
    fn local_whisper_provider_accepts_config_init_default_after_url_and_token_filled() {
        // Mirrors the post-init state from a `zeroclaw config init
        // transcription.local_whisper` followed by the operator setting
        // `url` and `bearer_token`: the scaffolded `max_audio_bytes` /
        // `timeout_secs` defaults must already be valid, so from_config
        // succeeds without manual adjustment.
        let cfg = zeroclaw_config::schema::LocalWhisperConfig {
            url: "http://127.0.0.1:9999/v1/transcribe".to_string(),
            bearer_token: Some("test-token".to_string()),
            ..zeroclaw_config::schema::LocalWhisperConfig::default()
        };

        let provider = LocalWhisperProvider::from_config("local_whisper", &cfg)
            .expect("config-init default must be loadable once url + bearer_token are set");
        assert_eq!(provider.max_audio_bytes, 25 * 1024 * 1024);
        assert_eq!(provider.timeout_secs, 300);
    }

    #[test]
    fn typed_local_whisper_default_uses_serde_defaults_not_rust_zero() {
        // Same shape contract as `local_whisper_default_uses_serde_defaults_not_rust_zero`,
        // applied to the typed provider surface
        // (`[providers.transcription.local_whisper.<alias>]`). The
        // `Configurable` macro emits `<T as Default>::default()` for newly
        // scaffolded `create_map_key(...)` entries, so a regression to
        // `#[derive(Default)]` here would let `max_audio_bytes = 0` /
        // `timeout_secs = 0` leak through into a typed map entry the same
        // way it did through the legacy `Default::default()` path.
        let cfg = zeroclaw_config::schema::LocalWhisperTranscriptionProviderConfig::default();
        assert_eq!(
            cfg.max_audio_bytes,
            25 * 1024 * 1024,
            "typed provider default must reuse the serde-default value (25 MB); got {}",
            cfg.max_audio_bytes
        );
        assert_eq!(
            cfg.timeout_secs, 300,
            "typed provider default must reuse the serde-default value (300 s); got {}",
            cfg.timeout_secs
        );
        assert!(
            cfg.uri.is_empty(),
            "uri stays empty (no working endpoint at config-init time)"
        );
        assert_eq!(
            cfg.bearer_token, None,
            "bearer_token stays None by default (unauthenticated local endpoint)"
        );
        assert_eq!(
            cfg.language, None,
            "language stays None (operator opts in per-deployment)"
        );
    }

    #[test]
    fn typed_local_whisper_provider_accepts_default_after_uri_and_token_filled() {
        // A freshly scaffolded `[providers.transcription.local_whisper.<alias>]`
        // map entry — what the `Configurable` macro writes when `create_map_key`
        // opens a new alias — must already pass `LocalWhisperProvider::from_typed_config`
        // once the operator fills `uri` and `bearer_token`. A regression to
        // `Default::default()` would produce `max_audio_bytes = 0` and
        // `timeout_secs = 0`; the typed-config bridge forwards those zeros
        // into `LocalWhisperProvider::from_config`, which rejects them at
        // load, and the alias landed in `dropped_config: providers.transcription.local_whisper`.
        let cfg = zeroclaw_config::schema::LocalWhisperTranscriptionProviderConfig {
            uri: "http://127.0.0.1:9999/v1/transcribe".to_string(),
            bearer_token: Some("test-token".to_string()),
            ..zeroclaw_config::schema::LocalWhisperTranscriptionProviderConfig::default()
        };

        let provider = LocalWhisperProvider::from_typed_config("local_whisper", &cfg).expect(
            "typed provider config-init default must be loadable once uri + bearer_token are set",
        );
        assert_eq!(provider.max_audio_bytes, 25 * 1024 * 1024);
        assert_eq!(provider.timeout_secs, 300);
        assert_eq!(provider.url, "http://127.0.0.1:9999/v1/transcribe");
        assert_eq!(provider.bearer_token, "test-token");
    }

    /// Child-struct serde round-trip: TOML serialize + deserialize on a
    /// `LocalWhisperConfig` whose numeric fields were populated via
    /// `..LocalWhisperConfig::default()`. This pins the contract that
    /// `Default` reuses the same serde-default helpers the deserializer
    /// does — the round-tripped values match the originals, no zero-
    /// value leakage.
    #[test]
    fn local_whisper_serde_round_trip() {
        let scaffolded = zeroclaw_config::schema::LocalWhisperConfig {
            url: "http://127.0.0.1:9999/v1/transcribe".to_string(),
            bearer_token: Some("test-token".to_string()),
            ..zeroclaw_config::schema::LocalWhisperConfig::default()
        };

        let toml_str =
            toml::to_string(&scaffolded).expect("LocalWhisperConfig must serialize to TOML");
        assert!(
            toml_str.contains("max_audio_bytes = 26214400"),
            "config init TOML must contain max_audio_bytes = 26214400;\ngot:\n{toml_str}"
        );
        assert!(
            toml_str.contains("timeout_secs = 300"),
            "config init TOML must contain timeout_secs = 300;\ngot:\n{toml_str}"
        );

        let reloaded: zeroclaw_config::schema::LocalWhisperConfig =
            toml::from_str(&toml_str).expect("round-tripped TOML must deserialize");
        assert_eq!(reloaded.max_audio_bytes, 25 * 1024 * 1024);
        assert_eq!(reloaded.timeout_secs, 300);
        assert_eq!(reloaded.url, "http://127.0.0.1:9999/v1/transcribe");
        assert_eq!(reloaded.bearer_token.as_deref(), Some("test-token"));

        let provider = LocalWhisperProvider::from_config("local_whisper", &reloaded)
            .expect("round-tripped config-init default must be loadable");
        assert_eq!(provider.max_audio_bytes, 25 * 1024 * 1024);
        assert_eq!(provider.timeout_secs, 300);
    }

    /// Production-boundary regression: the scaffolded `[transcription]`
    /// section must survive the same write→load cycle the daemon performs,
    /// without the resilient loader dropping it (`dropped_config:
    /// transcription`).
    ///
    /// The test walks the production paths end to end, in the same order
    /// the CLI and daemon hit them:
    ///
    /// 1. **Pre-existing config.toml** — `Config::load_or_init` has
    ///    already created `config.toml` before any `zeroclaw config`
    ///    subcommand runs, so every `save_dirty()` below takes the
    ///    incremental existing-document path (`apply_dirty_path`), never
    ///    the full-save fallback for a missing file.
    /// 2. **Scaffold + persist** — `Config::init_defaults(Some(
    ///    "transcription.local_whisper"))` is the exact call the
    ///    `zeroclaw config init <section>` handler makes
    ///    (`ConfigCommands::Init` in `src/main.rs`), persisted through
    ///    the same `mark_dirty`/`save_dirty` path the handler uses.
    /// 3. **Operator edits** — `set_prop_persistent` is the exact call
    ///    the `zeroclaw config set <path> <value>` handler makes
    ///    (`ConfigCommands::Set`): it sets the field and marks that path
    ///    dirty, so `transcription.enabled` is persisted as its own dirty
    ///    path rather than riding along on a full save.
    /// 4. **Resilient load** — `migration::migrate_to_current_salvaged`
    ///    is the exact call `Config::load_or_init` makes when the daemon
    ///    boots; its returned `dropped` list is the source of the
    ///    `dropped_config: <path>` WARN events (migration.rs), so
    ///    asserting the list does not contain `transcription` asserts
    ///    the absence of that named log event at its source.
    #[tokio::test]
    async fn local_whisper_config_init_preserves_transcription_section() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let config_path = tmp.path().join("config.toml");

        // Step 1 — the CLI entry state: config.toml already exists on
        // disk (load_or_init created it), so save_dirty below runs the
        // incremental existing-document machinery, not the full-save
        // fallback for a missing file.
        let initial = zeroclaw_config::schema::Config {
            config_path: config_path.clone(),
            ..zeroclaw_config::schema::Config::default()
        };
        initial
            .save()
            .await
            .expect("default config.toml must be created");
        assert!(
            config_path.is_file(),
            "config.toml must exist before the scaffold, mirroring load_or_init"
        );

        // Step 2 — the real scaffold: no handcrafted struct assignment.
        let mut config = zeroclaw_config::schema::Config {
            config_path: config_path.clone(),
            ..zeroclaw_config::schema::Config::default()
        };
        let initialized = config.init_defaults(Some("transcription.local_whisper"));
        assert!(
            initialized.contains(&"transcription.local_whisper"),
            "scaffold must report the section initialized, got: {initialized:?}"
        );

        // Persist through the production dirty/save path, mirroring the
        // CLI handler (mark each initialized section dirty, then save).
        for section in &initialized {
            config.mark_dirty(section);
        }
        config
            .save_dirty()
            .await
            .expect("save_dirty must persist the scaffolded section incrementally");

        // Step 3 — the operator fills the scaffolded block and enables
        // the parent section through the same persistent setter the
        // `config set` handler uses (set field + mark that path dirty).
        config
            .set_prop_persistent("transcription.enabled", "true")
            .expect("transcription.enabled must be settable");
        config
            .set_prop_persistent(
                "transcription.local_whisper.url",
                "http://127.0.0.1:9999/v1/transcribe",
            )
            .expect("transcription.local_whisper.url must be settable");
        config
            .set_prop_persistent("transcription.local_whisper.bearer_token", "test-token")
            .expect("transcription.local_whisper.bearer_token must be settable");
        config
            .save_dirty()
            .await
            .expect("save_dirty must persist operator edits incrementally");

        // Step 4 — the real resilient daemon load.
        let contents =
            std::fs::read_to_string(&config_path).expect("persisted config.toml must be readable");
        let salvage = zeroclaw_config::migration::migrate_to_current_salvaged(&contents);

        assert!(
            !salvage.dropped.iter().any(|path| path == "transcription"),
            "resilient load must not drop `transcription` (the dropped_config WARN), \
             dropped: {:?}",
            salvage.dropped
        );
        assert!(
            salvage.dropped_security.is_empty(),
            "no security-critical section must be degraded, dropped_security: {:?}",
            salvage.dropped_security
        );

        // The section survives with operator intent and the scaffolded
        // non-zero defaults, and the provider accepts it.
        let loaded = salvage.config;
        assert!(
            loaded.transcription.enabled,
            "transcription.enabled must survive the scaffold→persist→resilient-load cycle"
        );
        let local = loaded
            .transcription
            .local_whisper
            .as_ref()
            .expect("transcription.local_whisper must survive the resilient load");
        assert_eq!(local.max_audio_bytes, 25 * 1024 * 1024);
        assert_eq!(local.timeout_secs, 300);
        assert_eq!(local.url, "http://127.0.0.1:9999/v1/transcribe");

        let provider = LocalWhisperProvider::from_config("local_whisper", local)
            .expect("resilient-loaded local_whisper config must be loadable by from_config");
        assert_eq!(provider.max_audio_bytes, 25 * 1024 * 1024);
        assert_eq!(provider.timeout_secs, 300);
    }

    #[test]
    fn local_whisper_registered_when_config_present() {
        let config = TranscriptionConfig {
            local_whisper: Some(local_whisper_config("http://127.0.0.1:9999/v1/transcribe")),
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
        // registration. When transcription is enabled and no other provider
        // section is set, the safety net in TranscriptionManager surfaces
        // the error rather than returning a useless empty manager.
        let mut bad_cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        bad_cfg.bearer_token = Some(String::new());
        let config = TranscriptionConfig {
            local_whisper: Some(bad_cfg),
            enabled: true,
            ..TranscriptionConfig::default()
        };

        let err = TranscriptionManager::new(&config).err().unwrap();
        assert!(
            err.to_string()
                .contains("no transcription provider registered"),
            "expected 'no transcription provider registered' from manager safety net, got: {err}"
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
        let transcription_provider =
            LocalWhisperProvider::from_config("local_whisper", &cfg).unwrap();
        let big = vec![0u8; cfg.max_audio_bytes + 1];
        let err = transcription_provider
            .transcribe(&big, "voice.ogg")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[tokio::test]
    async fn local_whisper_rejects_unsupported_format() {
        let cfg = local_whisper_config("http://127.0.0.1:9999/v1/transcribe");
        let transcription_provider =
            LocalWhisperProvider::from_config("local_whisper", &cfg).unwrap();
        let data = vec![0u8; 100];
        let err = transcription_provider
            .transcribe(&data, "voice.aiff")
            .await
            .unwrap_err();
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
        let transcription_provider =
            LocalWhisperProvider::from_config("local_whisper", &cfg).unwrap();

        let result = transcription_provider
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
        let transcription_provider =
            LocalWhisperProvider::from_config("local_whisper", &cfg).unwrap();

        let result = transcription_provider
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
        let transcription_provider =
            LocalWhisperProvider::from_config("local_whisper", &cfg).unwrap();

        let err = transcription_provider
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
        let transcription_provider =
            LocalWhisperProvider::from_config("local_whisper", &cfg).unwrap();

        let err = transcription_provider
            .transcribe(b"fake-audio", "voice.ogg")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("502"), "got: {err}");
        assert!(
            err.to_string().contains("Bad Gateway"),
            "expected plain-text body in error, got: {err}"
        );
    }

    #[tokio::test]
    async fn with_typed_providers_registers_dotted_alias_keys() {
        use zeroclaw_config::providers::TranscriptionProviders;
        use zeroclaw_config::schema::{
            GroqTranscriptionProviderConfig, TranscriptionProviderConfig,
        };

        let mut typed = TranscriptionProviders::default();
        typed.groq.insert(
            "default".to_string(),
            GroqTranscriptionProviderConfig {
                base: TranscriptionProviderConfig {
                    api_key: Some("gsk_test_key".to_string()),
                    language: None,
                    initial_prompt: None,
                },
                model: Some("whisper-large-v3-turbo".to_string()),
            },
        );

        // new() would fail (transcription.enabled=false, no api_key) — build
        // an empty manager shell directly, then apply typed providers.
        let manager = TranscriptionManager {
            transcription_providers: std::collections::HashMap::new(),
            max_audio_bytes: None,
            agent_transcription_provider: String::new(),
        }
        .with_typed_providers(&typed);

        // The typed groq.default must be reachable under the dotted key.
        assert!(
            manager.transcription_providers.contains_key("groq.default"),
            "typed provider must be registered under 'groq.default'"
        );

        // Binding the dotted alias and calling transcribe must reach the
        // provider (not fail with "no transcription_provider configured").
        let manager = manager.with_agent_transcription_provider("groq.default");
        let result = manager.transcribe(b"", "voice.wav").await;
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("no transcription_provider configured"),
            "dotted alias must resolve; got: {err}"
        );
    }

    #[cfg(test)]
    mod openai_stt_env_tests {
        use super::*;
        use std::sync::{LazyLock, Mutex};

        /// Serialises env-mutating tests so concurrent `set_var`/`remove_var`
        /// calls cannot race on the shared process environment.
        static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

        /// Helper that sets both env vars, runs `f`, then clears both.
        /// Uses `unsafe` for `set_var`/`remove_var` because `ENV_LOCK` guarantees
        /// no concurrent mutation of these keys (safe in single-threaded test
        /// context with serialisation).
        #[allow(unsafe_code)]
        fn with_envs<F>(transcription_key: Option<&str>, openai_key: Option<&str>, f: F)
        where
            F: FnOnce(),
        {
            let _guard = ENV_LOCK.lock().unwrap();
            // SAFETY: ENV_LOCK serialises all env-mutating test code in this
            // module; keys are never read by concurrent threads.
            unsafe {
                // set requested vars
                if let Some(k) = transcription_key {
                    std::env::set_var("TRANSCRIPTION_API_KEY", k);
                }
                if let Some(k) = openai_key {
                    std::env::set_var("OPENAI_API_KEY", k);
                }
                // clear expected-empty vars first
                if transcription_key.is_none() {
                    std::env::remove_var("TRANSCRIPTION_API_KEY");
                }
                if openai_key.is_none() {
                    std::env::remove_var("OPENAI_API_KEY");
                }
            }

            f();

            // SAFETY: same guard reason; cleanup after test
            unsafe {
                std::env::remove_var("TRANSCRIPTION_API_KEY");
                std::env::remove_var("OPENAI_API_KEY");
            }
        }

        #[test]
        fn test_from_config_uses_explicit_api_key() {
            let config = OpenAiSttConfig {
                api_key: Some("sk-explicit".to_string()),
                model: "whisper-1".to_string(),
            };
            let provider = OpenAiWhisperProvider::from_config("test", &config).unwrap();
            assert_eq!(provider.api_key, "sk-explicit");
        }

        #[test]
        fn test_from_config_uses_transcription_api_key_env() {
            with_envs(Some("sk-transcript"), None, || {
                let config = OpenAiSttConfig {
                    api_key: None,
                    model: "whisper-1".to_string(),
                };
                let provider = OpenAiWhisperProvider::from_config("test", &config).unwrap();
                assert_eq!(provider.api_key, "sk-transcript");
            });
        }

        #[test]
        fn test_from_config_uses_openai_api_key_env() {
            with_envs(None, Some("sk-openai"), || {
                let config = OpenAiSttConfig {
                    api_key: None,
                    model: "whisper-1".to_string(),
                };
                let provider = OpenAiWhisperProvider::from_config("test", &config).unwrap();
                assert_eq!(provider.api_key, "sk-openai");
            });
        }

        #[test]
        fn test_from_config_fails_with_no_credentials() {
            with_envs(None, None, || {
                let config = OpenAiSttConfig {
                    api_key: None,
                    model: "whisper-1".to_string(),
                };
                assert!(OpenAiWhisperProvider::from_config("test", &config).is_err());
            });
        }

        #[test]
        fn test_priority_explicit_over_env() {
            with_envs(None, Some("sk-env"), || {
                let config = OpenAiSttConfig {
                    api_key: Some("sk-explicit".to_string()),
                    model: "whisper-1".to_string(),
                };
                let provider = OpenAiWhisperProvider::from_config("test", &config).unwrap();
                assert_eq!(provider.api_key, "sk-explicit");
            });
        }

        #[test]
        fn test_transcription_over_openai_env_priority() {
            with_envs(Some("sk-transcript"), Some("sk-openai"), || {
                let config = OpenAiSttConfig {
                    api_key: None,
                    model: "whisper-1".to_string(),
                };
                let provider = OpenAiWhisperProvider::from_config("test", &config).unwrap();
                assert_eq!(provider.api_key, "sk-transcript");
            });
        }

        #[test]
        fn test_from_typed_config_uses_explicit_api_key() {
            let cfg = zeroclaw_config::schema::OpenAiTranscriptionProviderConfig {
                base: zeroclaw_config::schema::TranscriptionProviderConfig {
                    api_key: Some("sk-explicit".to_string()),
                    language: None,
                    initial_prompt: None,
                },
                model: Some("whisper-1".to_string()),
            };
            let provider = OpenAiWhisperProvider::from_typed_config("test", &cfg).unwrap();
            assert_eq!(provider.api_key, "sk-explicit");
        }

        #[test]
        fn test_from_typed_config_uses_transcription_api_key_env() {
            with_envs(Some("sk-transcript"), None, || {
                let cfg = zeroclaw_config::schema::OpenAiTranscriptionProviderConfig {
                    base: zeroclaw_config::schema::TranscriptionProviderConfig {
                        api_key: None,
                        language: None,
                        initial_prompt: None,
                    },
                    model: Some("whisper-1".to_string()),
                };
                let provider = OpenAiWhisperProvider::from_typed_config("test", &cfg).unwrap();
                assert_eq!(provider.api_key, "sk-transcript");
            });
        }

        #[test]
        fn test_from_typed_config_uses_openai_api_key_env() {
            with_envs(None, Some("sk-openai"), || {
                let cfg = zeroclaw_config::schema::OpenAiTranscriptionProviderConfig {
                    base: zeroclaw_config::schema::TranscriptionProviderConfig {
                        api_key: None,
                        language: None,
                        initial_prompt: None,
                    },
                    model: Some("whisper-1".to_string()),
                };
                let provider = OpenAiWhisperProvider::from_typed_config("test", &cfg).unwrap();
                assert_eq!(provider.api_key, "sk-openai");
            });
        }

        #[test]
        fn test_from_typed_config_fails_with_no_credentials() {
            with_envs(None, None, || {
                let cfg = zeroclaw_config::schema::OpenAiTranscriptionProviderConfig {
                    base: zeroclaw_config::schema::TranscriptionProviderConfig {
                        api_key: None,
                        language: None,
                        initial_prompt: None,
                    },
                    model: Some("whisper-1".to_string()),
                };
                assert!(OpenAiWhisperProvider::from_typed_config("test", &cfg).is_err());
            });
        }
    }
}
