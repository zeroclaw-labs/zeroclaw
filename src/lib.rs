#![warn(clippy::all, clippy::pedantic)]
#![allow(
    clippy::assigning_clones,
    clippy::bool_to_int_with_if,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::cast_possible_wrap,
    clippy::doc_markdown,
    clippy::field_reassign_with_default,
    clippy::float_cmp,
    clippy::implicit_clone,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::manual_let_else,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::new_without_default,
    clippy::needless_pass_by_value,
    clippy::needless_raw_string_hashes,
    clippy::redundant_closure_for_method_calls,
    clippy::return_self_not_must_use,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::struct_field_names,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unnecessary_cast,
    clippy::unnecessary_lazy_evaluations,
    clippy::unnecessary_literal_bound,
    clippy::unnecessary_map_or,
    clippy::unused_self,
    clippy::cast_precision_loss,
    clippy::unnecessary_wraps,
    dead_code
)]

use clap::Subcommand;
use serde::{Deserialize, Serialize};

pub mod agent;
pub mod channels;
pub mod config;
pub mod cron;
pub mod daemon;
pub mod doctor;
pub mod gateway;
pub mod hardware;
pub mod health;
pub mod heartbeat;
pub mod identity;
pub mod integrations;
pub mod memory;
pub mod migration;
pub mod observability;
pub mod onboard;
pub mod providers;
pub mod runtime;
pub mod security;
pub mod service;
pub mod skills;
pub mod tools;
pub mod tunnel;
pub mod util;

pub use config::Config;

/// Service management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServiceCommands {
    /// Install daemon service unit for auto-start and restart
    Install,
    /// Start daemon service
    Start,
    /// Stop daemon service
    Stop,
    /// Check daemon service status
    Status,
    /// Uninstall daemon service unit
    Uninstall,
}

/// Channel management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelCommands {
    /// List all configured channels
    List,
    /// Start all configured channels (handled in main.rs for async)
    Start,
    /// Run health checks for configured channels (handled in main.rs for async)
    Doctor,
    /// Add a new channel configuration
    Add {
        /// Channel type (telegram, discord, slack, whatsapp, matrix, imessage, email)
        channel_type: String,
        /// Optional configuration as JSON
        config: String,
    },
    /// Remove a channel configuration
    Remove {
        /// Channel name to remove
        name: String,
    },
}

/// Skills management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillCommands {
    /// List all installed skills
    List,
    /// Install a new skill from a URL or local path
    Install {
        /// Source URL or local path
        source: String,
    },
    /// Remove an installed skill
    Remove {
        /// Skill name to remove
        name: String,
    },
}

/// Migration subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrateCommands {
    /// Import memory from an `OpenClaw` workspace into this `ZeroClaw` workspace
    Openclaw {
        /// Optional path to `OpenClaw` workspace (defaults to ~/.openclaw/workspace)
        #[arg(long)]
        source: Option<std::path::PathBuf>,

        /// Validate and preview migration without writing any data
        #[arg(long)]
        dry_run: bool,
    },
}

/// Cron subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CronCommands {
    /// List all scheduled tasks
    List,
    /// Add a new scheduled task
    Add {
        /// Cron expression
        expression: String,
        /// Command to run
        command: String,
    },
    /// Remove a scheduled task
    Remove {
        /// Task ID
        id: String,
    },
}

/// Integration subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IntegrationCommands {
    /// Show details about a specific integration
    Info {
        /// Integration name
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_commands_serde_roundtrip() {
        let command = ServiceCommands::Status;
        let json = serde_json::to_string(&command).unwrap();
        let parsed: ServiceCommands = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ServiceCommands::Status);
    }

    #[test]
    fn channel_commands_struct_variants_roundtrip() {
        let add = ChannelCommands::Add {
            channel_type: "telegram".into(),
            config: "{}".into(),
        };
        let remove = ChannelCommands::Remove {
            name: "main".into(),
        };

        let add_json = serde_json::to_string(&add).unwrap();
        let remove_json = serde_json::to_string(&remove).unwrap();

        let parsed_add: ChannelCommands = serde_json::from_str(&add_json).unwrap();
        let parsed_remove: ChannelCommands = serde_json::from_str(&remove_json).unwrap();

        assert_eq!(parsed_add, add);
        assert_eq!(parsed_remove, remove);
    }

    #[test]
    fn commands_with_payloads_roundtrip() {
        let skill = SkillCommands::Install {
            source: "https://example.com/skill".into(),
        };
        let migrate = MigrateCommands::Openclaw {
            source: Some(std::path::PathBuf::from("/tmp/openclaw")),
            dry_run: true,
        };
        let cron = CronCommands::Add {
            expression: "*/5 * * * *".into(),
            command: "echo hi".into(),
        };
        let integration = IntegrationCommands::Info {
            name: "Telegram".into(),
        };

        assert_eq!(
            serde_json::from_str::<SkillCommands>(&serde_json::to_string(&skill).unwrap()).unwrap(),
            skill
        );
        assert_eq!(
            serde_json::from_str::<MigrateCommands>(&serde_json::to_string(&migrate).unwrap())
                .unwrap(),
            migrate
        );
        assert_eq!(
            serde_json::from_str::<CronCommands>(&serde_json::to_string(&cron).unwrap()).unwrap(),
            cron
        );
        assert_eq!(
            serde_json::from_str::<IntegrationCommands>(
                &serde_json::to_string(&integration).unwrap()
            )
            .unwrap(),
            integration
        );
    }
}
