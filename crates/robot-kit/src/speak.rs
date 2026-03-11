//! Speak Tool - Text-to-speech via Piper
//!
//! Converts text to speech using Piper TTS (fast, offline, runs on Pi).
//! Plays audio through the speaker.

use crate::config::RobotConfig;
use crate::traits::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct SpeakTool {
    config: RobotConfig,
    audio_dir: PathBuf,
}

impl SpeakTool {
    pub fn new(config: RobotConfig) -> Self {
        let audio_dir = directories::UserDirs::new()
            .map(|d| d.home_dir().join(".zeroclaw/tts_cache"))
            .unwrap_or_else(|| PathBuf::from("/tmp/zeroclaw_tts"));

        let _ = std::fs::create_dir_all(&audio_dir);

        Self { config, audio_dir }
    }

    /// Generate speech using Piper and play it
    async fn speak(&self, text: &str, emotion: &str) -> Result<()> {
        let piper_path = &self.config.audio.piper_path;
        let voice = &self.config.audio.piper_voice;
        let speaker_device = &self.config.audio.speaker_device;

        // Model path
        let model_path = directories::UserDirs::new()
            .map(|d| {
                d.home_dir()
                    .join(format!(".zeroclaw/models/piper/{}.onnx", voice))
            })
            .unwrap_or_else(|| PathBuf::from(format!("/usr/local/share/piper/{}.onnx", voice)));

        // Adjust text based on emotion (simple SSML-like modifications)
        let processed_text = match emotion {
            "excited" => format!("{}!", text.trim_end_matches('.')),
            "sad" => text.to_string(), // Piper doesn't support prosody, but we keep the hook
            "whisper" => text.to_string(),
            _ => text.to_string(),
        };

        // Generate WAV file
        let output_path = self.audio_dir.join("speech.wav");

        // Pipe text to piper, output to WAV
        let mut piper = tokio::process::Command::new(piper_path)
            .args([
                "--model",
                model_path.to_str().unwrap(),
                "--output_file",
                output_path.to_str().unwrap(),
            ])
            .stdin(std::process::Stdio::piped())
            .spawn()?;

        // Write text to stdin
        if let Some(mut stdin) = piper.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(processed_text.as_bytes()).await?;
        }

        let status = piper.wait().await?;
        if !status.success() {
            anyhow::bail!("Piper TTS failed");
        }

        // Play audio using aplay
        let play_result = tokio::process::Command::new("aplay")
            .args(["-D", speaker_device, output_path.to_str().unwrap()])
            .output()
            .await?;

        if !play_result.status.success() {
            // Fallback: try paplay (PulseAudio)
            let fallback = tokio::process::Command::new("paplay")
                .arg(output_path.to_str().unwrap())
                .output()
                .await?;

            if !fallback.status.success() {
                anyhow::bail!(
                    "Audio playback failed. Tried aplay and paplay.\n{}",
                    String::from_utf8_lossy(&play_result.stderr)
                );
            }
        }

        Ok(())
    }

    /// Play a sound effect
    async fn play_sound(&self, sound: &str) -> Result<()> {
        let sounds_dir = directories::UserDirs::new()
            .map(|d| d.home_dir().join(".zeroclaw/sounds"))
            .unwrap_or_else(|| PathBuf::from("/usr/local/share/zeroclaw/sounds"));

        let sound_file = sounds_dir.join(format!("{}.wav", sound));

        if !sound_file.exists() {
            anyhow::bail!("Sound file not found: {}", sound_file.display());
        }

        let speaker_device = &self.config.audio.speaker_device;
        let output = tokio::process::Command::new("aplay")
            .args(["-D", speaker_device, sound_file.to_str().unwrap()])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!("Sound playback failed");
        }

        Ok(())
    }
}

#[async_trait]
impl Tool for SpeakTool {
    fn name(&self) -> &str {
        "speak"
    }

    fn description(&self) -> &str {
        "Speak text out loud using text-to-speech. The robot will say the given text \
         through its speaker. Can also play sound effects like 'beep', 'chime', 'laugh'."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to speak out loud"
                },
                "emotion": {
                    "type": "string",
                    "enum": ["neutral", "excited", "sad", "whisper"],
                    "description": "Emotional tone. Default 'neutral'."
                },
                "sound": {
                    "type": "string",
                    "description": "Play a sound effect instead of speaking (e.g., 'beep', 'chime', 'laugh', 'alert')"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        // Check if playing a sound effect
        if let Some(sound) = args["sound"].as_str() {
            return match self.play_sound(sound).await {
                Ok(()) => Ok(ToolResult {
                    success: true,
                    output: format!("Played sound: {}", sound),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Sound playback failed: {e}")),
                }),
            };
        }

        // Speak text
        let text = args["text"].as_str().ok_or_else(|| {
            anyhow::anyhow!("Missing 'text' parameter (or use 'sound' for effects)")
        })?;

        if text.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Cannot speak empty text".to_string()),
            });
        }

        // Limit text length for safety
        if text.len() > 1000 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Text too long (max 1000 characters)".to_string()),
            });
        }

        let emotion = args["emotion"].as_str().unwrap_or("neutral");

        match self.speak(text, emotion).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Said: \"{}\"", text),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Speech failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speak_tool_name() {
        let tool = SpeakTool::new(RobotConfig::default());
        assert_eq!(tool.name(), "speak");
    }

    #[test]
    fn speak_tool_schema() {
        let tool = SpeakTool::new(RobotConfig::default());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["text"].is_object());
        assert!(schema["properties"]["emotion"].is_object());
    }
}
