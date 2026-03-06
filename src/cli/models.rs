//! Model management commands.
//!
//! This module provides commands for managing provider model catalogs and caching.

use clap::Subcommand;

/// Model management subcommands.
#[derive(Subcommand, Debug)]
pub enum ModelCommands {
    /// Refresh and cache provider models
    Refresh {
        /// Provider name (defaults to configured default provider)
        #[arg(long)]
        provider: Option<String>,

        /// Force live refresh and ignore fresh cache
        #[arg(long)]
        force: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_commands_refresh_exists() {
        // Verify that Refresh variant can be constructed with all optional fields
        let _ = ModelCommands::Refresh {
            provider: None,
            force: false,
        };

        let _ = ModelCommands::Refresh {
            provider: Some("openrouter".to_string()),
            force: true,
        };
    }
}
