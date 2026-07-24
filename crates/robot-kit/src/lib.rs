//! # ZeroClaw Robot Kit

// TODO: Re-enable once all public items are documented
// #![warn(missing_docs)]
#![allow(missing_docs)]
#![warn(clippy::all)]

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
pub use safety::{SafeDrive, SafetyEvent, SafetyMonitor, SensorReading, preflight_check};

/// Crate version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Create all robot tools with default configuration
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
