use anyhow::{bail, Context, Result};
use reqwest::multipart::{Form, Part};

use crate::config::schema::TranscriptionProvider;
use crate::config::TranscriptionConfig;
use uuid::Uuid;

/// Maximum upload size accepted by the Groq Whisper API (25 MB).
const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;

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

/// Maximum file size for local transcription to avoid OOM (20 MB).
const MAX_LOCAL_AUDIO_BYTES: usize = 20 * 1024 * 1024;

/// Transcribe audio bytes via a Whisper-compatible transcription API.
///
/// Returns the transcribed text on success.  Requires `GROQ_API_KEY` in the
/// environment for Groq provider.  The caller is responsible for enforcing
/// duration limits *before* downloading the file.
pub async fn transcribe_audio(
    audio_data: Vec<u8>,
    file_name: &str,
    config: &TranscriptionConfig,
    workspace_path: &std::path::Path,
) -> Result<String> {
    match config.provider {
        TranscriptionProvider::Groq => {
            transcribe_audio_groq(audio_data, file_name, config).await
        }
        TranscriptionProvider::Local => {
            if audio_data.len() > MAX_LOCAL_AUDIO_BYTES {
                bail!(
                    "Local audio file too large ({} bytes, max {MAX_LOCAL_AUDIO_BYTES}) to prevent OOM",
                    audio_data.len()
                );
            }

            // Local whisper-rs expects a file path. We write the bytes to a temporary file.
            let temp_dir = std::env::temp_dir();
            let temp_path = temp_dir.join(format!("transcribe_{}", Uuid::new_v4()));
            tokio::fs::write(&temp_path, &audio_data).await.context("Failed to write temporary audio file for local transcription")?;
            
            let res = transcribe_audio_whisper_rs(
                &temp_path,
                config.whisper_model_path.as_deref(),
                workspace_path,
                None, // propagate language if needed, but voice notes don't have it explicitly stored yet
            ).await;

            let _ = tokio::fs::remove_file(&temp_path).await;
            res
        }
    }
}

async fn transcribe_audio_groq(
    audio_data: Vec<u8>,
    file_name: &str,
    config: &TranscriptionConfig,
) -> Result<String> {
    if audio_data.len() > MAX_AUDIO_BYTES {
        bail!(
            "Audio file too large ({} bytes, max {MAX_AUDIO_BYTES})",
            audio_data.len()
        );
    }

    let normalized_name = normalize_audio_filename(file_name);
    let extension = normalized_name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .unwrap_or("");
    let mime = mime_for_audio(extension).ok_or_else(|| {
        anyhow::anyhow!(
            "Unsupported audio format '.{extension}' — accepted: flac, mp3, mp4, mpeg, mpga, m4a, ogg, opus, wav, webm"
        )
    })?;

    let api_key = std::env::var("GROQ_API_KEY").context(
        "GROQ_API_KEY environment variable is not set — required for voice transcription",
    )?;

    let client = crate::config::build_runtime_proxy_client("transcription.groq");

    let file_part = Part::bytes(audio_data)
        .file_name(normalized_name)
        .mime_str(mime)?;

    let mut form = Form::new()
        .part("file", file_part)
        .text("model", config.model.clone())
        .text("response_format", "json");

    if let Some(ref lang) = config.language {
        form = form.text("language", lang.clone());
    }

    let resp = client
        .post(&config.api_url)
        .bearer_auth(&api_key)
        .multipart(form)
        .send()
        .await
        .context("Failed to send transcription request")?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .context("Failed to parse transcription response")?;

    if !status.is_success() {
        let error_msg = body["error"]["message"].as_str().unwrap_or("unknown error");
        bail!("Transcription API error ({}): {}", status, error_msg);
    }

    let text = body["text"]
        .as_str()
        .context("Transcription response missing 'text' field")?
        .to_string();

    Ok(text)
}

/// Transcribe audio file using local `whisper-rs` and `ffmpeg`
///
/// Requires `ffmpeg` to be installed and available in PATH.
/// Downloads `ggml-tiny.bin` to `workspace_dir/models` if no custom model is specified.
pub async fn transcribe_audio_whisper_rs(
    file_path: &std::path::Path,
    model_path: Option<&str>,
    workspace_dir: &std::path::Path,
    language: Option<&str>,
) -> Result<String> {
    #[cfg(feature = "transcription-local")]
    {
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

        let actual_model_path = match model_path {
            Some(p) => std::path::PathBuf::from(p),
            None => {
                let models_dir = workspace_dir.join("models");
                tokio::fs::create_dir_all(&models_dir).await?;
                let default_model = models_dir.join("ggml-tiny.bin");
                if !default_model.exists() {
                    tracing::info!("Downloading default whisper model (ggml-tiny.bin) to {:?}", default_model);
                    let response = reqwest::get("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin")
                        .await?
                        .error_for_status()?;
                    let bytes = response.bytes().await?;
                    
                    // Atomic download: write to temp file then rename
                    let temp_model_path = default_model.with_extension("tmp");
                    tokio::fs::write(&temp_model_path, bytes).await?;
                    if let Err(e) = tokio::fs::rename(&temp_model_path, &default_model).await {
                        // If rename fails because file now exists, that's fine (concurrent download won)
                        if default_model.exists() {
                            let _ = tokio::fs::remove_file(&temp_model_path).await;
                        } else {
                            return Err(e).context("Failed to rename temporary model file to target path");
                        }
                    }
                }
                default_model
            }
        };

        let output = tokio::process::Command::new("ffmpeg")
            .arg("-i")
            .arg(file_path)
            .args(["-ar", "16000"])
            .args(["-ac", "1"])
            .args(["-f", "f32le"])
            .arg("-")
            .output()
            .await
            .context("Failed to execute ffmpeg for audio decoding. Is it installed?")?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("ffmpeg failed to decode audio: {}", err);
        }

        let audio_bytes = output.stdout;
        if audio_bytes.len() % 4 != 0 {
            bail!("ffmpeg output is not a multiple of 4 bytes");
        }

        let audio_f32: Vec<f32> = audio_bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        // transcription-local feature enables whisper-rs
        let ctx = WhisperContext::new_with_params(
            actual_model_path.to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .context("Failed to create WhisperContext")?;

        let mut state = ctx.create_state().context("Failed to create WhisperState")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        
        if let Some(lang) = language {
            params.set_language(Some(lang));
        }

        state
            .full(params, &audio_f32)
            .context("Failed to run full Whisper transcription")?;

        let mut result = String::new();
        for segment in state.as_iter() {
            if let Ok(text) = segment.to_str() {
                result.push_str(text);
            }
        }
        Ok(result)
    }
    
    #[cfg(not(feature = "transcription-local"))]
    {
        let _ = (file_path, model_path, workspace_dir, language);
        bail!("Local transcription requires the 'transcription-local' cargo feature")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_oversized_audio() {
        let big = vec![0u8; MAX_AUDIO_BYTES + 1];
        let config = TranscriptionConfig::default();

        let workspace = std::path::Path::new(".");
        let err = transcribe_audio(big, "test.ogg", &config, workspace)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("too large"),
            "expected size error, got: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_missing_api_key() {
        // Ensure the key is absent for this test
        std::env::remove_var("GROQ_API_KEY");

        let data = vec![0u8; 100];
        let config = TranscriptionConfig::default();

        let workspace = std::path::Path::new(".");
        let err = transcribe_audio(data, "test.ogg", &config, workspace)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("GROQ_API_KEY"),
            "expected missing-key error, got: {err}"
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

        let workspace = std::path::Path::new(".");
        let err = transcribe_audio(data, "recording.aac", &config, workspace)
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
}
