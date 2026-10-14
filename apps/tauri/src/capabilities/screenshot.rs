//! Screenshot capability — captures the current display(s) using the system
//! `screencapture` tool, which respects the Screen Recording TCC permission.
//!
//! Returns a base64-encoded PNG. Gateway-served content has no remote Tauri
//! capability; any future caller requires a separately reviewed ACL boundary.

#[cfg(target_os = "macos")]
use base64::Engine;
use serde::Serialize;
#[cfg(any(target_os = "macos", test))]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(any(target_os = "macos", test))]
struct TemporaryScreenshot {
    path: PathBuf,
}

#[cfg(any(target_os = "macos", test))]
impl Drop for TemporaryScreenshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(any(target_os = "macos", test))]
fn with_temporary_screenshot<T>(
    path: PathBuf,
    operation: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let temporary_screenshot = TemporaryScreenshot { path };
    operation(&temporary_screenshot.path)
}

#[derive(Debug, Serialize)]
pub struct ScreenshotResult {
    pub format: String,
    pub data: String,
}

/// Capture the screen and return a base64-encoded PNG.
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

        with_temporary_screenshot(tmp, |tmp| {
            // -x silences shutter sound. -t png writes a PNG. -C captures cursor.
            let status = Command::new("/usr/sbin/screencapture")
                .args(["-x", "-t", "png"])
                .arg(tmp)
                .status()
                .map_err(|e| format!("screencapture spawn failed: {e}"))?;

            if !status.success() {
                return Err(format!("screencapture exited with {status}"));
            }

            let bytes =
                std::fs::read(tmp).map_err(|e| format!("failed to read captured image: {e}"))?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            Ok(ScreenshotResult {
                format: "png".into(),
                data: encoded,
            })
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

#[cfg(test)]
mod tests {
    use super::with_temporary_screenshot;
    use std::path::PathBuf;

    fn test_path(case: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "zeroclaw-screenshot-test-{}-{}-{case}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn temporary_screenshot_is_removed_when_read_step_fails() {
        let path = test_path("read-error");
        std::fs::write(&path, b"captured png").expect("create screenshot fixture");

        let error = with_temporary_screenshot(path.clone(), |candidate| {
            assert!(candidate.exists(), "fixture should exist during read step");
            Err::<(), _>("failed to read captured image: simulated".to_string())
        })
        .expect_err("simulated read failure should be returned");

        assert_eq!(error, "failed to read captured image: simulated");
        assert!(!path.exists(), "temporary screenshot should be removed");
    }

    #[test]
    fn temporary_screenshot_is_removed_after_success() {
        let path = test_path("success");
        std::fs::write(&path, b"captured png").expect("create screenshot fixture");

        with_temporary_screenshot(path.clone(), |candidate| {
            std::fs::read(candidate)
                .map(|_| ())
                .map_err(|error| format!("failed to read captured image: {error}"))
        })
        .expect("read should succeed");

        assert!(!path.exists(), "temporary screenshot should be removed");
    }

    #[test]
    fn cleanup_failure_does_not_replace_primary_error() {
        let path = test_path("cleanup-error");
        std::fs::create_dir(&path).expect("create directory fixture");
        std::fs::write(path.join("keep"), b"non-empty").expect("make cleanup fail");

        let error = with_temporary_screenshot(path.clone(), |_| {
            Err::<(), _>("screencapture failed".to_string())
        })
        .expect_err("primary failure should be returned");

        assert_eq!(error, "screencapture failed");
        std::fs::remove_dir_all(path).expect("remove directory fixture");
    }
}
