//! macOS Accessibility (AXUIElement) automation.
//!
//! Uses the Accessibility API to inspect UI elements and perform actions
//! on other applications. Requires Accessibility permission in System Settings.

use serde::Serialize;
use tokio::process::Command;

#[derive(Debug, Serialize)]
pub struct UIElement {
    pub role: String,
    pub title: String,
    pub value: String,
    pub position: Option<String>,
    pub size: Option<String>,
    pub children_count: usize,
}

/// Inspect UI elements of a running application.
///
/// Uses AppleScript + System Events as the inspection mechanism, which is
/// more portable than direct AXUIElement FFI while providing equivalent
/// functionality for most automation tasks.
#[tauri::command]
pub async fn inspect_ui_element(
    app_name: String,
    role_filter: Option<String>,
) -> Result<Vec<UIElement>, String> {
    let escaped_app = app_name.replace('\\', "\\\\").replace('"', "\\\"");

    // Get UI elements via System Events — returns role, name, description.
    let script = format!(
        r#"tell application "System Events"
    tell process "{escaped_app}"
        set frontmost to true
        set uiList to {{}}
        repeat with w in windows
            set uiList to uiList & {{role of w, name of w, ""}}
            repeat with elem in UI elements of w
                try
                    set uiList to uiList & {{role of elem, name of elem, description of elem}}
                end try
            end repeat
        end repeat
        return uiList
    end tell
end tell"#
    );

    let output: std::process::Output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .await
        .map_err(|e| format!("osascript failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Accessibility inspection failed: {stderr}"));
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Parse the comma-separated AppleScript list into UIElement structs.
    // The list is flat: [role1, name1, desc1, role2, name2, desc2, ...]
    let parts: Vec<&str> = raw.split(", ").collect();
    let mut elements = Vec::new();

    for chunk in parts.chunks(3) {
        if chunk.len() == 3 {
            let role = chunk[0].to_string();
            let title = chunk[1].to_string();
            let value = chunk[2].to_string();

            // Apply role filter if provided.
            if let Some(ref filter) = role_filter {
                if !role.to_lowercase().contains(&filter.to_lowercase()) {
                    continue;
                }
            }

            elements.push(UIElement {
                role,
                title,
                value,
                position: None,
                size: None,
                children_count: 0,
            });
        }
    }

    Ok(elements)
}

/// Click a UI element in an application by matching its name/title.
#[tauri::command]
pub async fn click_ui_element(app_name: String, element_name: String) -> Result<(), String> {
    let escaped_app = app_name.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_elem = element_name.replace('\\', "\\\\").replace('"', "\\\"");

    let script = format!(
        r#"tell application "System Events"
    tell process "{escaped_app}"
        set frontmost to true
        click (first UI element whose name is "{escaped_elem}")
    end tell
end tell"#
    );

    let output: std::process::Output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .await
        .map_err(|e| format!("osascript failed: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Click failed: {stderr}"))
    }
}

/// Type text into a focused UI element in an application.
#[tauri::command]
pub async fn type_into_element(app_name: String, text: String) -> Result<(), String> {
    let escaped_app = app_name.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_text = text.replace('\\', "\\\\").replace('"', "\\\"");

    let script = format!(
        r#"tell application "System Events"
    tell process "{escaped_app}"
        set frontmost to true
        keystroke "{escaped_text}"
    end tell
end tell"#
    );

    let output: std::process::Output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .await
        .map_err(|e| format!("osascript failed: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Type failed: {stderr}"))
    }
}
