//! Memory management commands.
//!
//! This module provides commands for inspecting and managing ZeroClaw memory storage.

use clap::Subcommand;

/// Memory management subcommands.
#[derive(Subcommand, Debug)]
pub enum MemoryCommands {
    /// List memory entries with optional filters
    List {
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value = "50")]
        limit: usize,
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// Get a specific memory entry by key
    Get { key: String },
    /// Show memory backend statistics and health
    Stats,
    /// Clear memories by category, by key, or clear all
    Clear {
        /// Delete a single entry by key (supports prefix match)
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        category: Option<String>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_commands_list_exists() {
        let _ = MemoryCommands::List {
            category: None,
            session: None,
            limit: 50,
            offset: 0,
        };

        let _ = MemoryCommands::List {
            category: Some("core".to_string()),
            session: Some("session-123".to_string()),
            limit: 100,
            offset: 10,
        };
    }

    #[test]
    fn memory_commands_get_exists() {
        let _ = MemoryCommands::Get {
            key: "test_key".to_string(),
        };
    }

    #[test]
    fn memory_commands_stats_exists() {
        let _ = MemoryCommands::Stats;
    }

    #[test]
    fn memory_commands_clear_exists() {
        let _ = MemoryCommands::Clear {
            key: None,
            category: None,
            yes: false,
        };

        let _ = MemoryCommands::Clear {
            key: Some("prefix".to_string()),
            category: Some("daily".to_string()),
            yes: true,
        };
    }
}
