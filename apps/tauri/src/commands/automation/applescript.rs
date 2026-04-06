//! AppleScript execution — predefined safe actions and optional raw mode.

use serde::Deserialize;
use tokio::process::Command;

/// Escape a string for safe interpolation into AppleScript.
/// Mirrors the pattern in `src/channels/imessage.rs`.
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

async fn run_osascript(script: &str) -> Result<String, String> {
    let output: std::process::Output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .await
        .map_err(|e| format!("Failed to run osascript: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("AppleScript error: {stderr}"))
    }
}

#[derive(Debug, Deserialize)]
pub struct AppleScriptParams {
    pub bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub text: Option<String>,
    pub path: Option<String>,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub level: Option<i32>,
    pub enabled: Option<bool>,
}

/// Execute a predefined AppleScript action by name.
///
/// Actions are built from validated parameters — no raw user strings are
/// interpolated without escaping.
#[tauri::command]
pub async fn run_applescript_action(
    action: String,
    params: AppleScriptParams,
) -> Result<String, String> {
    let script = match action.as_str() {
        "app.launch" => {
            let bid = params.bundle_id.as_deref().ok_or("Missing bundle_id")?;
            format!(
                r#"tell application id "{}" to launch"#,
                escape_applescript(bid)
            )
        }
        "app.activate" => {
            let bid = params.bundle_id.as_deref().ok_or("Missing bundle_id")?;
            format!(
                r#"tell application id "{}" to activate"#,
                escape_applescript(bid)
            )
        }
        "app.quit" => {
            let bid = params.bundle_id.as_deref().ok_or("Missing bundle_id")?;
            format!(
                r#"tell application id "{}" to quit"#,
                escape_applescript(bid)
            )
        }
        "window.bounds" => {
            let app = params.app_name.as_deref().ok_or("Missing app_name")?;
            format!(
                r#"tell application "{}" to get bounds of front window"#,
                escape_applescript(app)
            )
        }
        "window.set_bounds" => {
            let app = params.app_name.as_deref().ok_or("Missing app_name")?;
            let x = params.x.ok_or("Missing x")?;
            let y = params.y.ok_or("Missing y")?;
            let w = params.width.ok_or("Missing width")?;
            let h = params.height.ok_or("Missing height")?;
            format!(
                r#"tell application "{}" to set bounds of front window to {{{}, {}, {}, {}}}"#,
                escape_applescript(app),
                x,
                y,
                x + w,
                y + h
            )
        }
        "clipboard.get" => r#"the clipboard"#.to_string(),
        "clipboard.set" => {
            let text = params.text.as_deref().ok_or("Missing text")?;
            format!(
                r#"set the clipboard to "{}""#,
                escape_applescript(text)
            )
        }
        "finder.open" => {
            let path = params.path.as_deref().ok_or("Missing path")?;
            format!(
                r#"tell application "Finder" to open POSIX file "{}""#,
                escape_applescript(path)
            )
        }
        "system.volume" => {
            let level = params.level.ok_or("Missing level")?;
            let clamped = level.clamp(0, 100);
            format!("set volume output volume {clamped}")
        }
        "system.dark_mode" => {
            let enabled = params.enabled.ok_or("Missing enabled")?;
            format!(
                r#"tell application "System Events" to tell appearance preferences to set dark mode to {}"#,
                if enabled { "true" } else { "false" }
            )
        }
        "system.get_frontmost_app" => {
            r#"tell application "System Events" to get name of first application process whose frontmost is true"#.to_string()
        }
        "system.get_running_apps" => {
            r#"tell application "System Events" to get name of every application process whose background only is false"#.to_string()
        }
        _ => return Err(format!("Unknown action: {action}")),
    };

    run_osascript(&script).await
}

/// Deny-list of dangerous AppleScript constructs.
const RAW_DENY_PATTERNS: &[&str] = &[
    "do shell script",
    "run script",
    "system attribute",
    "call method",
    "current application",
    "NSAppleScript",
    "do javascript",
];

/// Execute raw AppleScript. Disabled by default — callers must opt in.
/// Scripts are checked against a deny-list of dangerous constructs.
#[tauri::command]
pub async fn run_applescript_raw(script: String) -> Result<String, String> {
    let lower = script.to_lowercase();
    for pattern in RAW_DENY_PATTERNS {
        if lower.contains(&pattern.to_lowercase()) {
            return Err(format!(
                "Blocked: script contains disallowed construct '{pattern}'"
            ));
        }
    }
    run_osascript(&script).await
}
