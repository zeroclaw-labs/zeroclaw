//! Screen capture via macOS `screencapture` CLI.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[derive(Debug, Deserialize)]
pub struct ScreenRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Serialize)]
pub struct ScreenCaptureResult {
    pub base64: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// Capture the screen (or a region) and return the image as base64 PNG.
#[tauri::command]
pub async fn capture_screen(region: Option<ScreenRegion>) -> Result<ScreenCaptureResult, String> {
    let tmp = std::env::temp_dir().join(format!("zeroclaw_screenshot_{}.png", std::process::id()));
    let tmp_str = tmp.to_string_lossy().to_string();

    let mut cmd = Command::new("screencapture");
    cmd.arg("-x"); // no sound

    if let Some(ref r) = region {
        cmd.arg("-R")
            .arg(format!("{},{},{},{}", r.x, r.y, r.width, r.height));
    }

    cmd.arg(&tmp_str);

    let status: std::process::ExitStatus = cmd
        .status()
        .await
        .map_err(|e| format!("screencapture failed: {e}"))?;

    if !status.success() {
        return Err(
            "screencapture exited with error — is Screen Recording permission granted?".into(),
        );
    }

    let bytes = tokio::fs::read(&tmp)
        .await
        .map_err(|e| format!("Failed to read screenshot: {e}"))?;

    let _ = tokio::fs::remove_file(&tmp).await;

    Ok(ScreenCaptureResult {
        base64: STANDARD.encode(&bytes),
        width: region.as_ref().map(|r| r.width),
        height: region.as_ref().map(|r| r.height),
    })
}
