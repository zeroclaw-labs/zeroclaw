//! LightWave macOS Desktop Automation
//!
//! Native macOS desktop control for Augusta.
//! Exports an `execute` function for action-based dispatch.
//! The `Tool` trait wrapper lives in lightwave-sys to avoid cyclic deps.

pub mod app;
pub mod clipboard;
pub mod input;
pub mod permission;
pub mod screen;
pub mod system;
pub mod types;
pub mod window;

use anyhow::Result;
use serde_json::{json, Value};

/// Tool name constant.
pub const TOOL_NAME: &str = "mac_desktop";

/// Tool description constant.
pub const TOOL_DESCRIPTION: &str =
    "macOS desktop automation: window management, mouse/keyboard input, screen capture, \
     app control, clipboard, and system info. Use `action` parameter to select operation.";

/// JSON Schema for the tool parameters.
pub fn parameters_schema() -> Value {
    json!({
        "type": "object",
        "required": ["action"],
        "properties": {
            "action": {
                "type": "string",
                "description": "Action to perform",
                "enum": [
                    "window.list", "window.focus",
                    "input.click", "input.move", "input.type", "input.key",
                    "screen.capture", "screen.displays",
                    "app.list", "app.frontmost", "app.launch", "app.quit",
                    "clipboard.get", "clipboard.set",
                    "system.info", "system.battery",
                    "permission.check"
                ]
            },
            "pid": {
                "type": "integer",
                "description": "Process ID (for window.focus)"
            },
            "x": {
                "type": "number",
                "description": "X coordinate (for input.click, input.move)"
            },
            "y": {
                "type": "number",
                "description": "Y coordinate (for input.click, input.move)"
            },
            "text": {
                "type": "string",
                "description": "Text to type (for input.type) or clipboard content (for clipboard.set)"
            },
            "key": {
                "type": "string",
                "description": "Key name (for input.key): Return, Escape, Tab, Space, etc."
            },
            "name": {
                "type": "string",
                "description": "App name or bundle ID (for app.launch, app.quit)"
            }
        }
    })
}

/// Execute result — matches lightwave-sys ToolResult shape.
pub struct ExecuteResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Execute a mac_desktop action.
pub fn execute(args: &Value) -> ExecuteResult {
    match execute_inner(args) {
        Ok(result) => ExecuteResult {
            success: true,
            output: serde_json::to_string_pretty(&result).unwrap_or_default(),
            error: None,
        },
        Err(e) => ExecuteResult {
            success: false,
            output: format!("Error: {e}"),
            error: Some(e.to_string()),
        },
    }
}

fn execute_inner(args: &Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action {
        // ── Window actions ─────────────────────────
        "window.list" => {
            let windows = window::list_windows()?;
            Ok(json!({ "windows": windows }))
        }
        "window.focus" => {
            let pid = args
                .get("pid")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("pid required for window.focus"))?;
            window::focus_window(pid as i32)?;
            Ok(json!({ "focused": true, "pid": pid }))
        }

        // ── Input actions ──────────────────────────
        "input.click" => {
            let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            input::click(x, y)?;
            Ok(json!({ "clicked": true, "x": x, "y": y }))
        }
        "input.move" => {
            let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
            input::move_mouse(x, y)?;
            Ok(json!({ "moved": true, "x": x, "y": y }))
        }
        "input.type" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            input::type_text(text)?;
            Ok(json!({ "typed": true, "length": text.len() }))
        }
        "input.key" => {
            let key = args
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("key required for input.key"))?;
            input::key_press(key)?;
            Ok(json!({ "pressed": true, "key": key }))
        }

        // ── Screen actions ─────────────────────────
        "screen.capture" => {
            let b64 = screen::capture_screen_base64()?;
            Ok(json!({ "format": "png", "encoding": "base64", "data": b64 }))
        }
        "screen.displays" => {
            let displays = screen::list_displays()?;
            Ok(json!({ "displays": displays }))
        }

        // ── App actions ────────────────────────────
        "app.list" => {
            let apps = app::list_apps()?;
            Ok(json!({ "apps": apps }))
        }
        "app.frontmost" => {
            let front = app::frontmost_app()?;
            Ok(json!({ "frontmost": front }))
        }
        "app.launch" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("name required for app.launch"))?;
            app::launch_app(name)?;
            Ok(json!({ "launched": true, "app": name }))
        }
        "app.quit" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("name required for app.quit"))?;
            app::quit_app(name)?;
            Ok(json!({ "quit": true, "app": name }))
        }

        // ── Clipboard actions ──────────────────────
        "clipboard.get" => {
            let content = clipboard::get_clipboard()?;
            Ok(json!({ "content": content }))
        }
        "clipboard.set" => {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("text required for clipboard.set"))?;
            clipboard::set_clipboard(text)?;
            Ok(json!({ "set": true, "length": text.len() }))
        }

        // ── System actions ─────────────────────────
        "system.info" => {
            let info = system::system_info()?;
            Ok(serde_json::to_value(info)?)
        }
        "system.battery" => {
            let battery = system::battery_info()?;
            Ok(serde_json::to_value(battery)?)
        }

        // ── Permission check ───────────────────────
        "permission.check" => {
            let status = permission::check_permissions();
            Ok(serde_json::to_value(status)?)
        }

        _ => anyhow::bail!(
            "Unknown action: '{}'. Use one of: window.list, window.focus, input.click, \
             input.move, input.type, input.key, screen.capture, screen.displays, app.list, \
             app.frontmost, app.launch, app.quit, clipboard.get, clipboard.set, system.info, \
             system.battery, permission.check",
            action
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_is_mac_desktop() {
        assert_eq!(TOOL_NAME, "mac_desktop");
    }

    #[test]
    fn parameters_schema_has_required_action() {
        let schema = parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "action");
    }

    #[test]
    fn parameters_schema_lists_all_actions() {
        let schema = parameters_schema();
        let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
        assert_eq!(actions.len(), 17);
        assert!(actions.contains(&json!("window.list")));
        assert!(actions.contains(&json!("clipboard.set")));
        assert!(actions.contains(&json!("permission.check")));
    }

    #[test]
    fn execute_unknown_action_fails() {
        let result = execute(&json!({"action": "nonexistent"}));
        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.output.contains("Unknown action"));
    }

    #[test]
    fn execute_missing_action_fails() {
        let result = execute(&json!({}));
        assert!(!result.success);
        assert!(result.output.contains("Unknown action"));
    }

    #[test]
    fn execute_window_focus_without_pid_fails() {
        let result = execute(&json!({"action": "window.focus"}));
        assert!(!result.success);
        assert!(result.output.contains("pid required"));
    }

    #[test]
    fn execute_input_key_without_key_fails() {
        let result = execute(&json!({"action": "input.key"}));
        assert!(!result.success);
        assert!(result.output.contains("key required"));
    }

    #[test]
    fn execute_app_launch_without_name_fails() {
        let result = execute(&json!({"action": "app.launch"}));
        assert!(!result.success);
        assert!(result.output.contains("name required"));
    }

    #[test]
    fn execute_app_quit_without_name_fails() {
        let result = execute(&json!({"action": "app.quit"}));
        assert!(!result.success);
        assert!(result.output.contains("name required"));
    }

    #[test]
    fn execute_clipboard_set_without_text_fails() {
        let result = execute(&json!({"action": "clipboard.set"}));
        assert!(!result.success);
        assert!(result.output.contains("text required"));
    }

    // ── macOS-only integration tests ─────────────────────

    #[test]
    #[cfg(target_os = "macos")]
    fn permission_check_returns_success() {
        let result = execute(&json!({"action": "permission.check"}));
        assert!(result.success);
        let val: Value = serde_json::from_str(&result.output).unwrap();
        assert!(val.get("accessibility").is_some());
        assert!(val.get("screen_capture").is_some());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn system_info_returns_valid_data() {
        let result = execute(&json!({"action": "system.info"}));
        assert!(result.success);
        let val: Value = serde_json::from_str(&result.output).unwrap();
        assert!(!val["hostname"].as_str().unwrap_or("").is_empty());
        assert!(val["cpu_cores"].as_u64().unwrap_or(0) > 0);
        assert!(val["memory_gb"].as_f64().unwrap_or(0.0) > 0.0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn battery_info_returns_success() {
        let result = execute(&json!({"action": "system.battery"}));
        assert!(result.success);
        let val: Value = serde_json::from_str(&result.output).unwrap();
        // level may be null on desktops, but charging/on_battery should exist
        assert!(val.get("charging").is_some());
        assert!(val.get("on_battery").is_some());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn clipboard_roundtrip() {
        let test_str = "lightwave-macos-test-12345";
        let set_result = execute(&json!({"action": "clipboard.set", "text": test_str}));
        assert!(set_result.success);

        let get_result = execute(&json!({"action": "clipboard.get"}));
        assert!(get_result.success);
        let val: Value = serde_json::from_str(&get_result.output).unwrap();
        assert_eq!(val["content"].as_str().unwrap(), test_str);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn app_list_returns_apps() {
        let result = execute(&json!({"action": "app.list"}));
        assert!(result.success);
        let val: Value = serde_json::from_str(&result.output).unwrap();
        let apps = val["apps"].as_array().unwrap();
        // At least Finder should always be running
        assert!(!apps.is_empty());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn app_frontmost_returns_result() {
        let result = execute(&json!({"action": "app.frontmost"}));
        assert!(result.success);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn screen_displays_returns_at_least_one() {
        let result = execute(&json!({"action": "screen.displays"}));
        assert!(result.success);
        let val: Value = serde_json::from_str(&result.output).unwrap();
        let displays = val["displays"].as_array().unwrap();
        assert!(!displays.is_empty());
        assert!(displays[0]["width"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn window_list_succeeds() {
        let result = execute(&json!({"action": "window.list"}));
        assert!(result.success);
        let val: Value = serde_json::from_str(&result.output).unwrap();
        assert!(val.get("windows").is_some());
    }
}
