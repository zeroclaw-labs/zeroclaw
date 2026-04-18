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

// ── LocalSttProvider (native whisper.cpp) ───────────────────────

/// Native local STT provider — runs Whisper inference in-process via
/// whisper.cpp (through `whisper-rs`). No external server, no API key.
///
/// The GGML model file is auto-downloaded from Hugging Face Hub on first
/// `transcribe()` call and cached in `~/.zeroclaw/models/whisper/`.
///
/// Audio is decoded to 16 kHz mono f32 PCM via `ffmpeg` before inference.
///
/// Gated behind the `local-stt` cargo feature.
#[cfg(feature = "local-stt")]
pub struct LocalSttProvider {
    model_size: String,
    model_dir: std::path::PathBuf,
    language: Option<String>,
    max_audio_bytes: usize,
    /// Cached model path after first download.
    model_path: tokio::sync::OnceCell<std::path::PathBuf>,
}

#[cfg(feature = "local-stt")]
impl LocalSttProvider {
    /// HuggingFace repo that hosts the official GGML Whisper models.
    const HF_REPO: &'static str = "ggerganov/whisper.cpp";

    /// Map user-friendly model size to the GGML filename in the HF repo.
    fn ggml_filename(model_size: &str) -> Result<&'static str> {
        match model_size {
            "tiny" => Ok("ggml-tiny.bin"),
            "tiny.en" => Ok("ggml-tiny.en.bin"),
            "base" => Ok("ggml-base.bin"),
            "base.en" => Ok("ggml-base.en.bin"),
            "small" => Ok("ggml-small.bin"),
            "small.en" => Ok("ggml-small.en.bin"),
            "medium" => Ok("ggml-medium.bin"),
            "medium.en" => Ok("ggml-medium.en.bin"),
            "large-v1" => Ok("ggml-large-v1.bin"),
            "large-v2" => Ok("ggml-large-v2.bin"),
            "large-v3" => Ok("ggml-large-v3.bin"),
            "large-v3-turbo" => Ok("ggml-large-v3-turbo.bin"),
            other => bail!(
                "Unknown Whisper model size '{other}'. \
                 Valid: tiny, tiny.en, base, base.en, small, small.en, \
                 medium, medium.en, large-v1, large-v2, large-v3, large-v3-turbo"
            ),
        }
    }

    /// Resolve (and download if necessary) the GGML model file.
    async fn ensure_model(
        model_size: &str,
        model_dir: &std::path::Path,
    ) -> Result<std::path::PathBuf> {
        let filename = Self::ggml_filename(model_size)?;
        let dest = model_dir.join(filename);

        // Fast path: model already cached.
        if dest.is_file() {
            tracing::debug!("Local STT model already cached: {}", dest.display());
            return Ok(dest);
        }

        tracing::info!(
            "Downloading Whisper model '{model_size}' from Hugging Face Hub \
             to {} (first-time setup)…",
            model_dir.display()
        );
        std::fs::create_dir_all(model_dir)
            .with_context(|| format!("Failed to create model directory {}", model_dir.display()))?;

        // Download via hf-hub (async/tokio).
        let api = hf_hub::api::tokio::Api::new()
            .context("Failed to initialise Hugging Face Hub client")?;
        let repo = api.model(Self::HF_REPO.to_string());
        let downloaded = repo
            .get(filename)
            .await
            .with_context(|| format!("Failed to download {filename} from {}", Self::HF_REPO))?;

        // hf-hub caches to its own directory — copy to our model_dir for a
        // stable, user-visible location.
        if downloaded != dest {
            std::fs::copy(&downloaded, &dest).with_context(|| {
                format!(
                    "Failed to copy model from {} to {}",
                    downloaded.display(),
                    dest.display()
                )
            })?;
        }

        tracing::info!("Model ready: {}", dest.display());
        Ok(dest)
    }

    /// Build from config (synchronous). Model download is deferred to first use.
    pub fn from_config(config: &zeroclaw_config::schema::LocalSttConfig) -> Result<Self> {
        // Validate model_size eagerly so typos fail at startup.
        Self::ggml_filename(&config.model_size)?;

        let model_dir = match config
            .model_dir
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(dir) => std::path::PathBuf::from(shellexpand::tilde(dir).as_ref()),
            None => {
                let base =
                    directories::BaseDirs::new().context("Cannot determine home directory")?;
                base.home_dir()
                    .join(".zeroclaw")
                    .join("models")
                    .join("whisper")
            }
        };

        anyhow::ensure!(
            config.max_audio_bytes > 0,
            "local: `max_audio_bytes` must be greater than zero"
        );

        Ok(Self {
            model_size: config.model_size.clone(),
            model_dir,
            language: config.language.clone(),
            max_audio_bytes: config.max_audio_bytes,
            model_path: tokio::sync::OnceCell::new(),
        })
    }

    /// Get or download the model path (lazy, thread-safe).
    async fn get_model_path(&self) -> Result<&std::path::Path> {
        self.model_path
            .get_or_try_init(|| Self::ensure_model(&self.model_size, &self.model_dir))
            .await
            .map(|p| p.as_path())
    }

    /// Decode any audio file to 16 kHz mono f32 PCM samples via `ffmpeg`.
    async fn decode_audio_to_pcm(audio_data: &[u8], file_name: &str) -> Result<Vec<f32>> {
        use tokio::process::Command;

        let ext = file_name
            .rsplit_once('.')
            .map(|(_, e)| format!(".{e}"))
            .unwrap_or_else(|| ".ogg".to_string());

        let tmp_dir = std::env::temp_dir();
        let input_path = tmp_dir.join(format!("zc_stt_in_{}{ext}", uuid::Uuid::new_v4()));
        let output_path = tmp_dir.join(format!("zc_stt_out_{}.wav", uuid::Uuid::new_v4()));

        // Write input audio.
        tokio::fs::write(&input_path, audio_data)
            .await
            .with_context(|| format!("Failed to write temp audio {}", input_path.display()))?;

        // Run ffmpeg: convert to 16 kHz mono s16le WAV.
        let ffmpeg_result = Command::new("ffmpeg")
            .args([
                "-y",
                "-i",
                input_path.to_str().unwrap_or("input"),
                "-ar",
                "16000",
                "-ac",
                "1",
                "-f",
                "wav",
                output_path.to_str().unwrap_or("output"),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .context(
                "Failed to run ffmpeg — ensure it is installed \
                 (e.g. `sudo dnf install ffmpeg-free` or `sudo apt install ffmpeg`)",
            )?;

        let _ = tokio::fs::remove_file(&input_path).await;

        if !ffmpeg_result.status.success() {
            let _ = tokio::fs::remove_file(&output_path).await;
            let stderr = String::from_utf8_lossy(&ffmpeg_result.stderr);
            bail!(
                "ffmpeg failed ({}): {}",
                ffmpeg_result.status,
                stderr.lines().last().unwrap_or("unknown error")
            );
        }

        // Read WAV output and extract f32 samples.
        let wav_bytes = tokio::fs::read(&output_path)
            .await
            .with_context(|| format!("Failed to read ffmpeg output {}", output_path.display()))?;
        let _ = tokio::fs::remove_file(&output_path).await;

        // Skip 44-byte WAV header, convert i16 LE samples to f32.
        anyhow::ensure!(wav_bytes.len() >= 44, "ffmpeg produced an invalid WAV file");
        let pcm_data = &wav_bytes[44..];
        let samples: Vec<f32> = pcm_data
            .chunks_exact(2)
            .map(|chunk| {
                let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                sample as f32 / 32768.0
            })
            .collect();

        anyhow::ensure!(
            !samples.is_empty(),
            "Audio produced no PCM samples after decoding"
        );

        Ok(samples)
    }
}

#[cfg(feature = "local-stt")]
#[async_trait]
impl TranscriptionProvider for LocalSttProvider {
    fn name(&self) -> &str {
        "local"
    }

    async fn transcribe(&self, audio_data: &[u8], file_name: &str) -> Result<String> {
        if audio_data.len() > self.max_audio_bytes {
            bail!(
                "Audio file too large ({} bytes, local max {})",
                audio_data.len(),
                self.max_audio_bytes
            );
        }

        // Validate audio format.
        let _ = resolve_audio_format(file_name)?;

        // Ensure model is downloaded (lazy, first call only).
        let model_path = self.get_model_path().await?.to_path_buf();

        // Decode to 16 kHz mono f32 PCM.
        let samples = Self::decode_audio_to_pcm(audio_data, file_name).await?;

        // Run whisper.cpp inference on a blocking thread (CPU-bound).
        let language = self.language.clone();

        tokio::task::spawn_blocking(move || {
            let ctx = whisper_rs::WhisperContext::new_with_params(
                model_path.to_str().unwrap_or("model.bin"),
                whisper_rs::WhisperContextParameters::default(),
            )
            .context("Failed to load Whisper model")?;

            let mut state = ctx
                .create_state()
                .context("Failed to create Whisper state")?;

            let mut params =
                whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });

            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_suppress_blank(true);

            if let Some(ref lang) = language {
                params.set_language(Some(lang));
            }

            state
                .full(params, &samples)
                .context("Whisper inference failed")?;

            let n_segments = state.full_n_segments();
            let mut text = String::new();
            for i in 0..n_segments {
                if let Some(segment) = state.get_segment(i) {
                    if let Ok(s) = segment.to_str() {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(s.trim());
                    }
                }
            }

            if text.is_empty() {
                text = "(no speech detected)".to_string();
            }

            Ok(text)
        })
        .await
        .context("Whisper inference task panicked")?
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

        #[cfg(feature = "local-stt")]
        if let Some(ref local_cfg) = config.local {
            match LocalSttProvider::from_config(local_cfg) {
                Ok(p) => {
                    providers.insert("local".to_string(), Box::new(p));
                }
                Err(e) => {
                    tracing::warn!("local STT config invalid, provider skipped: {e}");
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
        #[cfg(feature = "local-stt")]
        "local" => {
            let local_cfg = config.local.as_ref().context(
                "Default transcription provider 'local' is not configured. \
                 Add [transcription.local] and build with --features local-stt",
            )?;
            let local = LocalSttProvider::from_config(local_cfg)?;
            local.transcribe(&audio_data, file_name).await
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
        assert!(config.local.is_none());
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

    // ── LocalSttProvider tests (feature-gated) ──────────────────

    #[test]
    fn backward_compat_config_has_local_none() {
        let config = TranscriptionConfig::default();
        assert!(config.local.is_none());
    }

    #[cfg(feature = "local-stt")]
    #[test]
    fn local_stt_ggml_filename_maps_all_sizes() {
        let cases = [
            ("tiny", "ggml-tiny.bin"),
            ("tiny.en", "ggml-tiny.en.bin"),
            ("base", "ggml-base.bin"),
            ("base.en", "ggml-base.en.bin"),
            ("small", "ggml-small.bin"),
            ("small.en", "ggml-small.en.bin"),
            ("medium", "ggml-medium.bin"),
            ("medium.en", "ggml-medium.en.bin"),
            ("large-v1", "ggml-large-v1.bin"),
            ("large-v2", "ggml-large-v2.bin"),
            ("large-v3", "ggml-large-v3.bin"),
            ("large-v3-turbo", "ggml-large-v3-turbo.bin"),
        ];
        for (size, expected) in cases {
            assert_eq!(
                LocalSttProvider::ggml_filename(size).unwrap(),
                expected,
                "failed for size: {size}"
            );
        }
    }

    #[cfg(feature = "local-stt")]
    #[test]
    fn local_stt_ggml_filename_rejects_unknown() {
        let err = LocalSttProvider::ggml_filename("nonexistent").unwrap_err();
        assert!(
            err.to_string().contains("Unknown Whisper model size"),
            "got: {err}"
        );
    }

    #[cfg(feature = "local-stt")]
    #[test]
    fn local_stt_from_config_rejects_bad_model_size() {
        let cfg = zeroclaw_config::schema::LocalSttConfig {
            model_size: "nonexistent".to_string(),
            ..zeroclaw_config::schema::LocalSttConfig::default()
        };
        let err = LocalSttProvider::from_config(&cfg).unwrap_err();
        assert!(
            err.to_string().contains("Unknown Whisper model size"),
            "got: {err}"
        );
    }

    #[cfg(feature = "local-stt")]
    #[test]
    fn local_stt_from_config_rejects_zero_max_audio_bytes() {
        let cfg = zeroclaw_config::schema::LocalSttConfig {
            max_audio_bytes: 0,
            ..zeroclaw_config::schema::LocalSttConfig::default()
        };
        let err = LocalSttProvider::from_config(&cfg).unwrap_err();
        assert!(
            err.to_string()
                .contains("`max_audio_bytes` must be greater than zero"),
            "got: {err}"
        );
    }

    #[cfg(feature = "local-stt")]
    #[test]
    fn local_stt_from_config_accepts_defaults() {
        let cfg = zeroclaw_config::schema::LocalSttConfig::default();
        let provider = LocalSttProvider::from_config(&cfg).unwrap();
        assert_eq!(provider.name(), "local");
    }

    #[cfg(feature = "local-stt")]
    #[test]
    fn local_stt_registered_when_config_present() {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        let config = TranscriptionConfig {
            local: Some(zeroclaw_config::schema::LocalSttConfig::default()),
            default_provider: "local".to_string(),
            ..TranscriptionConfig::default()
        };

        let manager = TranscriptionManager::new(&config).unwrap();
        assert!(
            manager.available_providers().contains(&"local"),
            "expected 'local' in {:?}",
            manager.available_providers()
        );
    }

    #[cfg(feature = "local-stt")]
    #[tokio::test]
    async fn local_stt_rejects_oversized_audio() {
        let cfg = zeroclaw_config::schema::LocalSttConfig::default();
        let provider = LocalSttProvider::from_config(&cfg).unwrap();
        let big = vec![0u8; cfg.max_audio_bytes + 1];
        let err = provider.transcribe(&big, "voice.ogg").await.unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[cfg(feature = "local-stt")]
    #[tokio::test]
    async fn local_stt_rejects_unsupported_format() {
        let cfg = zeroclaw_config::schema::LocalSttConfig::default();
        let provider = LocalSttProvider::from_config(&cfg).unwrap();
        let err = provider
            .transcribe(&[0u8; 100], "voice.aac")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Unsupported audio format"),
            "got: {err}"
        );
    }
}
