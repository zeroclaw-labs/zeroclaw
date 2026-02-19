//! Emote Tool - LED expressions and sound effects
//!
//! Control LED matrix/strips for robot "expressions" and play sounds.
//! Makes the robot more engaging for kids!

use crate::config::RobotConfig;
use crate::traits::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Predefined LED expressions
#[derive(Debug, Clone, Copy)]
pub enum Expression {
    Happy,     // :)
    Sad,       // :(
    Surprised, // :O
    Thinking,  // :?
    Sleepy,    // -_-
    Excited,   // ^_^
    Love,      // <3 <3
    Angry,     // >:(
    Confused,  // @_@
    Wink,      // ;)
}

impl Expression {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "happy" | "smile" => Some(Self::Happy),
            "sad" | "frown" => Some(Self::Sad),
            "surprised" | "wow" => Some(Self::Surprised),
            "thinking" | "hmm" => Some(Self::Thinking),
            "sleepy" | "tired" => Some(Self::Sleepy),
            "excited" | "yay" => Some(Self::Excited),
            "love" | "heart" => Some(Self::Love),
            "angry" | "mad" => Some(Self::Angry),
            "confused" | "huh" => Some(Self::Confused),
            "wink" => Some(Self::Wink),
            _ => None,
        }
    }

    /// Get LED matrix pattern (8x8 example)
    /// Returns array of 64 RGB values
    fn pattern(&self) -> Vec<(u8, u8, u8)> {
        let black = (0, 0, 0);
        let white = (255, 255, 255);
        let yellow = (255, 255, 0);
        let red = (255, 0, 0);
        let blue = (0, 100, 255);
        let pink = (255, 100, 150);

        // 8x8 patterns (simplified representations)
        match self {
            Self::Happy => {
                // Simple smiley
                vec![
                    black, black, yellow, yellow, yellow, yellow, black, black, black, yellow,
                    black, black, black, black, yellow, black, yellow, black, white, black, black,
                    white, black, yellow, yellow, black, black, black, black, black, black, yellow,
                    yellow, black, white, black, black, white, black, yellow, yellow, black, black,
                    white, white, black, black, yellow, black, yellow, black, black, black, black,
                    yellow, black, black, black, yellow, yellow, yellow, yellow, black, black,
                ]
            }
            Self::Sad => {
                vec![
                    black, black, blue, blue, blue, blue, black, black, black, blue, black, black,
                    black, black, blue, black, blue, black, white, black, black, white, black,
                    blue, blue, black, black, black, black, black, black, blue, blue, black, black,
                    white, white, black, black, blue, blue, black, white, black, black, white,
                    black, blue, black, blue, black, black, black, black, blue, black, black,
                    black, blue, blue, blue, blue, black, black,
                ]
            }
            Self::Excited => {
                vec![
                    yellow, yellow, yellow, yellow, yellow, yellow, yellow, yellow, yellow, black,
                    black, yellow, yellow, black, black, yellow, yellow, black, white, yellow,
                    yellow, white, black, yellow, yellow, yellow, yellow, yellow, yellow, yellow,
                    yellow, yellow, yellow, black, black, black, black, black, black, yellow,
                    yellow, black, white, white, white, white, black, yellow, yellow, black, black,
                    black, black, black, black, yellow, yellow, yellow, yellow, yellow, yellow,
                    yellow, yellow, yellow,
                ]
            }
            Self::Love => {
                vec![
                    black, pink, pink, black, black, pink, pink, black, pink, pink, pink, pink,
                    pink, pink, pink, pink, pink, pink, pink, pink, pink, pink, pink, pink, pink,
                    pink, pink, pink, pink, pink, pink, pink, black, pink, pink, pink, pink, pink,
                    pink, black, black, black, pink, pink, pink, pink, black, black, black, black,
                    black, pink, pink, black, black, black, black, black, black, black, black,
                    black, black, black,
                ]
            }
            Self::Angry => {
                vec![
                    red, red, black, black, black, black, red, red, black, red, red, black, black,
                    red, red, black, black, black, red, black, black, red, black, black, black,
                    black, white, black, black, white, black, black, black, black, black, black,
                    black, black, black, black, black, black, white, white, white, white, black,
                    black, black, white, black, black, black, black, white, black, black, black,
                    black, black, black, black, black, black,
                ]
            }
            _ => {
                // Default neutral
                vec![white; 64]
            }
        }
    }
}

pub struct EmoteTool {
    #[allow(dead_code)]
    config: RobotConfig,
    sounds_dir: PathBuf,
}

impl EmoteTool {
    pub fn new(config: RobotConfig) -> Self {
        let sounds_dir = directories::UserDirs::new()
            .map(|d| d.home_dir().join(".zeroclaw/sounds"))
            .unwrap_or_else(|| PathBuf::from("/usr/local/share/zeroclaw/sounds"));

        Self { config, sounds_dir }
    }

    /// Set LED matrix expression
    async fn set_expression(&self, expr: Expression) -> Result<()> {
        let pattern = expr.pattern();

        // Convert to format for LED driver
        // In production, use rs_ws281x or similar
        let pattern_json = serde_json::to_string(&pattern)?;

        // Try to write to LED controller
        // Option 1: Write to FIFO/socket if LED daemon is running
        let led_fifo = PathBuf::from("/tmp/zeroclaw_led.fifo");
        if led_fifo.exists() {
            tokio::fs::write(&led_fifo, pattern_json).await?;
            return Ok(());
        }

        // Option 2: Shell out to LED control script
        let output = tokio::process::Command::new("zeroclaw-led")
            .args(["--pattern", &format!("{:?}", expr)])
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => Ok(()),
            _ => {
                tracing::info!("LED display: {:?} (hardware not connected)", expr);
                Ok(()) // Don't fail if LED hardware isn't available
            }
        }
    }

    /// Play emotion sound effect
    async fn play_emotion_sound(&self, emotion: &str) -> Result<()> {
        let sound_file = self.sounds_dir.join(format!("{}.wav", emotion));

        if !sound_file.exists() {
            tracing::debug!("No sound file for emotion: {}", emotion);
            return Ok(());
        }

        tokio::process::Command::new("aplay")
            .arg(sound_file)
            .output()
            .await?;

        Ok(())
    }

    /// Animate expression (e.g., blinking)
    async fn animate(&self, animation: &str) -> Result<()> {
        match animation {
            "blink" => {
                self.set_expression(Expression::Happy).await?;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                // "Closed eyes" - simplified
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                self.set_expression(Expression::Happy).await?;
            }
            "nod" => {
                // Would control servo if available
                tracing::info!("Animation: nod");
            }
            "shake" => {
                tracing::info!("Animation: shake");
            }
            "dance" => {
                // Cycle through expressions
                for expr in [
                    Expression::Happy,
                    Expression::Excited,
                    Expression::Love,
                    Expression::Happy,
                ] {
                    self.set_expression(expr).await?;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for EmoteTool {
    fn name(&self) -> &str {
        "emote"
    }

    fn description(&self) -> &str {
        "Express emotions through LED display and sounds. Use this to show the robot's \
         emotional state - happy when playing, sad when saying goodbye, excited for games, etc. \
         This makes interactions with kids more engaging!"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "enum": ["happy", "sad", "surprised", "thinking", "sleepy", "excited", "love", "angry", "confused", "wink"],
                    "description": "Facial expression to display on LED matrix"
                },
                "animation": {
                    "type": "string",
                    "enum": ["blink", "nod", "shake", "dance"],
                    "description": "Optional animation to perform"
                },
                "sound": {
                    "type": "boolean",
                    "description": "Play matching sound effect (default true)"
                },
                "duration": {
                    "type": "integer",
                    "description": "How long to hold expression in seconds (default 3)"
                }
            },
            "required": ["expression"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let expression_str = args["expression"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'expression' parameter"))?;

        let expression = Expression::from_str(expression_str)
            .ok_or_else(|| anyhow::anyhow!("Unknown expression: {}", expression_str))?;

        let play_sound = args["sound"].as_bool().unwrap_or(true);
        let duration = args["duration"].as_u64().unwrap_or(3);

        // Set expression
        self.set_expression(expression).await?;

        // Play sound if enabled
        if play_sound {
            let _ = self.play_emotion_sound(expression_str).await;
        }

        // Run animation if specified
        if let Some(animation) = args["animation"].as_str() {
            self.animate(animation).await?;
        }

        // Hold expression
        if duration > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(duration.min(10))).await;
        }

        Ok(ToolResult {
            success: true,
            output: format!("Expressing: {} for {}s", expression_str, duration),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emote_tool_name() {
        let tool = EmoteTool::new(RobotConfig::default());
        assert_eq!(tool.name(), "emote");
    }

    #[test]
    fn expression_parsing() {
        assert!(Expression::from_str("happy").is_some());
        assert!(Expression::from_str("EXCITED").is_some());
        assert!(Expression::from_str("unknown").is_none());
    }

    #[test]
    fn expression_pattern_size() {
        let expr = Expression::Happy;
        assert_eq!(expr.pattern().len(), 64); // 8x8
    }

    #[tokio::test]
    async fn emote_happy() {
        let tool = EmoteTool::new(RobotConfig::default());
        let result = tool
            .execute(json!({
                "expression": "happy",
                "duration": 0
            }))
            .await
            .unwrap();
        assert!(result.success);
    }
}
