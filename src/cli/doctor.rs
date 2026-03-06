//! Diagnostic commands for ZeroClaw.
//!
//! This module provides commands for probing system health, model catalogs,
//! and runtime trace events.

use clap::Subcommand;

/// Diagnostic subcommands.
#[derive(Subcommand, Debug)]
pub enum DoctorCommands {
    /// Probe model catalogs across providers and report availability
    Models {
        /// Probe a specific provider only (default: all known providers)
        #[arg(long)]
        provider: Option<String>,

        /// Prefer cached catalogs when available (skip forced live refresh)
        #[arg(long)]
        use_cache: bool,
    },
    /// Query runtime trace events (tool diagnostics and model replies)
    Traces {
        /// Show a specific trace event by id
        #[arg(long)]
        id: Option<String>,
        /// Filter list output by event type
        #[arg(long)]
        event: Option<String>,
        /// Case-insensitive text match across message/payload
        #[arg(long)]
        contains: Option<String>,
        /// Maximum number of events to display
        #[arg(long, default_value = "20")]
        limit: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_commands_models_exists() {
        let _ = DoctorCommands::Models {
            provider: None,
            use_cache: false,
        };

        let _ = DoctorCommands::Models {
            provider: Some("anthropic".to_string()),
            use_cache: true,
        };
    }

    #[test]
    fn doctor_commands_traces_exists() {
        let _ = DoctorCommands::Traces {
            id: None,
            event: None,
            contains: None,
            limit: 20,
        };

        let _ = DoctorCommands::Traces {
            id: Some("123".to_string()),
            event: Some("tool_call".to_string()),
            contains: Some("error".to_string()),
            limit: 50,
        };
    }
}
