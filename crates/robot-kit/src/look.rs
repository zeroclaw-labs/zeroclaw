//! Look Tool - Camera capture + vision model description
//!
//! Captures an image from the camera and optionally describes it
//! using a local vision model (LLaVA, Moondream) via Ollama.

use crate::config::RobotConfig;
use crate::traits::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct LookTool {
    config: RobotConfig,
    capture_dir: PathBuf,
}

impl LookTool {
    pub fn new(config: RobotConfig) -> Self {
        let capture_dir = directories::UserDirs::new()
            .map(|d| d.home_dir().join(".zeroclaw/captures"))
            .unwrap_or_else(|| PathBuf::from("/tmp/zeroclaw_captures"));

        // Ensure capture directory exists
        let _ = std::fs::create_dir_all(&capture_dir);

        Self {
            config,
            capture_dir,
        }
    }

    /// Capture image using ffmpeg (works with most cameras)
    async fn capture_image(&self) -> Result<PathBuf> {
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let filename = self.capture_dir.join(format!("capture_{}.jpg", timestamp));

        let device = &self.config.camera.device;
        let width = self.config.camera.width;
        let height = self.config.camera.height;

        // Use ffmpeg for broad camera compatibility
        let output = tokio::process::Command::new("ffmpeg")
            .args([
                "-f",
                "v4l2",
                "-video_size",
                &format!("{}x{}", width, height),
                "-i",
                device,
                "-frames:v",
                "1",
                "-y", // Overwrite
                filename.to_str().unwrap(),
            ])
            .output()
            .await?;

        if !output.status.success() {
            // Fallback: try fswebcam (simpler, often works on Pi)
            let fallback = tokio::process::Command::new("fswebcam")
                .args([
                    "-r",
                    &format!("{}x{}", width, height),
                    "--no-banner",
                    "-d",
                    device,
                    filename.to_str().unwrap(),
                ])
                .output()
                .await?;

            if !fallback.status.success() {
                anyhow::bail!(
                    "Camera capture failed. Tried ffmpeg and fswebcam.\n\
                     ffmpeg: {}\n\
                     fswebcam: {}",
                    String::from_utf8_lossy(&output.stderr),
                    String::from_utf8_lossy(&fallback.stderr)
                );
            }
        }

        Ok(filename)
    }

    /// Describe image using vision model via Ollama
    async fn describe_image(&self, image_path: &PathBuf, prompt: &str) -> Result<String> {
        let model = &self.config.camera.vision_model;
        if model == "none" {
            return Ok("Vision model disabled. Image captured only.".to_string());
        }

        // Read image as base64
        let image_bytes = tokio::fs::read(image_path).await?;
        let base64_image =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &image_bytes);

        // Call Ollama with image
        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/api/generate", self.config.camera.ollama_url))
            .json(&json!({
                "model": model,
                "prompt": prompt,
                "images": [base64_image],
                "stream": false
            }))
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Ollama vision request failed: {}", response.status());
        }

        let result: Value = response.json().await?;
        let description = result["response"]
            .as_str()
            .unwrap_or("No description generated")
            .to_string();

        Ok(description)
    }
}

#[async_trait]
impl Tool for LookTool {
    fn name(&self) -> &str {
        "look"
    }

    fn description(&self) -> &str {
        "Capture an image from the robot's camera and optionally describe what is seen. \
         Use this to observe the environment, find objects, or identify people."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["capture", "describe", "find"],
                    "description": "capture=just take photo, describe=photo+AI description, find=look for specific thing"
                },
                "prompt": {
                    "type": "string",
                    "description": "For 'describe': what to focus on. For 'find': what to look for."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        // Capture image
        let image_path = match self.capture_image().await {
            Ok(path) => path,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Camera capture failed: {e}")),
                });
            }
        };

        match action {
            "capture" => Ok(ToolResult {
                success: true,
                output: format!("Image captured: {}", image_path.display()),
                error: None,
            }),
            "describe" => {
                let prompt = args["prompt"]
                    .as_str()
                    .unwrap_or("Describe what you see in this image. Be specific about people, objects, and the environment.");

                match self.describe_image(&image_path, prompt).await {
                    Ok(description) => Ok(ToolResult {
                        success: true,
                        output: format!("I see: {}", description),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: format!(
                            "Image captured at {} but description failed",
                            image_path.display()
                        ),
                        error: Some(e.to_string()),
                    }),
                }
            }
            "find" => {
                let target = args["prompt"].as_str().ok_or_else(|| {
                    anyhow::anyhow!("'find' action requires 'prompt' specifying what to find")
                })?;

                let prompt = format!(
                    "Look at this image and determine: Is there a {} visible? \
                     If yes, describe where it is (left, right, center, near, far). \
                     If no, say 'Not found' and describe what you do see.",
                    target
                );

                match self.describe_image(&image_path, &prompt).await {
                    Ok(description) => Ok(ToolResult {
                        success: true,
                        output: description,
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    }),
                }
            }
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {action}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_tool_name() {
        let tool = LookTool::new(RobotConfig::default());
        assert_eq!(tool.name(), "look");
    }

    #[test]
    fn look_tool_schema() {
        let tool = LookTool::new(RobotConfig::default());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
    }
}
