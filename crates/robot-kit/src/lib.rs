//! # ZeroClaw Robot Kit
//!
//! A standalone robotics toolkit that integrates with ZeroClaw for AI-powered robots.
//!
//! ## Features
//!
//! - **Drive**: Omni-directional motor control (ROS2, serial, GPIO, mock)
//! - **Look**: Camera capture + vision model description (Ollama)
//! - **Listen**: Speech-to-text via Whisper.cpp
//! - **Speak**: Text-to-speech via Piper TTS
//! - **Sense**: LIDAR, motion sensors, ultrasonic distance
//! - **Emote**: LED matrix expressions and sound effects
//! - **Safety**: Independent safety monitor (collision avoidance, E-stop, watchdog)
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │  ZeroClaw AI Brain (or any controller)                  │
//! │  "Move forward, find the ball, tell me what you see"    │
//! └─────────────────────┬───────────────────────────────────┘
//!                       │ Tool calls
//!                       ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │  zeroclaw-robot-kit                                     │
//! │  ┌─────────┐ ┌──────┐ ┌────────┐ ┌───────┐ ┌───────┐   │
//! │  │ drive   │ │ look │ │ listen │ │ speak │ │ sense │   │
//! │  └────┬────┘ └──┬───┘ └───┬────┘ └───┬───┘ └───┬───┘   │
//! │       │         │         │          │         │        │
//! │  ┌────┴─────────┴─────────┴──────────┴─────────┴────┐  │
//! │  │              SafetyMonitor (parallel)             │  │
//! │  │  • Pre-move obstacle check                        │  │
//! │  │  • Proximity-based speed limiting                 │  │
//! │  │  • Bump sensor response                           │  │
//! │  │  • Watchdog auto-stop                             │  │
//! │  │  • Hardware E-stop override                       │  │
//! │  └──────────────────────────────────────────────────┘  │
//! └─────────────────────────────────────────────────────────┘
//!                       │
//!                       ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │  Hardware: Motors, Camera, Mic, Speaker, LIDAR, LEDs    │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use zeroclaw_robot_kit::{RobotConfig, DriveTool, SafetyMonitor, SafeDrive};
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() {
//!     // Load configuration
//!     let config = RobotConfig::default();
//!
//!     // Create safety monitor
//!     let (safety, _rx) = SafetyMonitor::new(config.safety.clone());
//!     let safety = Arc::new(safety);
//!
//!     // Wrap drive with safety
//!     let drive = Arc::new(DriveTool::new(config.clone()));
//!     let safe_drive = SafeDrive::new(drive, safety.clone());
//!
//!     // Use tools...
//!     let result = safe_drive.execute(serde_json::json!({
//!         "action": "forward",
//!         "distance": 1.0
//!     })).await;
//! }
//! ```
//!
//! ## Standalone Usage
//!
//! This crate can be used independently of ZeroClaw. It defines its own
//! `Tool` trait that is compatible with ZeroClaw's but doesn't require it.
//!
//! ## Safety
//!
//! **The AI can REQUEST movement, but SafetyMonitor ALLOWS it.**
//!
//! The safety system runs as an independent task and can override any
//! AI decision. This prevents collisions even if the LLM hallucinates.

// TODO: Re-enable once all public items are documented
// #![warn(missing_docs)]
#![allow(missing_docs)]
#![warn(clippy::all)]
#![forbid(unsafe_code)]

pub mod config;
pub mod traits;

pub mod drive;
pub mod emote;
pub mod listen;
pub mod look;
pub mod sense;
pub mod speak;

#[cfg(feature = "safety")]
pub mod safety;

#[cfg(test)]
mod tests;

// Re-exports for convenience
pub use config::RobotConfig;
pub use traits::{Tool, ToolResult, ToolSpec};

pub use drive::DriveTool;
pub use emote::EmoteTool;
pub use listen::ListenTool;
pub use look::LookTool;
pub use sense::SenseTool;
pub use speak::SpeakTool;

#[cfg(feature = "safety")]
pub use safety::{preflight_check, SafeDrive, SafetyEvent, SafetyMonitor, SensorReading};

/// Crate version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Create all robot tools with default configuration
///
/// Returns a Vec of boxed tools ready for use with an agent.
pub fn create_tools(config: &RobotConfig) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(DriveTool::new(config.clone())),
        Box::new(LookTool::new(config.clone())),
        Box::new(ListenTool::new(config.clone())),
        Box::new(SpeakTool::new(config.clone())),
        Box::new(SenseTool::new(config.clone())),
        Box::new(EmoteTool::new(config.clone())),
    ]
}

/// Create all robot tools with safety wrapper on drive
#[cfg(feature = "safety")]
pub fn create_safe_tools(
    config: &RobotConfig,
    safety: std::sync::Arc<SafetyMonitor>,
) -> Vec<Box<dyn Tool>> {
    let drive = std::sync::Arc::new(DriveTool::new(config.clone()));
    let safe_drive = SafeDrive::new(drive, safety);

    vec![
        Box::new(safe_drive),
        Box::new(LookTool::new(config.clone())),
        Box::new(ListenTool::new(config.clone())),
        Box::new(SpeakTool::new(config.clone())),
        Box::new(SenseTool::new(config.clone())),
        Box::new(EmoteTool::new(config.clone())),
    ]
}
