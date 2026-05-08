//! Screenshot capability — captures the current display(s) using the system
//! `screencapture` tool, which respects the Screen Recording TCC permission.
//!
//! Returns a base64-encoded PNG. The agent (or the dashboard webview during
//! testing) can render this directly via a `data:image/png;base64,…` URL.

use base64::Engine;
use serde::Serialize;
use std::process::Command;

#[derive(Debug, Serialize)]
pub struct ScreenshotResult {
    pub format: String,
    pub data: String,
}

/// Capture the screen and return a base64-encoded PNG.
///
/// Returns `permission_denied("screen_recording")` when TCC blocks the capture.
#[tauri::command]
pub fn take_screenshot() -> Result<ScreenshotResult, String> {
    #[cfg(target_os = "macos")]
    {
        use crate::macos::permissions;
        if permissions::check_screen_recording() != "granted" {
            return Err("permission_denied(screen_recording)".into());
        }

        let tmp = std::env::temp_dir().join(format!(
            "zeroclaw-screenshot-{}-{}.png",
            std::process::id(),
            chrono_ish_nanos()
        ));

        // -x silences shutter sound. -t png writes a PNG. -C captures cursor.
        let status = Command::new("/usr/sbin/screencapture")
            .args(["-x", "-t", "png"])
            .arg(&tmp)
            .status()
            .map_err(|e| format!("screencapture spawn failed: {e}"))?;

        if !status.success() {
            return Err(format!("screencapture exited with {status}"));
        }

        let bytes =
            std::fs::read(&tmp).map_err(|e| format!("failed to read captured image: {e}"))?;
        let _ = std::fs::remove_file(&tmp);

        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(ScreenshotResult {
            format: "png".into(),
            data: encoded,
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("Screenshot capability is currently macOS-only".into())
    }
}

#[cfg(target_os = "macos")]
fn chrono_ish_nanos() -> u128 {
    // Avoid pulling chrono into this module just for a tmpfile suffix.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
