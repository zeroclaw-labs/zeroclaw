//! Configuration management commands.
//!
//! This module provides commands for inspecting and managing ZeroClaw configuration.

use clap::Subcommand;

/// Configuration management subcommands.
#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    /// Dump the full configuration JSON Schema to stdout
    Schema,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_commands_schema_exists() {
        // Verify that the Schema variant can be constructed
        let _ = ConfigCommands::Schema;
    }
}
