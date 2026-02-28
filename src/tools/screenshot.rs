use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Maximum time to wait for a screenshot command to complete.
const SCREENSHOT_TIMEOUT_SECS: u64 = 15;
/// Maximum base64 payload size to return (2 MB of base64 ≈ 1.5 MB image).
const MAX_BASE64_BYTES: usize = 2_097_152;

/// Tool for capturing screenshots using platform-native commands.
///
/// macOS: `screencapture`
/// Linux: tries `gnome-screenshot`, `scrot`, `import` (`ImageMagick`) in order.
pub struct ScreenshotTool {
    security: Arc<SecurityPolicy>,
}

impl ScreenshotTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }

    fn sanitize_output_filename(filename: &str, fallback: &str) -> String {
        let Some(basename) = Path::new(filename)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            return fallback.to_string();
        };

        let trimmed = basename.trim();
        if trimmed.is_empty() || trimmed == "." || trimmed == ".." || trimmed.contains('\0') {
            return fallback.to_string();
        }

        trimmed.to_string()
    }

    /// Resolve screenshot output path and block writes through symlink targets.
    async fn resolve_output_path_for_write(&self, filename: &str) -> anyhow::Result<PathBuf> {
        tokio::fs::create_dir_all(&self.security.workspace_dir).await?;

        let workspace_root = tokio::fs::canonicalize(&self.security.workspace_dir)
            .await
            .unwrap_or_else(|_| self.security.workspace_dir.clone());
        let output_path = workspace_root.join(filename);

        // Parent must remain inside workspace after resolution.
        let parent = output_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Invalid screenshot output path"))?;
        let resolved_parent = tokio::fs::canonicalize(parent).await?;
        if !self.security.is_resolved_path_allowed(&resolved_parent) {
            anyhow::bail!(
                "{}",
                self.security
                    .resolved_path_violation_message(&resolved_parent)
            );
        }

        match tokio::fs::symlink_metadata(&output_path).await {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    anyhow::bail!(
                        "Refusing to write screenshot through symlink: {}",
                        output_path.display()
                    );
                }
                if !meta.is_file() {
                    anyhow::bail!(
                        "Screenshot output path is not a regular file: {}",
                        output_path.display()
                    );
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }

        Ok(output_path)
    }

    /// Determine candidate screenshot commands for the current platform.
    fn screenshot_commands(output_path: &str) -> Vec<Vec<String>> {
        if cfg!(target_os = "macos") {
            vec![vec![
                "screencapture".into(),
                "-x".into(), // no sound
                output_path.into(),
            ]]
        } else if cfg!(target_os = "linux") {
            vec![
                vec!["gnome-screenshot".into(), "-f".into(), output_path.into()],
                vec!["scrot".into(), output_path.into()],
                vec![
                    "import".into(),
                    "-window".into(),
                    "root".into(),
                    output_path.into(),
                ],
            ]
        } else {
            Vec::new()
        }
    }

    /// Execute the screenshot capture and return the result.
    async fn capture(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let filename = args
            .get("filename")
            .and_then(|v| v.as_str())
            .map_or_else(|| format!("screenshot_{timestamp}.png"), String::from);

        let fallback_name = format!("screenshot_{timestamp}.png");
        // Keep only a safe basename and reject dot-segment escapes.
        let safe_name = Self::sanitize_output_filename(&filename, &fallback_name);

        // Keep conservative filtering for unusual shell/control chars.
        const SHELL_UNSAFE: &[char] = &[
            '\'', '"', '`', '$', '\\', ';', '|', '&', '\n', '\0', '(', ')',
        ];
        if safe_name.contains(SHELL_UNSAFE) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Filename contains characters unsafe for shell execution".into()),
            });
        }

        let output_path = match self.resolve_output_path_for_write(&safe_name).await {
            Ok(path) => path,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Invalid screenshot output path: {e}")),
                });
            }
        };
        let output_str = output_path.to_string_lossy().to_string();

        let mut commands = Self::screenshot_commands(&output_str);
        if commands.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Screenshot not supported on this platform".into()),
            });
        }

        // macOS region flags
        if cfg!(target_os = "macos") {
            if let Some(region) = args.get("region").and_then(|v| v.as_str()) {
                match region {
                    "selection" => commands[0].insert(1, "-s".into()),
                    "window" => commands[0].insert(1, "-w".into()),
                    _ => {} // ignore unknown regions
                }
            }
        }

        let mut saw_spawnable_command = false;
        let mut last_failure: Option<String> = None;

        for mut cmd_args in commands {
            if cmd_args.is_empty() {
                continue;
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
                    saw_spawnable_command = true;
                    if output.status.success() {
                        return Self::read_and_encode(&output_path).await;
                    }
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    if stderr.is_empty() {
                        last_failure =
                            Some(format!("{} exited with status {}", program, output.status));
                    } else {
                        last_failure = Some(stderr);
                    }
                }
                Ok(Err(e)) if e.kind() == ErrorKind::NotFound => {
                    // Try next candidate command.
                }
                Ok(Err(e)) => {
                    saw_spawnable_command = true;
                    last_failure = Some(format!("Failed to execute screenshot command: {e}"));
                }
                Err(_) => {
                    saw_spawnable_command = true;
                    last_failure = Some(format!(
                        "Screenshot timed out after {SCREENSHOT_TIMEOUT_SECS}s"
                    ));
                }
            }
        }

        if !saw_spawnable_command {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "No screenshot tool found. Install gnome-screenshot, scrot, or ImageMagick."
                        .into(),
                ),
            });
        }

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(
                last_failure
                    .unwrap_or_else(|| "Screenshot command failed for unknown reasons".into()),
            ),
        })
    }

    /// Read the screenshot file and return base64-encoded result.
    #[allow(clippy::incompatible_msrv)]
    async fn read_and_encode(output_path: &std::path::Path) -> anyhow::Result<ToolResult> {
        // Check file size before reading to prevent OOM on large screenshots
        const MAX_RAW_BYTES: u64 = 1_572_864; // ~1.5 MB (base64 expands ~33%)
        if let Ok(meta) = tokio::fs::metadata(output_path).await {
            if meta.len() > MAX_RAW_BYTES {
                return Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Screenshot saved to: {}\nSize: {} bytes (too large to base64-encode inline)",
                        output_path.display(),
                        meta.len(),
                    ),
                    error: None,
                });
            }
        }

        match tokio::fs::read(output_path).await {
            Ok(bytes) => {
                use base64::Engine;
                let size = bytes.len();
                let mut encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let truncated = if encoded.len() > MAX_BASE64_BYTES {
                    encoded.truncate(crate::util::floor_utf8_char_boundary(
                        &encoded,
                        MAX_BASE64_BYTES,
                    ));
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
                    output: output_msg,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: format!("Screenshot saved to: {}", output_path.display()),
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
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }
        self.capture(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use std::path::Path;

    #[cfg(unix)]
    fn symlink_file(src: &Path, dst: &Path) {
        std::os::unix::fs::symlink(src, dst).expect("symlink should be created");
    }

    #[cfg(windows)]
    fn symlink_file(src: &Path, dst: &Path) {
        std::os::windows::fs::symlink_file(src, dst).expect("symlink should be created");
    }

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
        let commands = ScreenshotTool::screenshot_commands("/tmp/test.png");
        assert!(!commands.is_empty());
        assert!(commands.iter().all(|cmd| !cmd.is_empty()));
    }

    #[test]
    fn screenshot_filename_sanitizes_dot_segments() {
        let fallback = "fallback.png";
        assert_eq!(
            ScreenshotTool::sanitize_output_filename("../outside.png", fallback),
            "outside.png"
        );
        assert_eq!(
            ScreenshotTool::sanitize_output_filename("..", fallback),
            fallback
        );
        assert_eq!(
            ScreenshotTool::sanitize_output_filename(".", fallback),
            fallback
        );
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
    fn screenshot_command_contains_output_path() {
        let commands = ScreenshotTool::screenshot_commands("/tmp/my_screenshot.png");
        assert!(!commands.is_empty());
        let joined = commands[0].join(" ");
        assert!(
            joined.contains("/tmp/my_screenshot.png"),
            "Command should contain the output path"
        );
    }

    #[tokio::test]
    async fn screenshot_blocks_symlink_output_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        tokio::fs::create_dir_all(&workspace)
            .await
            .expect("workspace should exist");

        let outside = temp.path().join("outside.png");
        tokio::fs::write(&outside, b"secret")
            .await
            .expect("outside fixture should be written");
        symlink_file(&outside, &workspace.join("screen.png"));

        let tool = ScreenshotTool::new(Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        }));

        let result = tool.resolve_output_path_for_write("screen.png").await;
        assert!(result.is_err(), "symlink output target must be rejected");
    }
}
