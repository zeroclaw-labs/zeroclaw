//! Native macOS notifications via osascript.

use tokio::process::Command;

/// Send a native macOS notification.
#[tauri::command]
pub async fn send_desktop_notification(
    title: String,
    body: String,
    subtitle: Option<String>,
    sound: Option<String>,
) -> Result<(), String> {
    let escaped_title = title.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_body = body.replace('\\', "\\\\").replace('"', "\\\"");

    let mut script =
        format!(r#"display notification "{escaped_body}" with title "{escaped_title}""#);

    if let Some(ref sub) = subtitle {
        let escaped_sub = sub.replace('\\', "\\\\").replace('"', "\\\"");
        script.push_str(&format!(r#" subtitle "{escaped_sub}""#));
    }

    let sound_name = sound.as_deref().unwrap_or("default");
    let escaped_sound = sound_name.replace('\\', "\\\\").replace('"', "\\\"");
    script.push_str(&format!(r#" sound name "{escaped_sound}""#));

    let status: std::process::ExitStatus = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .status()
        .await
        .map_err(|e| format!("osascript failed: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("Failed to display notification".into())
    }
}
