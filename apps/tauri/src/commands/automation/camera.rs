//! Camera capture via `imagesnap` or `ffmpeg` CLI.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::process::Command;

/// Capture a photo from the built-in camera and return it as base64 JPEG.
///
/// Tries `imagesnap` first (commonly available), falls back to `ffmpeg`.
#[tauri::command]
pub async fn capture_photo() -> Result<String, String> {
    let tmp = std::env::temp_dir().join(format!("zeroclaw_camera_{}.jpg", std::process::id()));
    let tmp_str = tmp.to_string_lossy().to_string();

    // Try imagesnap first.
    let snap: Result<std::process::ExitStatus, _> = Command::new("imagesnap")
        .args(["-w", "1.0", &tmp_str])
        .status()
        .await;

    let captured = match snap {
        Ok(s) if s.success() => true,
        _ => {
            // Fallback: ffmpeg with AVFoundation input.
            let ff: Result<std::process::ExitStatus, _> = Command::new("ffmpeg")
                .args([
                    "-f",
                    "avfoundation",
                    "-framerate",
                    "30",
                    "-i",
                    "0",
                    "-frames:v",
                    "1",
                    "-y",
                    &tmp_str,
                ])
                .status()
                .await;
            matches!(ff, Ok(s) if s.success())
        }
    };

    if !captured {
        return Err(
            "Camera capture failed. Ensure Camera permission is granted and either `imagesnap` or `ffmpeg` is installed.".into()
        );
    }

    let bytes = tokio::fs::read(&tmp)
        .await
        .map_err(|e| format!("Failed to read camera image: {e}"))?;

    let _ = tokio::fs::remove_file(&tmp).await;

    Ok(STANDARD.encode(&bytes))
}
