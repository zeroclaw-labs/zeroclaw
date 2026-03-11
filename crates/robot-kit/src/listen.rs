//! Listen Tool - Speech-to-text via Whisper.cpp
//!
//! Records audio from microphone and transcribes using local Whisper model.
//! Designed for offline operation on Raspberry Pi.

use crate::config::RobotConfig;
use crate::traits::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct ListenTool {
    config: RobotConfig,
    recordings_dir: PathBuf,
}

impl ListenTool {
    pub fn new(config: RobotConfig) -> Self {
        let recordings_dir = directories::UserDirs::new()
            .map(|d| d.home_dir().join(".zeroclaw/recordings"))
            .unwrap_or_else(|| PathBuf::from("/tmp/zeroclaw_recordings"));

        let _ = std::fs::create_dir_all(&recordings_dir);

        Self {
            config,
            recordings_dir,
        }
    }

    /// Record audio using arecord (ALSA)
    async fn record_audio(&self, duration_secs: u64) -> Result<PathBuf> {
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let filename = self
            .recordings_dir
            .join(format!("recording_{}.wav", timestamp));

        let device = &self.config.audio.mic_device;

        // Record using arecord (standard on Linux/Pi)
        let output = tokio::process::Command::new("arecord")
            .args([
                "-D",
                device,
                "-f",
                "S16_LE", // 16-bit signed little-endian
                "-r",
                "16000", // 16kHz (Whisper expects this)
                "-c",
                "1", // Mono
                "-d",
                &duration_secs.to_string(),
                filename.to_str().unwrap(),
            ])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!(
                "Audio recording failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(filename)
    }

    /// Transcribe audio using whisper.cpp
    async fn transcribe(&self, audio_path: &Path) -> Result<String> {
        let whisper_path = &self.config.audio.whisper_path;
        let model = &self.config.audio.whisper_model;

        // whisper.cpp model path (typically in ~/.zeroclaw/models/)
        let model_path = directories::UserDirs::new()
            .map(|d| {
                d.home_dir()
                    .join(format!(".zeroclaw/models/ggml-{}.bin", model))
            })
            .unwrap_or_else(|| {
                PathBuf::from(format!("/usr/local/share/whisper/ggml-{}.bin", model))
            });

        // Run whisper.cpp
        let output = tokio::process::Command::new(whisper_path)
            .args([
                "-m",
                model_path.to_str().unwrap(),
                "-f",
                audio_path.to_str().unwrap(),
                "--no-timestamps",
                "-otxt", // Output as text
            ])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!(
                "Whisper transcription failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // whisper.cpp outputs to <input>.txt
        let txt_path = audio_path.with_extension("wav.txt");
        let transcript = tokio::fs::read_to_string(&txt_path)
            .await
            .unwrap_or_else(|_| String::from_utf8_lossy(&output.stdout).to_string());

        // Clean up temp files
        let _ = tokio::fs::remove_file(&txt_path).await;

        Ok(transcript.trim().to_string())
    }
}

#[async_trait]
impl Tool for ListenTool {
    fn name(&self) -> &str {
        "listen"
    }

    fn description(&self) -> &str {
        "Listen for speech and transcribe it to text. Records from the microphone \
         for the specified duration, then converts speech to text using Whisper."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "duration": {
                    "type": "integer",
                    "description": "Recording duration in seconds. Default 5, max 30.",
                    "minimum": 1,
                    "maximum": 30
                },
                "prompt": {
                    "type": "string",
                    "description": "Optional context hint for transcription (e.g., 'The speaker is a child')"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let duration = args["duration"].as_u64().unwrap_or(5).clamp(1, 30);

        // Record audio
        tracing::info!("Recording audio for {} seconds...", duration);
        let audio_path = match self.record_audio(duration).await {
            Ok(path) => path,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Recording failed: {e}")),
                });
            }
        };

        // Transcribe
        tracing::info!("Transcribing audio...");
        match self.transcribe(&audio_path).await {
            Ok(transcript) => {
                // Clean up audio file
                let _ = tokio::fs::remove_file(&audio_path).await;

                if transcript.is_empty() {
                    Ok(ToolResult {
                        success: true,
                        output: "(silence - no speech detected)".to_string(),
                        error: None,
                    })
                } else {
                    Ok(ToolResult {
                        success: true,
                        output: format!("I heard: \"{}\"", transcript),
                        error: None,
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Transcription failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_tool_name() {
        let tool = ListenTool::new(RobotConfig::default());
        assert_eq!(tool.name(), "listen");
    }

    #[test]
    fn listen_tool_schema() {
        let tool = ListenTool::new(RobotConfig::default());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["duration"].is_object());
    }
}
