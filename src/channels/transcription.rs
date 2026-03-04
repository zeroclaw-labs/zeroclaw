use anyhow::{bail, Context, Result};
use reqwest::multipart::{Form, Part};

use crate::config::TranscriptionConfig;

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

/// Transcribe an audio file via a local whisper backend.
///
/// Prefers `whisper-cli` (whisper.cpp) — Metal-accelerated on Apple Silicon,
/// typically 10-20x faster than Python whisper. Falls back to Python `whisper`
/// if whisper-cli or its model file is not found.
///
/// CAF files (iMessage voice memos) are pre-converted to WAV via ffmpeg because
/// whisper-cli does not support CAF natively. Python whisper handles CAF directly.
pub async fn transcribe_audio_local(file_path: &str) -> anyhow::Result<String> {
    // Prefer whisper-cli (whisper.cpp) when available. If it fails for any
    // reason (missing ffmpeg, timeout, non-zero exit), fall back to Python
    // whisper rather than propagating the error immediately — the user may
    // have whisper.cpp installed but ffmpeg absent for a specific format.
    let mut cpp_err: Option<anyhow::Error> = None;
    if let Some((bin, model)) = resolve_whisper_cpp() {
        match transcribe_with_whisper_cpp(file_path, bin, model).await {
            Ok(t) => return Ok(t),
            Err(e) => {
                tracing::warn!("whisper-cli failed ({e:#}), falling back to Python whisper");
                cpp_err = Some(e);
            }
        }
    }

    // Fall back to Python whisper; if it also fails, surface both errors.
    transcribe_with_python_whisper(file_path).await.map_err(|py_err| {
        if let Some(cpp) = cpp_err {
            anyhow::anyhow!("whisper-cli failed ({cpp:#}); Python whisper also failed: {py_err:#}")
        } else {
            py_err
        }
    })
}

/// Transcribe using whisper-cli (whisper.cpp). Converts CAF→WAV via ffmpeg first.
async fn transcribe_with_whisper_cpp(
    file_path: &str,
    bin: &str,
    model: &str,
) -> anyhow::Result<String> {
    use std::path::{Path, PathBuf};
    use tokio::process::Command;

    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    // Always convert to WAV — whisper-cli (brew build) only reliably reads WAV
    // regardless of which formats it advertises. ffmpeg handles all iMessage
    // audio formats (CAF, M4A, AAC, MP3, etc.).
    let (input_path, caf_tmp): (PathBuf, Option<PathBuf>) = if ext.eq_ignore_ascii_case("wav") {
        (PathBuf::from(file_path), None)
    } else {
        let tmp = std::env::temp_dir().join(format!("zc_wpp_{}.wav", uuid::Uuid::new_v4()));
        let ffmpeg = resolve_ffmpeg_bin().context(
            "ffmpeg not found — install ffmpeg to enable CAF transcription with whisper-cli",
        )?;
        let tmp_str = tmp
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Temp WAV path contains non-UTF-8 characters"))?;
        let mut ffmpeg_cmd = Command::new(ffmpeg);
        ffmpeg_cmd.args(["-y", "-i", file_path, "-ar", "16000", "-ac", "1", tmp_str]);
        ffmpeg_cmd.kill_on_drop(true);
        let conv = tokio::time::timeout(std::time::Duration::from_secs(120), ffmpeg_cmd.output())
            .await
            .map_err(|_| anyhow::anyhow!("ffmpeg CAF→WAV conversion timed out after 120s"))?
            .context("ffmpeg CAF→WAV conversion failed")?;
        if !conv.status.success() {
            // Clean up any partial output file before propagating the error.
            let _ = tokio::fs::remove_file(&tmp).await;
            anyhow::bail!(
                "ffmpeg failed converting CAF (exit: {})",
                conv.status.code().unwrap_or(-1)
            );
        }
        (tmp.clone(), Some(tmp))
    };

    let out_dir = std::env::temp_dir().join(format!("zc_wpp_{}", uuid::Uuid::new_v4()));
    if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        if let Some(ref tmp) = caf_tmp {
            let _ = tokio::fs::remove_file(tmp).await;
        }
        return Err(anyhow::anyhow!(
            "Failed to create whisper-cli output dir: {e}"
        ));
    }
    // Restrict to owner-only to protect temp audio content from other processes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&out_dir, std::fs::Permissions::from_mode(0o700)).await;
    }
    let stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("audio");
    let out_base = out_dir.join(stem);

    let input_str = match input_path.to_str() {
        Some(s) => s,
        None => {
            let _ = tokio::fs::remove_dir_all(&out_dir).await;
            if let Some(ref tmp) = caf_tmp {
                let _ = tokio::fs::remove_file(tmp).await;
            }
            anyhow::bail!("Input audio path contains non-UTF-8 characters");
        }
    };
    let out_base_str = match out_base.to_str() {
        Some(s) => s,
        None => {
            let _ = tokio::fs::remove_dir_all(&out_dir).await;
            if let Some(ref tmp) = caf_tmp {
                let _ = tokio::fs::remove_file(tmp).await;
            }
            anyhow::bail!("Output base path contains non-UTF-8 characters");
        }
    };
    tracing::debug!(
        "whisper-cli: {} -m {} -otxt -of {} -np -nt {}",
        bin,
        model,
        out_base_str,
        input_str
    );

    let mut whisper_cmd = Command::new(bin);
    whisper_cmd.args([
        "-m",
        model,
        "-otxt",
        "-of",
        out_base_str,
        "-np", // no progress bar
        "-nt", // no timestamps
        input_str,
    ]);
    whisper_cmd.kill_on_drop(true);
    let result = tokio::time::timeout(std::time::Duration::from_secs(120), whisper_cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("whisper-cli timed out after 120s"))
        .and_then(|r| r.context("whisper-cli error"));

    // Clean up temp WAV regardless of outcome.
    if let Some(ref tmp) = caf_tmp {
        tokio::fs::remove_file(tmp).await.ok();
    }

    let output = match result {
        Ok(o) => o,
        Err(e) => {
            // Clean up out_dir before propagating the error (timeout or spawn failure).
            let _ = tokio::fs::remove_dir_all(&out_dir).await;
            return Err(e);
        }
    };
    // Log metadata only — stdout/stderr may contain transcript content.
    tracing::debug!(
        "whisper-cli exit={:?} stdout_bytes={} stderr_bytes={}",
        output.status.code(),
        output.stdout.len(),
        output.stderr.len()
    );

    if !output.status.success() {
        let _ = tokio::fs::remove_dir_all(&out_dir).await;
        anyhow::bail!(
            "whisper-cli failed (exit {:?}): {} bytes of stderr",
            output.status.code(),
            output.stderr.len()
        );
    }

    let txt_path = out_dir.join(format!("{stem}.txt"));
    let txt = tokio::fs::read_to_string(&txt_path).await.map_err(|e| {
        let _ = std::fs::remove_dir_all(&out_dir);
        anyhow::anyhow!("Failed to read whisper-cli transcript output: {e}")
    })?;
    let _ = tokio::fs::remove_dir_all(&out_dir).await;

    let text = txt.trim().to_string();
    anyhow::ensure!(!text.is_empty(), "whisper-cli produced empty transcript");
    Ok(text)
}

/// Transcribe using Python whisper CLI. Handles CAF natively via ffmpeg.
async fn transcribe_with_python_whisper(file_path: &str) -> anyhow::Result<String> {
    use std::path::Path;
    use tokio::process::Command;

    let whisper_bin = resolve_whisper_bin()
        .context("No whisper backend — install whisper-cpp (`brew install whisper-cpp`) or openai-whisper (`pip install openai-whisper`)")?;

    let tmp_dir = std::env::temp_dir().join(format!("zc_whisper_{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&tmp_dir)
        .await
        .context("Failed to create whisper temp dir")?;

    let tmp_dir_str = match tmp_dir.to_str() {
        Some(s) => s,
        None => {
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
            anyhow::bail!("Whisper temp dir path contains non-UTF-8 characters");
        }
    };

    let mut whisper_cmd = Command::new(whisper_bin);
    whisper_cmd.args([
        "--model",
        "turbo",
        "--output_format",
        "txt",
        "--output_dir",
        tmp_dir_str,
        "--verbose",
        "False",
        file_path,
    ]);
    whisper_cmd.kill_on_drop(true);
    let output =
        match tokio::time::timeout(std::time::Duration::from_secs(120), whisper_cmd.output()).await
        {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
                return Err(anyhow::anyhow!("whisper CLI error: {e}"));
            }
            Err(_) => {
                let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
                return Err(anyhow::anyhow!("whisper CLI timed out after 120s"));
            }
        };

    if !output.status.success() {
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        // Log byte counts only — stderr may contain transcript fragments.
        tracing::debug!(
            "whisper CLI failed: exit={:?} stderr_bytes={}",
            output.status.code(),
            output.stderr.len()
        );
        anyhow::bail!("whisper CLI failed (exit {:?})", output.status.code());
    }

    let stem = Path::new(file_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("audio");
    let txt_path = tmp_dir.join(format!("{stem}.txt"));
    let txt = match tokio::fs::read_to_string(&txt_path).await {
        Ok(t) => t,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
            return Err(anyhow::anyhow!("Failed to read whisper output: {e}"));
        }
    };
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;

    let text = txt.trim().to_string();
    anyhow::ensure!(!text.is_empty(), "whisper produced empty transcript");
    Ok(text)
}

/// Return `true` if any local whisper backend is available.
pub fn whisper_available() -> bool {
    resolve_whisper_cpp().is_some() || resolve_whisper_bin().is_some()
}

/// Resolve whisper-cli (whisper.cpp) binary and model. Returns `None` if either
/// is missing. Result is cached after first call.
fn resolve_whisper_cpp() -> Option<(&'static str, &'static str)> {
    static CACHE: std::sync::OnceLock<Option<(&'static str, &'static str)>> =
        std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        const BINS: &[&str] = &[
            "/opt/homebrew/bin/whisper-cli",
            "/usr/local/bin/whisper-cli",
        ];
        const MODELS: &[&str] = &[
            // Apple Silicon Homebrew
            "/opt/homebrew/share/whisper-cpp/ggml-base.bin",
            "/opt/homebrew/share/whisper-cpp/ggml-small.bin",
            "/opt/homebrew/share/whisper-cpp/ggml-tiny.bin",
            "/opt/homebrew/share/whisper-cpp/for-tests-ggml-tiny.bin",
            // Intel Homebrew (prefix /usr/local/opt/whisper-cpp)
            "/usr/local/opt/whisper-cpp/share/whisper-cpp/ggml-base.bin",
            "/usr/local/opt/whisper-cpp/share/whisper-cpp/ggml-small.bin",
            "/usr/local/opt/whisper-cpp/share/whisper-cpp/ggml-tiny.bin",
            "/usr/local/opt/whisper-cpp/share/whisper-cpp/for-tests-ggml-tiny.bin",
        ];
        let bin = BINS
            .iter()
            .copied()
            .find(|b| std::path::Path::new(b).is_file())?;
        let model = MODELS
            .iter()
            .copied()
            .find(|m| std::path::Path::new(m).is_file())?;
        Some((bin, model))
    })
}

/// Resolve the Python `whisper` binary path. Cached after first call.
fn resolve_whisper_bin() -> Option<&'static str> {
    static WHISPER_BIN: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *WHISPER_BIN.get_or_init(|| {
        const CANDIDATES: &[&str] = &[
            "whisper",
            "/opt/homebrew/bin/whisper",
            "/usr/local/bin/whisper",
        ];
        CANDIDATES.iter().copied().find(|bin| {
            if bin.starts_with('/') {
                std::path::Path::new(bin).is_file()
            } else {
                std::process::Command::new("which")
                    .arg(bin)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            }
        })
    })
}

/// Resolve the `ffmpeg` binary path. Used for CAF→WAV pre-conversion.
/// Cached after first call.
fn resolve_ffmpeg_bin() -> Option<&'static str> {
    static FFMPEG_BIN: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *FFMPEG_BIN.get_or_init(|| {
        const CANDIDATES: &[&str] = &[
            "ffmpeg",
            "/opt/homebrew/bin/ffmpeg",
            "/usr/local/bin/ffmpeg",
            "/usr/bin/ffmpeg",
        ];
        CANDIDATES.iter().copied().find(|bin| {
            if bin.starts_with('/') {
                std::path::Path::new(bin).is_file()
            } else {
                std::process::Command::new("which")
                    .arg(bin)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
            }
        })
    })
}

/// Transcribe audio bytes via a Whisper-compatible transcription API.
///
/// Returns the transcribed text on success.
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

    let api_key = config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            std::env::var("GROQ_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .context(
            "Missing transcription API key: set [transcription].api_key or GROQ_API_KEY environment variable",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_whisper_bin_returns_str_or_none() {
        // Just assert the function doesn't panic; result depends on local install.
        let _ = resolve_whisper_bin();
    }

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
        // Ensure fallback env key is absent for this test.
        std::env::remove_var("GROQ_API_KEY");

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
        std::env::remove_var("GROQ_API_KEY");

        let data = vec![0u8; 100];
        let mut config = TranscriptionConfig::default();
        config.api_key = Some("transcription-key".to_string());

        // Keep invalid extension so we fail before network, but after key resolution.
        let err = transcribe_audio(data, "recording.aac", &config)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Unsupported audio format"),
            "expected unsupported-format error, got: {err}"
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
}
