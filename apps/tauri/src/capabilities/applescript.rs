//! AppleScript capability — runs arbitrary AppleScript via osascript, gated by
//! the macOS Automation TCC permission (per-target-app prompts handled by the
//! system). This is a *risky* capability and will be wrapped behind a per-app
//! approval allowlist when the full NodeClient lands (#6321 / #6499).
//!
//! For now, callers (the dashboard devtools console during testing) take
//! responsibility for not running scripts they wouldn't run themselves.

use std::process::Command;

/// Run an AppleScript snippet via `osascript -e`.
///
/// Returns the trimmed stdout on success, or the stderr from osascript on
/// failure (which usually surfaces the per-app TCC prompt rejection).
#[tauri::command]
pub fn run_applescript(code: String) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", &code])
            .output()
            .map_err(|e| format!("osascript spawn failed: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if stderr.is_empty() {
                format!("osascript exited with {}", output.status)
            } else {
                stderr
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = code;
        Err("AppleScript capability is currently macOS-only".into())
    }
}
