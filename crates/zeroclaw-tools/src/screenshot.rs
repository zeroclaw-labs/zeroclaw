use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Maximum time to wait for a screenshot command to complete.
const SCREENSHOT_TIMEOUT_SECS: u64 = 15;
/// Maximum base64 payload size to return (2 MB of base64 ≈ 1.5 MB image).
const MAX_BASE64_BYTES: usize = 2_097_152;

/// Native desktop capture spawns a process and writes into the workspace, so it retains the
/// existing mutating-autonomy gate. An Android bridge capture is a read-only observation through
/// an already-authorized AccessibilityService and must remain available in read-only profiles.
fn requires_action_permission(android_bridge_available: bool) -> bool {
    !android_bridge_available
}

/// Tool for capturing screenshots using platform-native commands.
/// macOS: `screencapture`
/// Linux: tries `gnome-screenshot`, `scrot`, `import` (`ImageMagick`) in order.
/// Android: none of those exist and an app UID may not run them, so the capture is delegated to
/// the accessibility bridge when one is wired in (see `with_android_bridge`).
pub struct ScreenshotTool {
    security: Arc<SecurityPolicy>,
    /// Present only on Android, where the subprocess path cannot work at all.
    #[cfg(all(unix, target_os = "android"))]
    android_bridge: Option<crate::android::AndroidBridgeClient>,
    #[cfg(all(unix, target_os = "android"))]
    android_max_width: u32,
}

impl ScreenshotTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            #[cfg(all(unix, target_os = "android"))]
            android_bridge: None,
            #[cfg(all(unix, target_os = "android"))]
            android_max_width: 540,
        }
    }

    /// Route captures through the Android accessibility bridge.
    ///
    /// Without this, `screenshot` is dead on Android: it shells out to `screencapture` or
    /// `gnome-screenshot`, none of which exist there, so every call returns "not supported". A
    /// model that reaches for the familiar name then has no image and no working alternative under
    /// that name. Delegating means one behaviour under both names rather than a trap.
    #[cfg(all(unix, target_os = "android"))]
    #[must_use]
    pub fn with_android_bridge(
        mut self,
        client: crate::android::AndroidBridgeClient,
        max_width: u32,
    ) -> Self {
        self.android_bridge = Some(client);
        self.android_max_width = max_width;
        self
    }

    /// Determine the screenshot command for the current platform.
    fn screenshot_command(output_path: &str) -> Option<Vec<String>> {
        if cfg!(target_os = "macos") {
            Some(vec![
                "screencapture".into(),
                "-x".into(), // no sound
                output_path.into(),
            ])
        } else if cfg!(target_os = "linux") {
            Some(vec![
                "sh".into(),
                "-c".into(),
                format!(
                    "if command -v gnome-screenshot >/dev/null 2>&1; then \
                         gnome-screenshot -f '{output_path}'; \
                     elif command -v scrot >/dev/null 2>&1; then \
                         scrot '{output_path}'; \
                     elif command -v import >/dev/null 2>&1; then \
                         import -window root '{output_path}'; \
                     else \
                         echo 'NO_SCREENSHOT_TOOL' >&2; exit 1; \
                     fi"
                ),
            ])
        } else {
            None
        }
    }

    /// Execute the screenshot capture and return the result.
    async fn capture(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // On Android the subprocess path below cannot succeed, so take the bridge when we have
        // one. The same expect_package guard applies as on android_screenshot: offering a second
        // name for the same job must not offer a second, unguarded way to do it.
        #[cfg(all(unix, target_os = "android"))]
        if let Some(client) = &self.android_bridge {
            if let Some(expected) = args
                .get("expect_package")
                .and_then(serde_json::Value::as_str)
            {
                client.assert_foreground(expected).await?;
            }
            let max_width = args
                .get("max_width")
                .and_then(serde_json::Value::as_u64)
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(self.android_max_width);
            let data = client
                .call("screenshot", json!({ "max_width": max_width }))
                .await?;
            return crate::android::screenshot::render_screenshot(&data);
        }

        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let filename = args
            .get("filename")
            .and_then(|v| v.as_str())
            .map_or_else(|| format!("screenshot_{timestamp}.png"), String::from);

        // Sanitize filename to prevent path traversal
        let safe_name = PathBuf::from(&filename).file_name().map_or_else(
            || format!("screenshot_{timestamp}.png"),
            |n| n.to_string_lossy().to_string(),
        );

        // Reject filenames with shell-breaking characters to prevent injection in sh -c
        const SHELL_UNSAFE: &[char] = &[
            '\'', '"', '`', '$', '\\', ';', '|', '&', '\n', '\0', '(', ')',
        ];
        if safe_name.contains(SHELL_UNSAFE) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Filename contains characters unsafe for shell execution".into()),
            });
        }

        let output_path = self.security.workspace_dir.join(&safe_name);
        let output_str = output_path.to_string_lossy().to_string();

        let Some(mut cmd_args) = Self::screenshot_command(&output_str) else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Screenshot not supported on this platform".into()),
            });
        };

        // macOS region flags
        if cfg!(target_os = "macos")
            && let Some(region) = args.get("region").and_then(|v| v.as_str())
        {
            match region {
                "selection" => cmd_args.insert(1, "-s".into()),
                "window" => cmd_args.insert(1, "-w".into()),
                _ => {} // ignore unknown regions
            }
        }

        let program = cmd_args.remove(0);
        let result = tokio::time::timeout(
            Duration::from_secs(SCREENSHOT_TIMEOUT_SECS),
            tokio::process::Command::new(&program)
                .args(&cmd_args)
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if stderr.contains("NO_SCREENSHOT_TOOL") {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(
                                "No screenshot tool found. Install gnome-screenshot, scrot, or ImageMagick."
                                    .into(),
                            ),
                        });
                    }
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("Screenshot command failed: {stderr}")),
                    });
                }

                Self::read_and_encode(&output_path).await
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to execute screenshot command: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Screenshot timed out after {SCREENSHOT_TIMEOUT_SECS}s"
                )),
            }),
        }
    }

    /// Read the screenshot file and return base64-encoded result.
    async fn read_and_encode(output_path: &std::path::Path) -> anyhow::Result<ToolResult> {
        // Check file size before reading to prevent OOM on large screenshots
        const MAX_RAW_BYTES: u64 = 1_572_864; // ~1.5 MB (base64 expands ~33%)
        if let Ok(meta) = tokio::fs::metadata(output_path).await
            && meta.len() > MAX_RAW_BYTES
        {
            return Ok(ToolResult {
                success: true,
                output: format!(
                    "Screenshot saved to: {}\nSize: {} bytes (too large to base64-encode inline)",
                    output_path.display(),
                    meta.len(),
                )
                .into(),
                error: None,
            });
        }

        match tokio::fs::read(output_path).await {
            Ok(bytes) => {
                use base64::Engine;
                let size = bytes.len();
                let mut encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let truncated = if encoded.len() > MAX_BASE64_BYTES {
                    let mut boundary = MAX_BASE64_BYTES.min(encoded.len());
                    while boundary > 0 && !encoded.is_char_boundary(boundary) {
                        boundary -= 1;
                    }
                    encoded.truncate(boundary);
                    true
                } else {
                    false
                };

                let mut output_msg = format!(
                    "Screenshot saved to: {}\nSize: {size} bytes\nBase64 length: {}",
                    output_path.display(),
                    encoded.len(),
                );
                if truncated {
                    output_msg.push_str(" (truncated)");
                }
                let mime = match output_path.extension().and_then(|e| e.to_str()) {
                    Some("jpg" | "jpeg") => "image/jpeg",
                    Some("bmp") => "image/bmp",
                    Some("gif") => "image/gif",
                    Some("webp") => "image/webp",
                    _ => "image/png",
                };
                let _ = write!(output_msg, "\ndata:{mime};base64,{encoded}");

                Ok(ToolResult {
                    success: true,
                    output: output_msg.into(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: format!("Screenshot saved to: {}", output_path.display()).into(),
                error: Some(format!("Failed to read screenshot file: {e}")),
            }),
        }
    }
}

#[async_trait]
impl Tool for ScreenshotTool {
    fn name(&self) -> &str {
        "screenshot"
    }

    fn description(&self) -> &str {
        "Capture a screenshot of the current screen. Returns the file path and base64-encoded PNG data."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        // Only the android cfg below mutates this; on every other target it is built and returned
        // as-is, so the binding reads as needlessly mutable there.
        #[cfg_attr(not(all(unix, target_os = "android")), allow(unused_mut))]
        let mut schema = json!({
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": "Optional filename (default: screenshot_<timestamp>.png). Saved in workspace."
                },
                "region": {
                    "type": "string",
                    "description": "Optional region for macOS: 'selection' for interactive crop, 'window' for front window. Ignored on Linux."
                }
            }
        });
        // On Android this tool captures through the accessibility bridge, so it accepts the same
        // arguments as android_screenshot. Advertise them, or the guard is unreachable through
        // this name and a caller has no way to say which app it means.
        #[cfg(all(unix, target_os = "android"))]
        if self.android_bridge.is_some()
            && let Some(props) = schema
                .get_mut("properties")
                .and_then(serde_json::Value::as_object_mut)
        {
            props.insert(
                "expect_package".into(),
                json!({
                    "type": "string",
                    "description": crate::android::tool_msg("tool-android-action-param-expect-package")
                }),
            );
            props.insert(
                "max_width".into(),
                json!({
                    "type": "integer",
                    "description": crate::android::tool_msg("tool-android-screenshot-param-max-width")
                }),
            );
        }
        schema
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        #[cfg(all(unix, target_os = "android"))]
        let bridge_backed = self.android_bridge.is_some();
        #[cfg(not(all(unix, target_os = "android")))]
        let bridge_backed = false;

        if requires_action_permission(bridge_backed) && !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }
        self.capture(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn screenshot_tool_name() {
        let tool = ScreenshotTool::new(test_security());
        assert_eq!(tool.name(), "screenshot");
    }

    #[test]
    fn screenshot_tool_description() {
        let tool = ScreenshotTool::new(test_security());
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("screenshot"));
    }

    #[test]
    fn bridge_capture_does_not_require_mutating_autonomy() {
        assert!(!requires_action_permission(true));
        assert!(requires_action_permission(false));
    }

    #[test]
    fn screenshot_tool_schema() {
        let tool = ScreenshotTool::new(test_security());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["filename"].is_object());
        assert!(schema["properties"]["region"].is_object());
    }

    #[test]
    fn screenshot_tool_spec() {
        let tool = ScreenshotTool::new(test_security());
        let spec = tool.spec();
        assert_eq!(spec.name, "screenshot");
        assert!(spec.parameters.is_object());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn screenshot_command_exists() {
        let cmd = ScreenshotTool::screenshot_command("/tmp/test.png");
        assert!(cmd.is_some());
        let args = cmd.unwrap();
        assert!(!args.is_empty());
    }

    #[tokio::test]
    async fn screenshot_rejects_shell_injection_filename() {
        let tool = ScreenshotTool::new(test_security());
        let result = tool
            .execute(json!({"filename": "test'injection.png"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("unsafe for shell execution"));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn screenshot_command_contains_output_path() {
        let cmd = ScreenshotTool::screenshot_command("/tmp/my_screenshot.png").unwrap();
        let joined = cmd.join(" ");
        assert!(
            joined.contains("/tmp/my_screenshot.png"),
            "Command should contain the output path"
        );
    }

    #[test]
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn screenshot_command_is_unsupported_on_other_platforms() {
        assert!(ScreenshotTool::screenshot_command("screenshot.png").is_none());
    }
}
