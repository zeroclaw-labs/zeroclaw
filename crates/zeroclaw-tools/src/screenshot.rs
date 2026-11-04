use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::media::{MarkerKind, RenderedMarker};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Maximum time to wait for a screenshot command to complete.
const SCREENSHOT_TIMEOUT_SECS: u64 = 15;

/// Tool for capturing screenshots using platform-native commands.
/// macOS: `screencapture`
/// Linux: tries `gnome-screenshot`, `scrot`, `import` (`ImageMagick`) in order.
pub struct ScreenshotTool {
    security: Arc<SecurityPolicy>,
}

impl ScreenshotTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
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

                Self::describe_saved_file(&output_path).await
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

    /// Read the saved screenshot file and describe it for the tool result.
    ///
    /// The saved file is declared as an image attachment; the text carries
    /// the path and size only.
    async fn describe_saved_file(output_path: &std::path::Path) -> anyhow::Result<ToolResult> {
        match tokio::fs::read(output_path).await {
            Ok(bytes) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Screenshot saved to: {}\nSize: {} bytes",
                    output_path.display(),
                    bytes.len(),
                )
                .into(),
                error: None,
            }
            .with_attachment(RenderedMarker {
                target: output_path.display().to_string(),
                kind: MarkerKind::Image,
            })),
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
        "Capture a screenshot of the current screen. Returns the saved file path."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
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
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.security.can_act() {
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
        let description = tool.description();
        assert!(description.contains("screenshot"));
        assert!(description.contains("Returns the saved file path"));
        assert!(!description.contains("base64"));
    }

    #[tokio::test]
    async fn screenshot_describe_saved_file_reports_path_and_size_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("minimal.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();

        let result = ScreenshotTool::describe_saved_file(&path).await.unwrap();

        assert!(result.success);
        let text = result.output.as_str();
        assert!(text.contains("Screenshot saved to:"), "text: {text}");
        assert!(text.contains("Size: 8 bytes"), "text: {text}");
        assert!(!text.contains("data:"), "text: {text}");
        assert!(!text.contains("base64"), "text: {text}");
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

    #[tokio::test]
    async fn describe_saved_file_declares_the_saved_png_as_attachment() {
        // The saved screenshot is a produced image: the tool must declare it
        // (exactly one image attachment) while leaving the text unchanged,
        // because nothing downstream scans the text for the path anymore.
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("shot.png");
        std::fs::write(&image, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();

        let result = ScreenshotTool::describe_saved_file(&image)
            .await
            .expect("describe_saved_file succeeds on a small PNG");

        assert!(result.success);
        assert_eq!(result.output.attachments().len(), 1);
        assert_eq!(result.output.attachments()[0].kind, MarkerKind::Image);
        assert_eq!(
            result.output.attachments()[0].target,
            image.display().to_string()
        );
        assert!(
            result
                .output
                .as_str()
                .contains(&format!("Screenshot saved to: {}", image.display())),
            "the text still names the saved path: {}",
            result.output.as_str()
        );
        assert!(
            !result.output.as_str().contains("[IMAGE:"),
            "the declaration never rides the text as marker syntax"
        );
    }
}
