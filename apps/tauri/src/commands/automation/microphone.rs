//! Microphone audio capture via `ffmpeg`.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::process::Command;

/// Record audio from the default microphone for the given duration (seconds).
/// Returns the audio as base64-encoded WAV.
#[tauri::command]
pub async fn record_audio(duration_secs: u32) -> Result<String, String> {
    let duration = duration_secs.clamp(1, 60); // Cap at 60 seconds.

    let tmp = std::env::temp_dir().join(format!("zeroclaw_audio_{}.wav", std::process::id()));
    let tmp_str = tmp.to_string_lossy().to_string();

    let status: std::process::ExitStatus = Command::new("ffmpeg")
        .args([
            "-f",
            "avfoundation",
            "-i",
            ":0", // default audio device
            "-t",
            &duration.to_string(),
            "-y",
            &tmp_str,
        ])
        .status()
        .await
        .map_err(|e| format!("ffmpeg not found or failed: {e}"))?;

    if !status.success() {
        return Err(
            "Audio recording failed. Ensure Microphone permission is granted and `ffmpeg` is installed.".into()
        );
    }

    let bytes = tokio::fs::read(&tmp)
        .await
        .map_err(|e| format!("Failed to read audio file: {e}"))?;

    let _ = tokio::fs::remove_file(&tmp).await;

    Ok(STANDARD.encode(&bytes))
}
